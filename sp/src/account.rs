//! Main Account orchestrator for Silent Payment wallets.
//!
//! The `Account` struct ties together all components of a Silent Payment wallet:
//! - SpClient for key management and address derivation
//! - BlindbitBackend for blockchain data access
//! - Stores for coins, transactions, labels, and scan state
//! - Background scanning thread for continuous blockchain monitoring

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use bitcoin::absolute::Height;
use bitcoin::hashes::Hash;
use bitcoin::{Amount, BlockHash, Network, OutPoint, Transaction, Txid};
use silentpayments::SilentPaymentAddress;

use backend_blindbit_native_non_async::{BlindbitBackend, UreqClient};
use spdk_core::account::SpAccount;
use spdk_core::{
    bip39, FeeRate, OwnedOutput, Recipient, RecipientAddress, SilentPaymentUnsignedTransaction,
    SpClient, SpScanner, Updater,
};

use crate::{
    CoinState, Config, LabelKey, ScanState, SpCoinEntry, SpCoinStore, SpLabelStore, SpTxEntry,
    SpTxStore,
};

//=============================================================================
// Type Aliases
//=============================================================================

/// Type alias for the tuple of stores returned by create_or_load_stores.
type Stores = (
    Arc<Mutex<SpCoinStore>>,
    Arc<Mutex<SpLabelStore>>,
    Arc<Mutex<SpTxStore>>,
    Arc<Mutex<ScanState>>,
);

//=============================================================================
// AccountError
//=============================================================================

/// Errors that can occur in Account operations.
#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    /// Configuration is invalid
    #[error("config invalid: {0}")]
    Config(String),
    /// Scan operation failed
    #[error("scan failed: {0}")]
    Scan(String),
    /// Network/backend communication error
    #[error("network error: {0}")]
    Network(String),
    /// Signing requested but no keys available
    #[error("signing failed: no keys")]
    NoKeys,
    /// Transaction broadcast failed
    #[error("broadcast failed: {0}")]
    Broadcast(String),
    /// No broadcast URL configured
    #[error("no broadcast url configured")]
    NoBroadcastUrl,
    /// Scanner thread is already running
    #[error("scanner already running")]
    ScannerAlreadyRunning,
    /// Transaction building failed
    #[error("transaction error: {0}")]
    Transaction(String),
}

//=============================================================================
// Notification
//=============================================================================

/// Notifications sent by the Account to signal events.
#[derive(Debug, Clone)]
pub enum Notification {
    /// Scan has started
    ScanStarted,
    /// Scan progress update
    ScanProgress {
        /// Current block being scanned
        current: u32,
        /// End block of this scan
        end: u32,
    },
    /// Scan completed successfully
    ScanCompleted,
    /// Scan encountered an error
    ScanError {
        /// Error message
        message: String,
        /// Number of retries attempted before giving up
        retries_attempted: u32,
    },
    /// A new output was found
    NewOutput(OutPoint),
    /// An output was spent
    OutputSpent(OutPoint),
    /// Scanner has stopped
    Stopped,
}

//=============================================================================
// PaymentType
//=============================================================================

/// Type of payment (for UI display).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaymentType {
    /// Received payment
    Receive,
    /// Sent payment
    Send,
}

//=============================================================================
// Payment
//=============================================================================

/// A payment record for UI display.
#[derive(Debug, Clone)]
pub struct Payment {
    /// Transaction ID as string
    pub txid: String,
    /// Type of payment
    pub payment_type: PaymentType,
    /// Amount in satoshis
    pub amount: u64,
    /// User-assigned label
    pub label: String,
    /// Confirmation height if confirmed
    pub height: Option<u32>,
}

//=============================================================================
// Account
//=============================================================================

/// Main orchestrator for a Silent Payment wallet account.
///
/// Ties together the SpClient, backend, and all stores. Provides methods for
/// balance queries, scanning, transaction building, and persistence.
pub struct Account {
    /// Silent payment client (keys and address derivation)
    client: SpClient,
    /// Blindbit backend for blockchain data
    backend: BlindbitBackend<UreqClient>,
    /// Store for owned outputs
    coin_store: Arc<Mutex<SpCoinStore>>,
    /// Store for user-facing labels
    label_store: Arc<Mutex<SpLabelStore>>,
    /// Store for transactions
    tx_store: Arc<Mutex<SpTxStore>>,
    /// Scan state tracking
    scan_state: Arc<Mutex<ScanState>>,
    /// Account configuration
    config: Config,
    /// Channel sender for notifications
    sender: mpsc::Sender<Notification>,
    /// Channel receiver for notifications (take once)
    receiver: Option<mpsc::Receiver<Notification>>,
    /// Handle to the background scanner thread
    scanner_handle: Option<JoinHandle<()>>,
    /// Flag to signal scanner to stop
    scanner_stop: Arc<AtomicBool>,
}

impl Account {
    //-------------------------------------------------------------------------
    // Constructors
    //-------------------------------------------------------------------------

