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
use bitcoin::secp256k1::{Keypair, Message, Secp256k1, SecretKey};
use bitcoin::sighash::{Prevouts, SighashCache};
use bitcoin::taproot::Signature;
use bitcoin::{Amount, BlockHash, Network, OutPoint, TapSighashType, Txid};
use miniscript::psbt::PsbtExt;
use silentpayments::SilentPaymentAddress;

use backend_blindbit_native_non_async::{BlindbitBackend, InfoResponse, UreqClient};
use spdk_core::account::SpAccount;
use spdk_core::{bip39, OwnedOutput, SpClient, SpScanner, Updater};

use crate::{
    coin_store::{MergedCoinSource, SpCoinSource},
    recipient::{SpChangeRecipientProvider, SpSecretProvider},
    CoinState, Config, LabelKey, ScanState, SpCoinEntry, SpCoinStore, SpLabelStore, SpTxEntry,
    SpTxStore,
};

// Type Aliases

/// Type alias for the tuple of stores returned by create_or_load_stores.
type Stores = (
    Arc<Mutex<SpCoinStore>>,
    Arc<Mutex<SpLabelStore>>,
    Arc<Mutex<SpTxStore>>,
    Arc<Mutex<ScanState>>,
);

// AccountError

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
    /// Scanner thread is already running
    #[error("scanner already running")]
    ScannerAlreadyRunning,
    /// Transaction building failed
    #[error("transaction error: {0}")]
    Transaction(String),
}

// Re-use unified Notification from bwk
pub use bwk::{Notification, SpNotification};

// ScanMode

/// Scan mode for the scanner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScanMode {
    /// One-shot scan: scan from last position to current chain tip, then return.
    /// If already at tip, returns immediately.
    #[default]
    OneShot,
    /// Continuous watch mode: scan to tip, then keep watching for new blocks.
    /// Runs in background until explicitly stopped via `stop_scan()`.
    Continuous,
}

// PaymentType

/// Type of payment (for UI display).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaymentType {
    /// Received payment
    Receive,
    /// Sent payment
    Send,
}

// Payment

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

// Account

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
    /// Optional embedded standard wallet accounts (segwit, taproot, etc.)
    sub_accounts: Vec<bwk::Account>,
}

impl Account {
    // Constructors

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