    /// Create a new Account from configuration.
    ///
    /// Validates the config, creates the SpClient, initializes or loads stores,
    /// and prepares the notification channel.
    ///
    /// # Errors
    ///
    /// Returns `AccountError::Config` if:
    /// - Neither mnemonic nor scan_sk is provided
    /// - blindbit_url is empty
    pub fn new(config: Config) -> Result<Self, AccountError> {
        // Validate config
        if config.mnemonic.is_none() && config.scan_sk.is_none() {
            return Err(AccountError::Config(
                "either mnemonic or scan_sk must be provided".to_string(),
            ));
        }
        if config.blindbit_url.is_empty() {
            return Err(AccountError::Config("blindbit_url is required".to_string()));
        }

        // Create SpClient
        let client = Self::create_sp_client(&config)?;

        // Create backend
        let backend = Self::create_backend(&config)?;

        // Create notification channel
        let (sender, receiver) = mpsc::channel();

        // Create/load stores
        let (coin_store, label_store, tx_store, scan_state) = Self::create_or_load_stores(&config);

        Ok(Account {
            client,
            backend,
            coin_store,
            label_store,
            tx_store,
            scan_state,
            config,
            sender,
            receiver: Some(receiver),
            scanner_handle: None,
            scanner_stop: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Create account from mnemonic (convenience constructor).
    ///
    /// This is a convenience wrapper around `Config::new()` + `Account::new()`.
    ///
    /// # Arguments
    /// * `account_name` - Name of the account
    /// * `network` - Bitcoin network (mainnet, testnet, signet)
    /// * `mnemonic` - BIP39 mnemonic phrase
    /// * `blindbit_url` - URL of the Blindbit backend
    /// * `data_dir` - Directory for storing account data
    pub fn from_mnemonic(
        account_name: String,
        network: Network,
        mnemonic: &str,
        blindbit_url: String,
        data_dir: std::path::PathBuf,
    ) -> Result<Self, AccountError> {
        let config = Config::new(
            account_name,
            network,
            mnemonic.to_string(),
            blindbit_url,
            data_dir,
        );
        Self::new(config)
    }

    /// Load an existing Account from configuration.
    ///
    /// Same as `new()` but prioritizes loading existing data from files.
    pub fn load(config: Config) -> Result<Self, AccountError> {
        // For now, load behaves the same as new since stores
        // already prioritize loading from files when persist=true
        Self::new(config)
    }

    /// Create SpClient from config.
    fn create_sp_client(config: &Config) -> Result<SpClient, AccountError> {
        if let Some(ref mnemonic) = config.mnemonic {
            let mnemonic = bip39::Mnemonic::parse(mnemonic)
                .map_err(|e| AccountError::Config(format!("invalid mnemonic: {}", e)))?;
            SpClient::new_from_mnemonic(mnemonic, config.network)
                .map_err(|e| AccountError::Config(format!("failed to create SpClient: {}", e)))
        } else if let Some(ref scan_sk_hex) = config.scan_sk {
            // Create from raw keys
            let scan_sk_bytes = hex::decode(scan_sk_hex)
                .map_err(|e| AccountError::Config(format!("invalid scan_sk hex: {}", e)))?;
            let scan_sk = bitcoin::secp256k1::SecretKey::from_slice(&scan_sk_bytes)
                .map_err(|e| AccountError::Config(format!("invalid scan_sk: {}", e)))?;

            let spend_key = if let Some(ref spend_key_hex) = config.spend_key {
                let spend_key_bytes = hex::decode(spend_key_hex)
                    .map_err(|e| AccountError::Config(format!("invalid spend_key hex: {}", e)))?;

                if spend_key_bytes.len() == 32 {
                    // Secret key
                    let sk = bitcoin::secp256k1::SecretKey::from_slice(&spend_key_bytes)
                        .map_err(|e| AccountError::Config(format!("invalid spend_key: {}", e)))?;
                    spdk_core::SpendKey::Secret(sk)
                } else if spend_key_bytes.len() == 33 {
                    // Public key
                    let pk = bitcoin::secp256k1::PublicKey::from_slice(&spend_key_bytes)
                        .map_err(|e| AccountError::Config(format!("invalid spend_key: {}", e)))?;
                    spdk_core::SpendKey::Public(pk)
                } else {
                    return Err(AccountError::Config(
                        "spend_key must be 32 or 33 bytes".to_string(),
                    ));
                }
            } else {
                return Err(AccountError::Config(
                    "spend_key required when using scan_sk".to_string(),
                ));
            };

            SpClient::new(scan_sk, spend_key, config.network)
                .map_err(|e| AccountError::Config(format!("failed to create SpClient: {}", e)))
        } else {
            Err(AccountError::Config(
                "either mnemonic or scan_sk must be provided".to_string(),
            ))
        }
    }

    /// Create BlindbitBackend from config.
    fn create_backend(config: &Config) -> Result<BlindbitBackend<UreqClient>, AccountError> {
        let http_client = UreqClient::new();
        BlindbitBackend::new(config.blindbit_url.clone(), http_client)
            .map_err(|e| AccountError::Network(format!("failed to create backend: {}", e)))
    }

    /// Create or load stores based on config.persist.
    fn create_or_load_stores(config: &Config) -> Stores {
        let birthday = config.birthday_height.unwrap_or(0);

        // Coin store
        let coin_store = if config.persist && config.coins_path().exists() {
            SpCoinStore::from_file(config.coins_path())
                .map(|s| s.enable_persist(true))
                .unwrap_or_else(|_| {
                    SpCoinStore::with_path(config.coins_path()).enable_persist(true)
                })
        } else {
            SpCoinStore::with_path(config.coins_path()).enable_persist(config.persist)
        };

        // Label store
        let label_store = if config.persist && config.labels_path().exists() {
            SpLabelStore::from_file(config.labels_path())
                .map(|s| s.enable_persist(true))
                .unwrap_or_else(|_| {
                    SpLabelStore::with_path(config.labels_path()).enable_persist(true)
                })
        } else {
            SpLabelStore::with_path(config.labels_path()).enable_persist(config.persist)
        };

        // Transaction store
        let tx_store = if config.persist && config.txs_path().exists() {
            SpTxStore::from_file(config.txs_path())
                .map(|s| s.enable_persist(true))
                .unwrap_or_else(|_| SpTxStore::with_path(config.txs_path()).enable_persist(true))
        } else {
            SpTxStore::with_path(config.txs_path()).enable_persist(config.persist)
        };

        // Scan state
        let scan_state = if config.persist && config.state_path().exists() {
            ScanState::from_file(config.state_path())
                .map(|s| s.enable_persist(true))
                .unwrap_or_else(|_| {
                    ScanState::with_path(birthday, config.state_path()).enable_persist(true)
                })
        } else {
            ScanState::with_path(birthday, config.state_path()).enable_persist(config.persist)
        };

        (
            Arc::new(Mutex::new(coin_store)),
            Arc::new(Mutex::new(label_store)),
            Arc::new(Mutex::new(tx_store)),
            Arc::new(Mutex::new(scan_state)),
        )
    }

    //-------------------------------------------------------------------------
    // Getters (accessors)
    //-------------------------------------------------------------------------

    /// Returns the account name.
    pub fn name(&self) -> &str {
        &self.config.account_name
    }

    /// Returns the network.
    pub fn network(&self) -> Network {
        self.config.network
    }

    /// Returns a clone of the config.
    pub fn get_config(&self) -> Config {
        self.config.clone()
    }

    /// Returns the Silent Payment address for this account.
    pub fn sp_address(&self) -> SilentPaymentAddress {
        self.client.get_receiving_address()
    }

    /// Takes the notification receiver (can only be called once).
    pub fn receiver(&mut self) -> Option<mpsc::Receiver<Notification>> {
        self.receiver.take()
    }

    //-------------------------------------------------------------------------
    // Coin & Balance Methods
    //-------------------------------------------------------------------------

    /// Returns a clone of all coins in the store.
    pub fn coins(&self) -> BTreeMap<OutPoint, SpCoinEntry> {
        self.coin_store.lock().expect("poisoned").coins().clone()
    }

    /// Returns a coin entry by outpoint if it exists.
    pub fn get_coin(&self, outpoint: &OutPoint) -> Option<SpCoinEntry> {
        self.coin_store
            .lock()
            .expect("poisoned")
            .get(outpoint)
            .cloned()
    }

    /// Returns spendable coins and balance summary.
    pub fn spendable_coins(&self) -> CoinState {
        self.coin_store.lock().expect("poisoned").spendable_coins()
    }

    /// Returns the total spendable balance in satoshis.
    pub fn balance(&self) -> u64 {
        self.spendable_coins().confirmed_balance
    }

    //-------------------------------------------------------------------------
    // Transaction History
    //-------------------------------------------------------------------------

    /// Returns all transaction entries.
    pub fn tx_history(&self) -> Vec<SpTxEntry> {
        self.tx_store
            .lock()
            .expect("poisoned")
            .transactions()
            .into_iter()
            .cloned()
            .collect()
    }

    /// Returns a unified payment history combining coins and transactions.
    ///
    /// This method creates a unified view of all payments by:
    /// 1. Converting coins from coin_store to received payments
    /// 2. Converting transactions from tx_store to payments
    /// 3. Deduplicating by txid (tx_store entries take precedence)
    /// 4. Sorting by height (newest first), then by txid for same height
    ///
    /// For received coins, the label is looked up from label_store.
    /// For transactions, the label from the transaction entry is used,
    /// falling back to label_store lookup.
    pub fn payment_history(&self) -> Vec<Payment> {
        use std::collections::HashMap;

        // Local struct to avoid clippy::type_complexity warning for tx_data tuple
        struct TxData {
            txid: Txid,
            txid_str: String,
            direction: crate::TxDirection,
            amount: u64,
            label: Option<String>,
            height: Option<u32>,
        }

        let mut payments_by_txid: HashMap<String, Payment> = HashMap::new();

        // First, process coins from coin_store as received payments
        // Collect coins data while holding only coin_store lock
        let coins_data: Vec<(OutPoint, u64, u32, String)> = {
            let coin_store = self.coin_store.lock().expect("poisoned");
            coin_store
                .coins()
                .iter()
                .map(|(outpoint, coin)| {
                    (
                        *outpoint,
                        coin.amount_sat(),
                        coin.height(),
                        outpoint.txid.to_string(),
                    )
                })
                .collect()
        };

        // Now lookup labels with only label_store lock
        let coin_labels: HashMap<OutPoint, String> = {
            let label_store = self.label_store.lock().expect("poisoned");
            coins_data
                .iter()
                .filter_map(|(outpoint, _, _, _)| {
                    label_store
                        .outpoint(outpoint)
                        .map(|l| (*outpoint, l.clone()))
                })
                .collect()
        };

        // Process coins without any locks
        for (outpoint, amount, height, txid_str) in coins_data {
            let label = coin_labels.get(&outpoint).cloned().unwrap_or_default();

            let payment = Payment {
                txid: txid_str.clone(),
                payment_type: PaymentType::Receive,
                amount,
                label,
                height: Some(height),
            };

            // If we already have a payment for this txid, aggregate the amount
            if let Some(existing) = payments_by_txid.get_mut(&txid_str) {
                existing.amount += payment.amount;
            } else {
                payments_by_txid.insert(txid_str, payment);
            }
        }

        // Then, process transactions from tx_store (these take precedence)
        // Collect transaction data while holding only tx_store lock
        let tx_data: Vec<TxData> = {
            let tx_store = self.tx_store.lock().expect("poisoned");
            tx_store
                .transactions()
                .iter()
                .map(|tx_entry| TxData {
                    txid: tx_entry.txid,
                    txid_str: tx_entry.txid.to_string(),
                    direction: tx_entry.direction.clone(),
                    amount: tx_entry.amount,
                    label: tx_entry.label.clone(),
                    height: tx_entry.height,
                })
                .collect()
        };

        // Lookup labels for transactions that don't have one, with only label_store lock
        let tx_labels: HashMap<Txid, String> = {
            let label_store = self.label_store.lock().expect("poisoned");
            tx_data
                .iter()
                .filter(|td| td.label.is_none())
                .filter_map(|td| {
                    label_store
                        .transaction(&td.txid)
                        .map(|l| (td.txid, l.clone()))
                })
                .collect()
        };

        // Process transactions without any locks
        for td in tx_data {
            // Determine payment type from transaction direction
            let payment_type = match td.direction {
                crate::TxDirection::Incoming => PaymentType::Receive,
                crate::TxDirection::Outgoing => PaymentType::Send,
                crate::TxDirection::Internal => PaymentType::Send, // Treat internal as send
            };

            // Get label: prefer tx_entry label, fallback to label_store
            let label = td
                .label
                .or_else(|| tx_labels.get(&td.txid).cloned())
                .unwrap_or_default();

            let payment = Payment {
                txid: td.txid_str.clone(),
                payment_type,
                amount: td.amount,
                label,
                height: td.height,
            };

            // tx_store entries take precedence over coin_store entries
            payments_by_txid.insert(td.txid_str, payment);
        }

        // Collect and sort payments
        let mut payments: Vec<Payment> = payments_by_txid.into_values().collect();

        // Sort by height (newest first, None last), then by txid for stability
        payments.sort_by(|a, b| match (b.height, a.height) {
            (Some(h_b), Some(h_a)) => h_b.cmp(&h_a).then_with(|| a.txid.cmp(&b.txid)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.txid.cmp(&b.txid),
        });

        payments
    }

    //-------------------------------------------------------------------------
    // Labels
    //-------------------------------------------------------------------------

    /// Update the label for a coin.
    pub fn update_coin_label(&self, outpoint: OutPoint, label: String) {
        let mut store = self.label_store.lock().expect("poisoned");
        if label.is_empty() {
            store.remove(&LabelKey::OutPoint(outpoint));
        } else {
            store.set_outpoint(outpoint, label);
        }
        store.persist();
    }

    /// Update the label for a transaction.
    pub fn update_tx_label(&self, txid: Txid, label: String) {
        let mut store = self.label_store.lock().expect("poisoned");
        if label.is_empty() {
            store.remove(&LabelKey::Transaction(txid));
        } else {
            store.set_transaction(txid, label);
        }
        store.persist();
    }

    /// Get the label for a coin.
    pub fn get_coin_label(&self, outpoint: &OutPoint) -> Option<String> {
        self.label_store
            .lock()
            .expect("poisoned")
            .outpoint(outpoint)
            .cloned()
    }

    //-------------------------------------------------------------------------
    // Scanning
    //-------------------------------------------------------------------------

    /// Get the current block height from the backend.
    pub fn block_height(&self) -> Result<u32, AccountError> {
        self.backend
            .block_height()
            .map(|h| h.to_consensus_u32())
            .map_err(|e| AccountError::Network(e.to_string()))
    }

    /// Check if the backend is online and reachable.
    pub fn backend_online(&self) -> bool {
        self.block_height().is_ok()
    }

    /// Returns the Blindbit server URL.
    pub fn blindbit_url(&self) -> &str {
        &self.config.blindbit_url
    }

    /// Update the Blindbit server URL.
    pub fn set_blindbit_url(&mut self, url: String) {
        self.config.blindbit_url = url;
        // Recreate backend with new URL
        if let Ok(backend) = Self::create_backend(&self.config) {
            self.backend = backend;
        }
        if self.config.persist {
            self.config.to_file();
        }
    }

    /// Scan a range of blocks for silent payment outputs.
    ///
    /// # Arguments
    /// * `start` - Starting block height (or None to continue from last scan)
    /// * `end` - Ending block height (or None to use current chain tip)
    pub fn scan_blocks(
        &mut self,
        start: Option<u32>,
        end: Option<u32>,
    ) -> Result<(), AccountError> {
        // Determine start height
        let start_height =
            start.unwrap_or_else(|| self.scan_state.lock().expect("poisoned").next_scan_start());

        // Determine end height
        let end_height = match end {
            Some(h) => h,
            None => self.block_height()?,
        };

        if start_height > end_height {
            return Ok(()); // Nothing to scan
        }

        let start = Height::from_consensus(start_height)
            .map_err(|e| AccountError::Scan(format!("invalid start height: {}", e)))?;
        let end = Height::from_consensus(end_height)
            .map_err(|e| AccountError::Scan(format!("invalid end height: {}", e)))?;

        // Get dust limit
        let dust_limit = self.config.dust_limit.map(Amount::from_sat);

        // Create a fresh backend for the scanner (scanner takes ownership)
        let scan_backend = Self::create_backend(&self.config)?;

        // Query server info to determine cutthrough support
        let with_cutthrough = scan_backend
            .info()
            .map(|info| info.tweaks_cut_through_with_dust_filter)
            .unwrap_or(false);

        // Create scanner
        let mut scanner = SpAccount::new(
            scan_backend,
            self.client.clone(),
            AccountUpdater {
                coin_store: self.coin_store.clone(),
                tx_store: self.tx_store.clone(),
                scan_state: self.scan_state.clone(),
                sender: self.sender.clone(),
            },
        );

        // Send notification
        let _ = self.sender.send(Notification::ScanStarted);

        // Perform scan
        scanner
            .scan_blocks(start, end, dust_limit, with_cutthrough)
            .map_err(|e| AccountError::Scan(e.to_string()))?;

        // Send completion notification
        let _ = self.sender.send(Notification::ScanCompleted);

        Ok(())
    }

    /// Start a background scanner thread.
    ///
    /// The scanner will continuously poll for new blocks and scan them.
    pub fn start_scanner(&mut self) -> Result<(), AccountError> {
        if self.scanner_handle.is_some() {
            return Err(AccountError::ScannerAlreadyRunning);
        }

        // Reset stop flag
        self.scanner_stop.store(false, Ordering::Relaxed);

        // Clone what we need for the thread
        let client = self.client.clone();
        let blindbit_url = self.config.blindbit_url.clone();
        let dust_limit = self.config.dust_limit.map(Amount::from_sat);
        let coin_store = self.coin_store.clone();
        let tx_store = self.tx_store.clone();
        let scan_state = self.scan_state.clone();
        let sender = self.sender.clone();
        let stop = self.scanner_stop.clone();

        let handle = thread::spawn(move || {
            // Create backend in thread for block height checks
            let http_client = UreqClient::new();
            let backend = match BlindbitBackend::new(blindbit_url.clone(), http_client) {
                Ok(b) => b,
                Err(e) => {
                    let _ = sender.send(Notification::ScanError {
                        message: e.to_string(),
                        retries_attempted: 0,
                    });
                    return;
                }
            };

            let mut attempt = 0u32;

            while !stop.load(Ordering::Relaxed) {
                // Get current chain tip
                let chain_height = match backend.block_height() {
                    Ok(h) => h.to_consensus_u32(),
                    Err(e) => {
                        log::warn!("scanner: failed to get block height: {}", e);
                        // Exponential backoff
                        let delay = Duration::from_millis(100 << attempt.min(10));
                        thread::sleep(delay);
                        attempt += 1;
                        continue;
                    }
                };

                // Reset backoff on success
                attempt = 0;

                // Get scan start
                let start_height = scan_state.lock().expect("poisoned").next_scan_start();

                if start_height > chain_height {
                    // Nothing to scan, wait a bit
                    thread::sleep(Duration::from_secs(10));
                    continue;
                }

                let start = match Height::from_consensus(start_height) {
                    Ok(h) => h,
                    Err(_) => continue,
                };
                let end = match Height::from_consensus(chain_height) {
                    Ok(h) => h,
                    Err(_) => continue,
                };

                let _ = sender.send(Notification::ScanStarted);

                // Create a fresh backend for this scan iteration
                let http_client_scan = UreqClient::new();
                let scan_backend =
                    match BlindbitBackend::new(blindbit_url.clone(), http_client_scan) {
                        Ok(b) => b,
                        Err(e) => {
                            let _ = sender.send(Notification::ScanError {
                                message: e.to_string(),
                                retries_attempted: 0,
                            });
                            continue;
                        }
                    };

                // Query server info to determine cutthrough support
                let with_cutthrough = scan_backend
                    .info()
                    .map(|info| info.tweaks_cut_through_with_dust_filter)
                    .unwrap_or(false);

                // Create scanner
                let mut scanner = SpAccount::new(
                    scan_backend,
                    client.clone(),
                    AccountUpdater {
                        coin_store: coin_store.clone(),
                        tx_store: tx_store.clone(),
                        scan_state: scan_state.clone(),
                        sender: sender.clone(),
                    },
                );

                // Scan
                match scanner.scan_blocks(start, end, dust_limit, with_cutthrough) {
                    Ok(()) => {
                        let _ = sender.send(Notification::ScanCompleted);
                    }
                    Err(e) => {
                        let _ = sender.send(Notification::ScanError {
                            message: e.to_string(),
                            retries_attempted: 0,
                        });
                    }
                }

                // Brief pause before next iteration
                thread::sleep(Duration::from_secs(5));
            }

            let _ = sender.send(Notification::Stopped);
        });

        self.scanner_handle = Some(handle);
        Ok(())
    }

    /// Stop the background scanner thread.
    pub fn stop_scanner(&mut self) {
        self.scanner_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.scanner_handle.take() {
            let _ = handle.join();
        }
    }

    /// Check if the scanner is currently running.
    pub fn scanner_running(&self) -> bool {
        self.scanner_handle
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }

    /// Returns the last scanned block height.
    pub fn last_scanned_height(&self) -> Option<u32> {
        self.scan_state
            .lock()
            .expect("poisoned")
            .last_scanned_height()
    }

    //-------------------------------------------------------------------------
    // Transaction Building
    //-------------------------------------------------------------------------

    /// Check if this account can sign transactions.
    ///
    /// Returns true if we have the spend secret key.
    pub fn can_sign(&self) -> bool {
        self.client.try_get_secret_spend_key().is_ok()
    }

    /// Create an unsigned transaction sending to the given recipients.
    ///
    /// This method:
    /// 1. Gets spendable coins from the coin store
    /// 2. Uses coin selection to choose UTXOs
    /// 3. Builds the unsigned transaction with silent payment outputs
    ///
    /// # Arguments
    /// * `recipients` - List of (address, amount) pairs to send to
    /// * `fee_rate` - Fee rate in sat/vbyte
    ///
    /// # Errors
    /// * `AccountError::Transaction` if there are insufficient funds or other tx building errors
    pub fn create_transaction(
        &self,
        recipients: Vec<(RecipientAddress, Amount)>,
        fee_rate: FeeRate,
    ) -> Result<SilentPaymentUnsignedTransaction, AccountError> {
        // Get spendable coins from coin store
        let coin_state = self.coin_store.lock().expect("poisoned").spendable_coins();

        // Convert to the format SpClient expects: Vec<(OutPoint, OwnedOutput)>
        let available_utxos: Vec<(OutPoint, OwnedOutput)> = coin_state
            .coins
            .into_iter()
            .map(|(outpoint, entry)| (outpoint, entry.owned_output().clone()))
            .collect();

        if available_utxos.is_empty() {
            return Err(AccountError::Transaction("no spendable coins".to_string()));
        }

        // Convert recipients to Recipient structs
        let recipients: Vec<Recipient> = recipients
            .into_iter()
            .map(|(address, amount)| Recipient { address, amount })
            .collect();

        // Create the transaction using SpClient
        self.client
            .create_new_transaction(available_utxos, recipients, fee_rate, self.config.network)
            .map_err(|e| AccountError::Transaction(e.to_string()))
    }

    /// Create a drain transaction that spends ALL coins to a single recipient.
    ///
    /// This method:
    /// 1. Gets ALL spendable coins from the coin store
    /// 2. Uses ALL of them as inputs (drains the wallet)
    /// 3. Sends everything (minus fees) to the single recipient
    ///
    /// # Arguments
    /// * `recipient` - The address to send all funds to
    /// * `fee_rate` - Fee rate in sat/vbyte
    ///
    /// # Errors
    /// * `AccountError::Transaction` if there are no spendable coins or other tx building errors
    pub fn create_drain_transaction(
        &self,
        recipient: RecipientAddress,
        fee_rate: FeeRate,
    ) -> Result<SilentPaymentUnsignedTransaction, AccountError> {
        // Get ALL spendable coins from coin store
        let coin_state = self.coin_store.lock().expect("poisoned").spendable_coins();

        // Convert to the format SpClient expects: Vec<(OutPoint, OwnedOutput)>
        let available_utxos: Vec<(OutPoint, OwnedOutput)> = coin_state
            .coins
            .into_iter()
            .map(|(outpoint, entry)| (outpoint, entry.owned_output().clone()))
            .collect();

        if available_utxos.is_empty() {
            return Err(AccountError::Transaction("no spendable coins".to_string()));
        }

        // Create the drain transaction using SpClient (uses ALL inputs)
        self.client
            .create_drain_transaction(available_utxos, recipient, fee_rate, self.config.network)
            .map_err(|e| AccountError::Transaction(e.to_string()))
    }

    /// Finalize an unsigned transaction, preparing it for signing.
    ///
    /// This method converts the unsigned transaction to a ready-to-sign format by:
    /// 1. Creating the actual transaction inputs from selected UTXOs
    /// 2. Generating silent payment recipient public keys
    /// 3. Building the final transaction outputs with real script pubkeys
    ///
    /// After finalization, the transaction can be signed with `sign_transaction()`.
    ///
    /// # Arguments
    /// * `unsigned_tx` - The unsigned transaction from `create_transaction()` or `create_drain_transaction()`
    ///
    /// # Errors
    /// * `AccountError::Transaction` if finalization fails (e.g., invalid silent payment address)
    pub fn finalize_transaction(
        &self,
        unsigned_tx: SilentPaymentUnsignedTransaction,
    ) -> Result<SilentPaymentUnsignedTransaction, AccountError> {
        SpClient::finalize_transaction(unsigned_tx)
            .map_err(|e| AccountError::Transaction(e.to_string()))
    }

    //-------------------------------------------------------------------------
    // Signing
    //-------------------------------------------------------------------------

    /// Sign a finalized transaction.
    ///
    /// This method signs all inputs of the transaction using the spend key.
    /// The transaction must have been finalized with `finalize_transaction()` first.
    ///
    /// # Arguments
    /// * `unsigned_tx` - The finalized unsigned transaction
    ///
    /// # Errors
    /// * `AccountError::NoKeys` if this account cannot sign (no spend secret key)
    /// * `AccountError::Transaction` if signing fails
    pub fn sign_transaction(
        &self,
        unsigned_tx: SilentPaymentUnsignedTransaction,
    ) -> Result<Transaction, AccountError> {
        if !self.can_sign() {
            return Err(AccountError::NoKeys);
        }

        // Generate random bytes for Schnorr signature auxiliary randomness
        let mut aux_rand = [0u8; 32];
        getrandom::getrandom(&mut aux_rand).map_err(|e| {
            AccountError::Transaction(format!("failed to generate random bytes: {}", e))
        })?;

        self.client
            .sign_transaction(unsigned_tx, &aux_rand)
            .map_err(|e| AccountError::Transaction(e.to_string()))
    }

    //-------------------------------------------------------------------------
    // Broadcasting
    //-------------------------------------------------------------------------

    /// Broadcast a transaction to the network.
    ///
    /// Uses the configured broadcast_url to POST the transaction.
    pub fn broadcast(&self, tx: &Transaction) -> Result<Txid, AccountError> {
        let broadcast_url = self
            .config
            .broadcast_url
            .as_ref()
            .ok_or(AccountError::NoBroadcastUrl)?;

        // Serialize transaction to hex
        let tx_hex = bitcoin::consensus::encode::serialize_hex(tx);

        // POST the transaction
        let response = ureq::post(broadcast_url)
            .set("Content-Type", "text/plain")
            .send_string(&tx_hex)
            .map_err(|e| AccountError::Broadcast(e.to_string()))?;

        // Parse response for txid
        let response_text = response
            .into_string()
            .map_err(|e| AccountError::Broadcast(e.to_string()))?;

        // Try to parse as txid
        let txid: Txid = response_text
            .trim()
            .parse()
            .map_err(|e| AccountError::Broadcast(format!("invalid txid in response: {}", e)))?;

        Ok(txid)
    }

    /// Sign and broadcast in one step.
    ///
    /// This is a convenience method that combines `sign_transaction()` and `broadcast()`.
    ///
    /// # Arguments
    /// * `unsigned_tx` - The finalized unsigned transaction to sign and broadcast
    ///
    /// # Errors
    /// * `AccountError::NoKeys` if this account cannot sign (no spend secret key)
    /// * `AccountError::Transaction` if signing fails
    /// * `AccountError::NoBroadcastUrl` if no broadcast URL is configured
    /// * `AccountError::Broadcast` if broadcast fails
    pub fn sign_and_broadcast(
        &self,
        unsigned_tx: SilentPaymentUnsignedTransaction,
    ) -> Result<Txid, AccountError> {
        let signed_tx = self.sign_transaction(unsigned_tx)?;
        self.broadcast(&signed_tx)
    }

    //-------------------------------------------------------------------------
    // Persistence
    //-------------------------------------------------------------------------

    /// Persist all stores to disk.
    pub fn persist(&self) {
        self.coin_store.lock().expect("poisoned").persist();
        self.label_store.lock().expect("poisoned").persist();
        self.tx_store.lock().expect("poisoned").persist();
        self.scan_state.lock().expect("poisoned").persist();
    }
}

impl Drop for Account {
    fn drop(&mut self) {
        self.stop_scanner();
        self.persist();
    }
}

//=============================================================================
// AccountUpdater - Implements Updater trait for Account
//=============================================================================

/// Internal struct that implements the Updater trait for scanning.
struct AccountUpdater {
    coin_store: Arc<Mutex<SpCoinStore>>,
    tx_store: Arc<Mutex<SpTxStore>>,
    scan_state: Arc<Mutex<ScanState>>,
    sender: mpsc::Sender<Notification>,
}

impl Updater for AccountUpdater {
    fn record_scan_progress(
        &mut self,
        _start: Height,
        current: Height,
        end: Height,
    ) -> Result<(), spdk_core::Error> {
        // Send progress notification
        let _ = self.sender.send(Notification::ScanProgress {
            current: current.to_consensus_u32(),
            end: end.to_consensus_u32(),
        });
        Ok(())
    }

    fn record_block_outputs(
        &mut self,
        height: Height,
        block_hash: BlockHash,
        found_outputs: HashMap<OutPoint, OwnedOutput>,
    ) -> Result<(), spdk_core::Error> {
        // Update scan state
        {
            let mut state = self.scan_state.lock().expect("poisoned");
            state.update(height.to_consensus_u32(), *block_hash.as_byte_array());
            state.persist();
        }

        // Insert outputs into coin store AND persist in same lock scope
        {
            let mut store = self.coin_store.lock().expect("poisoned");
            for (outpoint, output) in found_outputs {
                store.insert(outpoint, output);
                let _ = self.sender.send(Notification::NewOutput(outpoint));
            }
            store.persist();
        }

        Ok(())
    }

    fn record_block_inputs(
        &mut self,
        height: Height,
        block_hash: BlockHash,
        found_inputs: HashSet<OutPoint>,
    ) -> Result<(), spdk_core::Error> {
        // Update scan state
        {
            let mut state = self.scan_state.lock().expect("poisoned");
            state.update(height.to_consensus_u32(), *block_hash.as_byte_array());
            state.persist();
        }

        // Mark inputs as spent AND persist in same lock scope
        {
            let mut store = self.coin_store.lock().expect("poisoned");
            for outpoint in found_inputs {
                store.mark_mined(&outpoint, *block_hash.as_byte_array());
                let _ = self.sender.send(Notification::OutputSpent(outpoint));
            }
            store.persist();
        }

        Ok(())
    }

    fn save_to_persistent_storage(&mut self) -> Result<(), spdk_core::Error> {
        self.coin_store.lock().expect("poisoned").persist();
        self.tx_store.lock().expect("poisoned").persist();
        self.scan_state.lock().expect("poisoned").persist();
        Ok(())
    }
}

//=============================================================================
// Tests
//=============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_config() -> Config {
        Config::new(
            "test-account".to_string(),
            Network::Signet,
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about".to_string(),
            "https://blindbit.example.com".to_string(),
            PathBuf::from("/tmp/bwk-sp-account-test"),
        ).enable_persist(false)
    }

    #[test]
    fn test_account_error_display() {
        let err = AccountError::Config("test error".to_string());
        assert!(err.to_string().contains("config invalid"));

        let err = AccountError::Scan("scan error".to_string());
        assert!(err.to_string().contains("scan failed"));

        let err = AccountError::Network("network error".to_string());
        assert!(err.to_string().contains("network error"));

        let err = AccountError::NoKeys;
        assert!(err.to_string().contains("no keys"));

        let err = AccountError::Broadcast("broadcast error".to_string());
        assert!(err.to_string().contains("broadcast failed"));

        let err = AccountError::NoBroadcastUrl;
        assert!(err.to_string().contains("no broadcast url"));

        let err = AccountError::ScannerAlreadyRunning;
        assert!(err.to_string().contains("already running"));
    }

    #[test]
    fn test_notification_variants() {
        let notif = Notification::ScanStarted;
        assert!(matches!(notif, Notification::ScanStarted));

        let notif = Notification::ScanProgress {
            current: 100,
            end: 200,
        };
        if let Notification::ScanProgress { current, end } = notif {
            assert_eq!(current, 100);
            assert_eq!(end, 200);
        } else {
            panic!("expected ScanProgress");
        }

        let notif = Notification::ScanCompleted;
        assert!(matches!(notif, Notification::ScanCompleted));

        let notif = Notification::ScanError {
            message: "test error".to_string(),
            retries_attempted: 3,
        };
        if let Notification::ScanError {
            message,
            retries_attempted,
        } = notif
        {
            assert_eq!(message, "test error");
            assert_eq!(retries_attempted, 3);
        } else {
            panic!("expected ScanError");
        }

        let notif = Notification::Stopped;
        assert!(matches!(notif, Notification::Stopped));
    }

    #[test]
    fn test_payment_type_eq() {
        assert_eq!(PaymentType::Receive, PaymentType::Receive);
        assert_eq!(PaymentType::Send, PaymentType::Send);
        assert_ne!(PaymentType::Receive, PaymentType::Send);
    }

    #[test]
    fn test_payment_struct() {
        let payment = Payment {
            txid: "abc123".to_string(),
            payment_type: PaymentType::Receive,
            amount: 50000,
            label: "test payment".to_string(),
            height: Some(800000),
        };

        assert_eq!(payment.txid, "abc123");
        assert_eq!(payment.payment_type, PaymentType::Receive);
        assert_eq!(payment.amount, 50000);
        assert_eq!(payment.label, "test payment");
        assert_eq!(payment.height, Some(800000));
    }

    #[test]
    fn test_payment_struct_unconfirmed() {
        let payment = Payment {
            txid: "def456".to_string(),
            payment_type: PaymentType::Send,
            amount: 10000,
            label: String::new(),
            height: None,
        };

        assert_eq!(payment.txid, "def456");
        assert_eq!(payment.payment_type, PaymentType::Send);
        assert_eq!(payment.amount, 10000);
        assert!(payment.label.is_empty());
        assert!(payment.height.is_none());
    }

    #[test]
    fn test_payment_clone() {
        let payment = Payment {
            txid: "abc123".to_string(),
            payment_type: PaymentType::Receive,
            amount: 50000,
            label: "original".to_string(),
            height: Some(100),
        };

        let cloned = payment.clone();
        assert_eq!(cloned.txid, payment.txid);
        assert_eq!(cloned.payment_type, payment.payment_type);
        assert_eq!(cloned.amount, payment.amount);
        assert_eq!(cloned.label, payment.label);
        assert_eq!(cloned.height, payment.height);
    }

    #[test]
    fn test_payment_debug() {
        let payment = Payment {
            txid: "abc".to_string(),
            payment_type: PaymentType::Send,
            amount: 1000,
            label: "test".to_string(),
            height: Some(1),
        };

        let debug_str = format!("{:?}", payment);
        assert!(debug_str.contains("Payment"));
        assert!(debug_str.contains("abc"));
        assert!(debug_str.contains("Send"));
    }

    #[test]
    fn test_config_validation_no_keys() {
        let mut config = test_config();
        config.mnemonic = None;
        config.scan_sk = None;

        let result = Account::new(config);
        assert!(result.is_err());
        if let Err(AccountError::Config(msg)) = result {
            assert!(msg.contains("mnemonic or scan_sk"));
        } else {
            panic!("expected Config error");
        }
    }

    #[test]
    fn test_config_validation_no_blindbit_url() {
        let mut config = test_config();
        config.blindbit_url = String::new();

        let result = Account::new(config);
        assert!(result.is_err());
        if let Err(AccountError::Config(msg)) = result {
            assert!(msg.contains("blindbit_url"));
        } else {
            panic!("expected Config error");
        }
    }

    #[test]
    fn test_from_mnemonic_invalid_mnemonic() {
        let result = Account::from_mnemonic(
            "test-account".to_string(),
            Network::Signet,
            "invalid mnemonic words",
            "https://blindbit.example.com".to_string(),
            PathBuf::from("/tmp/bwk-sp-test-from-mnemonic"),
        );
        assert!(result.is_err());
        if let Err(AccountError::Config(msg)) = result {
            assert!(msg.contains("invalid mnemonic"));
        } else {
            panic!("expected Config error for invalid mnemonic");
        }
    }

    #[test]
    fn test_from_mnemonic_empty_blindbit_url() {
        let result = Account::from_mnemonic(
            "test-account".to_string(),
            Network::Signet,
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
            String::new(),
            PathBuf::from("/tmp/bwk-sp-test-from-mnemonic-2"),
        );
        assert!(result.is_err());
        if let Err(AccountError::Config(msg)) = result {
            assert!(msg.contains("blindbit_url"));
        } else {
            panic!("expected Config error for empty blindbit_url");
        }
    }

    // Note: Full Account creation tests require a working blindbit backend
    // which is not available in unit tests. Integration tests would test
    // the full flow.
}