        // Create sub-accounts from config descriptors
        let sub_accounts = config
            .descriptors
            .iter()
            .enumerate()
            .map(|(i, sub_cfg)| {
                let mut bwk_config = bwk::Config {
                    data_dir: config.account_dir(),
                    dir_name: "",
                    account: format!("{}-sub-{}", config.account_name, i),
                    electrum_url: sub_cfg.electrum_url.clone(),
                    electrum_port: sub_cfg.electrum_port,
                    offline: if sub_cfg.electrum_url.is_none() {
                        Some(true)
                    } else {
                        None
                    },
                    network: config.network,
                    look_ahead: 20,
                    mnemonic: config.mnemonic.clone(),
                    descriptor: sub_cfg.descriptor.clone(),
                    persist: config.persist,
                };
                bwk_config.dir_name =
                    Box::leak(format!("{}-sub-{}", config.account_name, i).into_boxed_str());
                bwk::Account::new_with_sender(bwk_config, sender.clone())
            })
            .collect();

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
            sub_accounts,
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
        let birthday = config
            .birthday_height
            .unwrap_or_else(|| config.min_birthday_height());

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
    } // Getters (accessors)

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

    /// Returns a reference to the SpClient for advanced operations.
    pub fn sp_client(&self) -> &SpClient {
        &self.client
    }

    /// Takes the notification receiver (can only be called once).
    pub fn receiver(&mut self) -> Option<mpsc::Receiver<Notification>> {
        self.receiver.take()
    } // Coin & Balance Methods

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
    } // Transaction History

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
    } // Labels

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
    } // Scanning

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

    /// Start a scan with the specified mode.
    ///
    /// # Arguments
    /// * `mode` - The scanning mode (OneShot or Continuous)
    ///
    /// # Modes
    /// - `OneShot`: Synchronous scan from last position to current chain tip, then returns.
    ///   If already at tip, returns immediately without scanning.
    /// - `Continuous`: Spawns a background thread that scans to tip, then watches for new blocks.
    ///   Returns immediately after spawning. Use `stop_scan()` to stop.
    ///
    /// # Errors
    /// - `AccountError::ScannerAlreadyRunning` if continuous scan is already active
    /// - `AccountError::Scan` if scan fails
    pub fn start_scan(&mut self, mode: ScanMode) -> Result<(), AccountError> {
        match mode {
            ScanMode::OneShot => self.scan_oneshot(),
            ScanMode::Continuous => self.start_continuous_scan(),
        }
    }

    /// Internal: Execute one-shot scan to current chain tip.
    fn scan_oneshot(&mut self) -> Result<(), AccountError> {
        let start_height = self.scan_state.lock().expect("poisoned").next_scan_start();
        let end_height = self.block_height()?;

        if start_height > end_height {
            return Ok(()); // Already at tip, nothing to scan
        }

        let start = Height::from_consensus(start_height)
            .map_err(|e| AccountError::Scan(format!("invalid start height: {}", e)))?;
        let end = Height::from_consensus(end_height)
            .map_err(|e| AccountError::Scan(format!("invalid end height: {}", e)))?;

        let dust_limit = self.config.dust_limit.map(Amount::from_sat);
        let scan_backend = Self::create_backend(&self.config)?;

        let with_cutthrough = scan_backend
            .info()
            .map(|info| info.tweaks_cut_through_with_dust_filter)
            .unwrap_or(false);

        let mut scanner = SpAccount::new(
            scan_backend,
            self.client.clone(),
            AccountUpdater {
                coin_store: self.coin_store.clone(),
                tx_store: self.tx_store.clone(),
                scan_state: self.scan_state.clone(),
                sender: self.sender.clone(),
            },
            self.scanner_stop.clone(),
        );

        let _ = self
            .sender
            .send(Notification::Sp(SpNotification::ScanStarted {
                start: start_height,
                end: end_height,
            }));

        scanner
            .scan_blocks(start, end, dust_limit, with_cutthrough)
            .map_err(|e| AccountError::Scan(e.to_string()))?;

        let _ = self
            .sender
            .send(Notification::Sp(SpNotification::ScanCompleted));

        Ok(())
    }

    /// Internal: Start continuous scan in background thread.
    fn start_continuous_scan(&mut self) -> Result<(), AccountError> {
        if self.scanner_handle.is_some() {
            return Err(AccountError::ScannerAlreadyRunning);
        }

        self.scanner_stop.store(false, Ordering::Relaxed);
        let _ = self
            .sender
            .send(Notification::Sp(SpNotification::StartingScan));

        let client = self.client.clone();
        let blindbit_url = self.config.blindbit_url.clone();
        let dust_limit = self.config.dust_limit.map(Amount::from_sat);
        let coin_store = self.coin_store.clone();
        let tx_store = self.tx_store.clone();
        let scan_state = self.scan_state.clone();
        let sender = self.sender.clone();
        let stop = self.scanner_stop.clone();

        let handle = thread::spawn(move || {
            let http_client = UreqClient::new();
            let backend = match BlindbitBackend::new(blindbit_url.clone(), http_client) {
                Ok(b) => b,
                Err(e) => {
                    let _ = sender.send(Notification::Sp(SpNotification::FailStartScanning {
                        message: e.to_string(),
                    }));
                    let _ = sender.send(Notification::Sp(SpNotification::ScanStopped));
                    return;
                }
            };

            let http_client_scan = UreqClient::new();
            let mut last_notified_tip: Option<u32> = None;
            let mut waiting = false;

            let scan_backend = match BlindbitBackend::new(blindbit_url.clone(), http_client_scan) {
                Ok(b) => b,
                Err(e) => {
                    let _ = sender.send(Notification::Sp(SpNotification::FailStartScanning {
                        message: e.to_string(),
                    }));
                    let _ = sender.send(Notification::Sp(SpNotification::ScanStopped));
                    return;
                }
            };

            let with_cutthrough = scan_backend
                .info()
                .map(|info| info.tweaks_cut_through_with_dust_filter)
                .unwrap_or(false);

            let mut scanner = match SpAccount::restore(
                scan_backend,
                client.clone(),
                AccountUpdater {
                    coin_store: coin_store.clone(),
                    tx_store: tx_store.clone(),
                    scan_state: scan_state.clone(),
                    sender: sender.clone(),
                },
                stop.clone(),
            ) {
                Ok(s) => s,
                Err(e) => {
                    let _ = sender.send(Notification::Sp(SpNotification::FailStartScanning {
                        message: e.to_string(),
                    }));
                    let _ = sender.send(Notification::Sp(SpNotification::ScanStopped));
                    return;
                }
            };

            while !stop.load(Ordering::Relaxed) {
                let chain_height = match backend.block_height() {
                    Ok(h) => h.to_consensus_u32(),
                    Err(e) => {
                        log::warn!("scanner: failed to get block height: {}", e);
                        let _ = sender.send(Notification::Sp(SpNotification::FailStartScanning {
                            message: e.to_string(),
                        }));
                        break;
                    }
                };

                let start_height = scan_state.lock().expect("poisoned").next_scan_start();

                if start_height > chain_height {
                    if !waiting {
                        let _ = sender.send(Notification::Sp(SpNotification::WaitingForBlocks {
                            tip_height: chain_height,
                        }));
                        waiting = true;
                    }
                    thread::sleep(Duration::from_secs(2));
                    continue;
                }

                waiting = false;

                // New blocks detected - notify if we were previously waiting
                if let Some(prev_tip) = last_notified_tip {
                    if chain_height > prev_tip {
                        let _ = sender.send(Notification::Sp(SpNotification::NewBlocksDetected {
                            from_height: prev_tip,
                            to_height: chain_height,
                        }));
                    }
                }

                let start = match Height::from_consensus(start_height) {
                    Ok(h) => h,
                    Err(_) => continue,
                };
                let end = match Height::from_consensus(chain_height) {
                    Ok(h) => h,
                    Err(_) => continue,
                };

                let _ = sender.send(Notification::Sp(SpNotification::ScanStarted {
                    start: start.to_consensus_u32(),
                    end: end.to_consensus_u32(),
                }));

                match scanner.scan_blocks(start, end, dust_limit, with_cutthrough) {
                    Ok(()) => {
                        let _ = sender.send(Notification::Sp(SpNotification::ScanCompleted));
                        last_notified_tip = Some(chain_height);
                    }
                    Err(e) => {
                        let _ = sender.send(Notification::Sp(SpNotification::FailScan {
                            message: e.to_string(),
                        }));
                        break;
                    }
                }

                // Brief pause before checking for new blocks
                thread::sleep(Duration::from_millis(500));
            }

            let _ = sender.send(Notification::Sp(SpNotification::ScanStopped));
        });

        self.scanner_handle = Some(handle);
        Ok(())
    }

    /// Stop the continuous scan.
    ///
    /// No-op if not running in continuous mode.
    pub fn stop_scan(&mut self) {
        let _ = self
            .sender
            .send(Notification::Sp(SpNotification::StoppingScan));
        self.scanner_stop.store(true, Ordering::Relaxed);
        self.scanner_handle = None;
    }

    /// Check if a continuous scan is currently running.
    pub fn is_scanning(&self) -> bool {
        self.scanner_handle
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }

    /// Scan a range of blocks for silent payment outputs.
    pub fn scan_blocks(
        &mut self,
        start: Option<u32>,
        end: Option<u32>,
    ) -> Result<(), AccountError> {
        // If both are None, use the new one-shot scan
        if start.is_none() && end.is_none() {
            return self.start_scan(ScanMode::OneShot);
        }

        // Custom range scan (legacy behavior)
        let start_height =
            start.unwrap_or_else(|| self.scan_state.lock().expect("poisoned").next_scan_start());
        let end_height = match end {
            Some(h) => h,
            None => self.block_height()?,
        };

        if start_height > end_height {
            return Ok(());
        }

        let start = Height::from_consensus(start_height)
            .map_err(|e| AccountError::Scan(format!("invalid start height: {}", e)))?;
        let end = Height::from_consensus(end_height)
            .map_err(|e| AccountError::Scan(format!("invalid end height: {}", e)))?;

        let dust_limit = self.config.dust_limit.map(Amount::from_sat);
        let scan_backend = Self::create_backend(&self.config)?;

        let with_cutthrough = scan_backend
            .info()
            .map(|info| info.tweaks_cut_through_with_dust_filter)
            .unwrap_or(false);

        let mut scanner = SpAccount::new(
            scan_backend,
            self.client.clone(),
            AccountUpdater {
                coin_store: self.coin_store.clone(),
                tx_store: self.tx_store.clone(),
                scan_state: self.scan_state.clone(),
                sender: self.sender.clone(),
            },
            self.scanner_stop.clone(),
        );

        scanner
            .scan_blocks(start, end, dust_limit, with_cutthrough)
            .map_err(|e| AccountError::Scan(e.to_string()))?;

        let _ = self
            .sender
            .send(Notification::Sp(SpNotification::ScanCompleted));
        Ok(())
    }

    /// Start a background scanner thread.
    pub fn start_scanner(&mut self) -> Result<(), AccountError> {
        self.start_scan(ScanMode::Continuous)
    }

    /// Stop the background scanner thread.
    pub fn stop_scanner(&mut self) {
        self.stop_scan()
    }

    /// Check if the scanner is currently running.
    pub fn scanner_running(&self) -> bool {
        self.is_scanning()
    }

    /// Returns the last scanned block height.
    pub fn last_scanned_height(&self) -> Option<u32> {
        self.scan_state
            .lock()
            .expect("poisoned")
            .last_scanned_height()
    } // Sub-accounts

    /// Add a standard wallet sub-account (segwit, taproot, etc.).
    pub fn add_sub_account(&mut self, account: bwk::Account) {
        self.sub_accounts.push(account);
    }

    /// Returns a reference to the embedded sub-accounts.
    pub fn sub_accounts(&self) -> &[bwk::Account] {
        &self.sub_accounts
    }

    /// Returns a mutable reference to the embedded sub-accounts.
    pub fn sub_accounts_mut(&mut self) -> &mut [bwk::Account] {
        &mut self.sub_accounts
    }

    /// Total balance across SP and all sub-accounts.
    pub fn total_balance(&self) -> u64 {
        let sp_balance = self.balance();
        let bip32_balance: u64 = self.sub_accounts.iter().map(|a| a.balance().0).sum();
        sp_balance + bip32_balance
    } // Transaction Building

    /// Check if this account can sign transactions.
    ///
    /// Returns true if we have the spend secret key.
    pub fn can_sign(&self) -> bool {
        self.client.try_get_secret_spend_key().is_ok()
    }

    /// Returns a [`TxBuilder`] pre-configured with this account's coin source,
    /// change provider, and SP partial secret provider.
    ///
    /// Usage mirrors [`bwk::Account::tx_builder()`]:
    /// ```ignore
    /// let mut builder = account.tx_builder();
    /// builder.add_output(SpRecipient::new(sp_addr, 50_000, network));
    /// builder.feerate(1000);
    /// let mut psbt = builder.generate()?;
    /// account.sign_psbt(&mut psbt)?;
    /// ```
    pub fn tx_builder(&self) -> bwk_tx::TxBuilder {
        let change_addr = self.client.sp_receiver.get_change_address();
        let change_provider = Box::new(SpChangeRecipientProvider::new(
            change_addr,
            self.config.network,
        ));

        let sp_source = SpCoinSource::new(self.coin_store.clone());
        let sp_provider = Box::new(SpSecretProvider::new(
            self.coin_store.clone(),
            self.client.clone(),
        ));

        // Merge coin sources from all sub-accounts
        let bip32_sources: Vec<Box<dyn bwk_tx::CoinSource>> = self
            .sub_accounts
            .iter()
            .map(|a| Box::new(a.coin_source()) as Box<dyn bwk_tx::CoinSource>)
            .collect();
        let merged_source = Box::new(MergedCoinSource::new(sp_source, bip32_sources));

        bwk_tx::TxBuilder::new(change_provider)
            .coin_source(merged_source)
            .sp_provider(sp_provider)
    }

    /// Sign all inputs in a PSBT — both SP and BIP32 (segwit/taproot).
    ///
    /// 1. Signs SP inputs using `b_spend + tweak` (no taproot tweak).
    /// 2. Signs BIP32 inputs via sub-account SigningManagers.
    ///
    /// # Errors
    /// * `AccountError::NoKeys` if this account has no spend secret key
    /// * `AccountError::Transaction` on signing failure
    pub fn sign_psbt(&self, psbt: &mut bitcoin::Psbt) -> Result<(), AccountError> {
        // Sign SP inputs
        if self.can_sign() {
            self.sign_sp_inputs(psbt)?;
        }

        // Sign BIP32 inputs via sub-account SigningManagers
        for sub in &self.sub_accounts {
            sub.sign_psbt(psbt);
        }

        Ok(())
    }

    /// Sign all inputs and finalize the PSBT into a broadcast-ready transaction.
    ///
    /// 1. Signs SP inputs (`b_spend + tweak`).
    /// 2. Signs BIP32 inputs via sub-account SigningManagers.
    /// 3. Finalizes all inputs (builds witnesses) and extracts the transaction.
    pub fn sign_and_finalize(
        &self,
        psbt: &mut bitcoin::Psbt,
    ) -> Result<bitcoin::Transaction, AccountError> {
        self.sign_psbt(psbt)?;
        Self::finalize(psbt)
    }

    /// Finalize a signed PSBT into a broadcast-ready transaction.
    fn finalize(psbt: &mut bitcoin::Psbt) -> Result<bitcoin::Transaction, AccountError> {
        let secp = bitcoin::secp256k1::Secp256k1::verification_only();
        PsbtExt::finalize_mut(psbt, &secp).map_err(|errors| {
            AccountError::Transaction(format!("failed to finalize: {errors:?}"))
        })?;
        Ok(psbt.clone().extract_tx_unchecked_fee_rate())
    }

    /// Sign only the SP inputs in a PSBT.
    ///
    /// For each input whose outpoint is found in this account's coin store,
    /// computes the signing key (`b_spend + tweak`) and produces a Schnorr
    /// signature stored in `tap_key_sig`.
    fn sign_sp_inputs(&self, psbt: &mut bitcoin::Psbt) -> Result<(), AccountError> {
        let b_spend = self
            .client
            .try_get_secret_spend_key()
            .map_err(|e| AccountError::Transaction(e.to_string()))?;

        let secp = Secp256k1::new();
        let hash_ty = TapSighashType::Default;

        let prevouts: Vec<bitcoin::TxOut> = psbt
            .inputs
            .iter()
            .map(|input| {
                input
                    .witness_utxo
                    .clone()
                    .expect("PSBT input must have witness_utxo")
            })
            .collect();

        let mut cache = SighashCache::new(&psbt.unsigned_tx);
        let coin_store = self.coin_store.lock().expect("poisoned");

        let mut aux_rand = [0u8; 32];
        getrandom::getrandom(&mut aux_rand)
            .map_err(|e| AccountError::Transaction(format!("random bytes: {e}")))?;

        for (i, input) in psbt.unsigned_tx.input.iter().enumerate() {
            let Some(entry) = coin_store.get(&input.previous_output) else {
                // Not an SP input, skip
                continue;
            };

            let sighash = cache
                .taproot_key_spend_signature_hash(i, &Prevouts::All(&prevouts), hash_ty)
                .map_err(|e| AccountError::Transaction(format!("sighash: {e}")))?;

            let msg = Message::from_digest(sighash.to_byte_array());
            let tweak = SecretKey::from_slice(entry.tweak())
                .map_err(|e| AccountError::Transaction(format!("tweak: {e}")))?;
            let sk = b_spend
                .add_tweak(&tweak.into())
                .map_err(|e| AccountError::Transaction(format!("add_tweak: {e}")))?;

            let keypair = Keypair::from_secret_key(&secp, &sk);
            // SP outputs use dangerous_assume_tweaked() — no taproot tweak on the
            // output key, so sign with the untweaked keypair directly.
            let sig = secp.sign_schnorr_with_aux_rand(&msg, &keypair, &aux_rand);

            psbt.inputs[i].tap_key_sig = Some(Signature {
                signature: sig,
                sighash_type: hash_ty,
            });
        }

        Ok(())
    } // Persistence

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
        self.stop_scan();
        self.persist();
    }
}

// AccountUpdater - Implements Updater trait for Account

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
        log::debug!(
            "record_scan_progress: current={}, end={}",
            current.to_consensus_u32(),
            end.to_consensus_u32()
        );

        // Update scan state with current progress
        {
            let mut state = self.scan_state.lock().expect("poisoned");
            state.set_last_scanned_height(current.to_consensus_u32());
            state.persist();
        }

        // Send progress notification every 100 blocks, and always for the last block
        let current_u32 = current.to_consensus_u32();
        let end_u32 = end.to_consensus_u32();
        if current_u32 % 100 == 0 || current_u32 == end_u32 {
            let _ = self
                .sender
                .send(Notification::Sp(SpNotification::ScanProgress {
                    current: current_u32,
                    end: end_u32,
                }));
        }
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
                let _ = self
                    .sender
                    .send(Notification::Sp(SpNotification::NewOutput(outpoint)));
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
                let _ = self
                    .sender
                    .send(Notification::Sp(SpNotification::OutputSpent(outpoint)));
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

    fn restore_owned_outpoints(&self) -> Result<HashSet<OutPoint>, spdk_core::Error> {
        let store = self.coin_store.lock().expect("poisoned");
        Ok(store.all_outpoints())
    }
}

/// Get backend info without an Account.
///
/// If the URL already has a scheme, uses it directly. Otherwise tries `http://` then `https://`.
/// Returns the `InfoResponse` and the URL that worked.
pub fn backend_info(blindbit_url: String) -> Result<(InfoResponse, String), AccountError> {
    let try_url = |url: &str| -> Result<InfoResponse, AccountError> {
        let http_client = UreqClient::new();
        let backend = BlindbitBackend::new(url.to_string(), http_client)
            .map_err(|e| AccountError::Network(format!("failed to create backend: {}", e)))?;
        backend
            .info()
            .map_err(|e| AccountError::Network(e.to_string()))
    };

    // If a scheme is already present, use as-is.
    if blindbit_url.starts_with("https://") || blindbit_url.starts_with("http://") {
        let info = try_url(&blindbit_url)?;
        return Ok((info, blindbit_url));
    }

    // No scheme — try http:// then https://.
    let http_url = format!("http://{}", blindbit_url);
    if let Ok(info) = try_url(&http_url) {
        return Ok((info, http_url));
    }

    let https_url = format!("https://{}", blindbit_url);
    let info = try_url(&https_url)?;
    Ok((info, https_url))
}

/// Get block height without an Account.
pub fn backend_block_height(blindbit_url: String) -> Result<u32, AccountError> {
    let http_client = UreqClient::new();
    let backend = BlindbitBackend::new(blindbit_url, http_client)
        .map_err(|e| AccountError::Network(format!("failed to create backend: {}", e)))?;
    backend
        .block_height()
        .map(|h| h.to_consensus_u32())
        .map_err(|e| AccountError::Network(e.to_string()))
}

// Tests

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

        let err = AccountError::ScannerAlreadyRunning;
        assert!(err.to_string().contains("already running"));
    }

    #[test]
    fn test_notification_variants() {
        let notif = Notification::Sp(SpNotification::StartingScan);
        assert!(matches!(
            notif,
            Notification::Sp(SpNotification::StartingScan)
        ));

        let notif = Notification::Sp(SpNotification::FailStartScanning {
            message: "test error".to_string(),
        });
        if let Notification::Sp(SpNotification::FailStartScanning { message }) = notif {
            assert_eq!(message, "test error");
        } else {
            panic!("expected FailStartScanning");
        }

        let notif = Notification::Sp(SpNotification::FailScan {
            message: "scan error".to_string(),
        });
        if let Notification::Sp(SpNotification::FailScan { message }) = notif {
            assert_eq!(message, "scan error");
        } else {
            panic!("expected FailScan");
        }

        let notif = Notification::Sp(SpNotification::StoppingScan);
        assert!(matches!(
            notif,
            Notification::Sp(SpNotification::StoppingScan)
        ));

        let notif = Notification::Sp(SpNotification::ScanStopped);
        assert!(matches!(
            notif,
            Notification::Sp(SpNotification::ScanStopped)
        ));

        let notif = Notification::Sp(SpNotification::ScanProgress {
            current: 100,
            end: 200,
        });
        if let Notification::Sp(SpNotification::ScanProgress { current, end }) = notif {
            assert_eq!(current, 100);
            assert_eq!(end, 200);
        } else {
            panic!("expected ScanProgress");
        }

        let notif = Notification::Sp(SpNotification::ScanCompleted);
        assert!(matches!(
            notif,
            Notification::Sp(SpNotification::ScanCompleted)
        ));
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

    #[test]
    fn test_scan_progress_throttling() {
        let scan_state = Arc::new(Mutex::new(ScanState::new(0)));
        let coin_store = Arc::new(Mutex::new(SpCoinStore::new()));
        let tx_store = Arc::new(Mutex::new(SpTxStore::new()));
        let (sender, receiver) = mpsc::channel();

        let mut updater = AccountUpdater {
            coin_store,
            tx_store,
            scan_state,
            sender,
        };

        // Helper to collect all ScanProgress current values from the channel
        let drain = |rx: &mpsc::Receiver<Notification>| -> Vec<u32> {
            let mut v = Vec::new();
            while let Ok(notif) = rx.try_recv() {
                if let Notification::Sp(SpNotification::ScanProgress { current, .. }) = notif {
                    v.push(current);
                }
            }
            v
        };

        // --- Range 1: blocks 0..=350 (end=350) ---
        // Expected notifications: 0, 100, 200, 300, 350 (last block)
        let end = Height::from_consensus(350).unwrap();
        for h in 0..=350u32 {
            let current = Height::from_consensus(h).unwrap();
            updater
                .record_scan_progress(Height::from_consensus(0).unwrap(), current, end)
                .unwrap();
        }
        let notified = drain(&receiver);
        assert_eq!(notified, vec![0, 100, 200, 300, 350]);

        // --- Range 2: blocks 351..=400 (end=400) ---
        // Expected: 400 (both %100 and last)
        let end = Height::from_consensus(400).unwrap();
        for h in 351..=400u32 {
            let current = Height::from_consensus(h).unwrap();
            updater
                .record_scan_progress(Height::from_consensus(351).unwrap(), current, end)
                .unwrap();
        }
        let notified = drain(&receiver);
        assert_eq!(notified, vec![400]);

        // --- Range 3: blocks 401..=410 (end=410, short range, no %100 hit) ---
        // Expected: 410 (last block only)
        let end = Height::from_consensus(410).unwrap();
        for h in 401..=410u32 {
            let current = Height::from_consensus(h).unwrap();
            updater
                .record_scan_progress(Height::from_consensus(401).unwrap(), current, end)
                .unwrap();
        }
        let notified = drain(&receiver);
        assert_eq!(notified, vec![410]);

        // --- Range 4: blocks 411..=700 (end=700, end is %100) ---
        // Expected: 500, 600, 700 (700 is both %100 and last — should appear once)
        let end = Height::from_consensus(700).unwrap();
        for h in 411..=700u32 {
            let current = Height::from_consensus(h).unwrap();
            updater
                .record_scan_progress(Height::from_consensus(411).unwrap(), current, end)
                .unwrap();
        }
        let notified = drain(&receiver);
        assert_eq!(notified, vec![500, 600, 700]);
    }
}
