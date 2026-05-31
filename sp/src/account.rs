//! Main Account orchestrator for Silent Payment wallets.
//!
//! The `Account` struct ties together all components of a Silent Payment wallet:
//! - SpClient for key management and address derivation
//! - BlindbitBackend for blockchain data access
//! - Stores for coins, transactions, labels, and scan state
//! - Background scanning thread for continuous blockchain monitoring

use std::collections::{BTreeMap, HashMap, HashSet};
use std::str::FromStr;
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
use bwk::label_store::LabelStore;
use miniscript::psbt::PsbtExt;
use silentpayments::SilentPaymentAddress;

use backend_blindbit_native_non_async::{BlindbitBackend, InfoResponse, UreqClient};
use spdk_core::account::SpAccount;
use spdk_core::{bip39, OwnedOutput, SpClient, SpScanner, Updater};

use bwk::persist::{ConfigStore, NoopConfigStore};

use crate::{
    coin_store::{KeyedBip32Source, MergedCoinSource, SpCoinSource},
    recipient::{SpChangeRecipientProvider, SpSecretProvider},
    CoinState, Config, LabelKey, ScanState, SpCoinEntry, SpCoinStore, SpTxEntry, SpTxStore,
};

// Type Aliases

/// Type alias for the tuple of stores returned by create_or_load_stores.
type Stores = (
    Arc<Mutex<SpCoinStore>>,
    Arc<Mutex<LabelStore>>,
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

/// Main orchestrator for a Silent Payment wallet account. Generic
/// over any [`crate::profile::SpStorageProfile`]; defaults to
/// [`crate::profile::SpRamProfile<DefaultBackend>`].
pub struct Account<
    P: crate::profile::SpStorageProfile = crate::profile::SpRamProfile<
        crate::profile::DefaultBackend,
    >,
> {
    client: SpClient,
    backend: BlindbitBackend<UreqClient>,
    coin_store: Arc<Mutex<SpCoinStore<P>>>,
    label_store: Arc<Mutex<LabelStore>>,
    tx_store: Arc<Mutex<SpTxStore<P>>>,
    scan_state: Arc<Mutex<ScanState>>,
    config: Config,
    /// Persistence sink for `config`. [`NoopConfigStore`] by default.
    /// Consumers wire whatever shape suits them — a
    /// [`bwk::persist::FileConfigStore`] for file-backed persistence, a
    /// [`bwk::persist::CallbackConfigStore`] to bridge save/load through
    /// host-supplied closures, or any other [`ConfigStore`] impl.
    config_store: Arc<dyn ConfigStore<Config>>,
    sender: mpsc::Sender<Notification>,
    receiver: Option<mpsc::Receiver<Notification>>,
    scanner_handle: Option<JoinHandle<()>>,
    scanner_stop: Arc<AtomicBool>,
    // Sub-accounts use the default bwk RAM profile — independent of sp's P.
    sub_accounts: Vec<bwk::Account>,
}

// Constructors are tied to the default SpRamProfile because they open
// concrete on-disk RamStore instances.
impl Account<crate::profile::SpRamProfile<crate::profile::DefaultBackend>> {
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
        Self::with_config_store(config, Arc::new(NoopConfigStore::<Config>::default()))
    }

    /// Like [`Account::new`] but with an explicit config store
    /// ([`bwk::persist::FileConfigStore`] for file-backed,
    /// [`bwk::persist::CallbackConfigStore`] to bridge through
    /// caller-supplied closures, or any other [`ConfigStore`]).
    pub fn with_config_store(
        config: Config,
        config_store: Arc<dyn ConfigStore<Config>>,
    ) -> Result<Self, AccountError> {
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
        let backend = create_backend(&config)?;

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
                let bwk_config = bwk::Config {
                    data_dir: config.account_dir(),
                    dir_name: format!("{}-sub-{}", config.account_name, i),
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
                    mnemonic: sub_cfg.mnemonic.clone().or_else(|| config.mnemonic.clone()),
                    descriptor: sub_cfg.descriptor.clone(),
                    persist: config.persist,
                    skip_labels: true,
                    persist_kind: config.persist_kind,
                };
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
            config_store,
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
                .map_err(|e| AccountError::Config(format!("invalid mnemonic: {e}")))?;
            SpClient::new_from_mnemonic(mnemonic, config.network)
                .map_err(|e| AccountError::Config(format!("failed to create SpClient: {e}")))
        } else if let Some(ref scan_sk_hex) = config.scan_sk {
            // Create from raw keys
            let scan_sk_bytes = hex::decode(scan_sk_hex)
                .map_err(|e| AccountError::Config(format!("invalid scan_sk hex: {e}")))?;
            let scan_sk = bitcoin::secp256k1::SecretKey::from_slice(&scan_sk_bytes)
                .map_err(|e| AccountError::Config(format!("invalid scan_sk: {e}")))?;

            let spend_key = if let Some(ref spend_key_hex) = config.spend_key {
                let spend_key_bytes = hex::decode(spend_key_hex)
                    .map_err(|e| AccountError::Config(format!("invalid spend_key hex: {e}")))?;

                if spend_key_bytes.len() == 32 {
                    // Secret key
                    let sk = bitcoin::secp256k1::SecretKey::from_slice(&spend_key_bytes)
                        .map_err(|e| AccountError::Config(format!("invalid spend_key: {e}")))?;
                    spdk_core::SpendKey::Secret(sk)
                } else if spend_key_bytes.len() == 33 {
                    // Public key
                    let pk = bitcoin::secp256k1::PublicKey::from_slice(&spend_key_bytes)
                        .map_err(|e| AccountError::Config(format!("invalid spend_key: {e}")))?;
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
                .map_err(|e| AccountError::Config(format!("failed to create SpClient: {e}")))
        } else {
            Err(AccountError::Config(
                "either mnemonic or scan_sk must be provided".to_string(),
            ))
        }
    }

    // (see the free `create_backend` helper below for the backend
    // constructor — it's pure w.r.t. `P`.)

    /// Create or load stores based on config.persist + persist_kind.
    ///
    /// Builds a single backend ([`bwk::persist::JsonBackend`] or
    /// [`bwk::persist::SqliteBackend`]) from the config and threads it into
    /// every store. JSON layout is byte-for-byte equivalent to the
    /// pre-backend layout (one `{store}.json` file per store name).
    fn create_or_load_stores(config: &Config) -> Stores {
        let birthday = config
            .birthday_height
            .unwrap_or_else(|| config.min_birthday_height());

        let backend: Arc<dyn bwk::persist::PersistenceBackend> = match bwk::persist::build_backend(
            config.persist.then_some(config.persist_kind),
            config.account_dir(),
        ) {
            Ok(b) => b,
            Err(e) => {
                log::error!(
                    "create_or_load_stores: failed to build persistence backend ({e}); \
                         falling back to no-op"
                );
                Arc::new(bwk::persist::NoopBackend)
            }
        };

        let coin_store =
            SpCoinStore::load_from_backend(backend.clone(), crate::coin_store::STORE_KEY);
        let label_store =
            LabelStore::load_from_backend(backend.clone(), bwk::persist::LABELS_STORE_KEY);
        let tx_store = SpTxStore::load_from_backend(backend.clone(), crate::tx_store::STORE_KEY);
        let scan_state = ScanState::load_from_backend(birthday, backend);

        (
            Arc::new(Mutex::new(coin_store)),
            Arc::new(Mutex::new(label_store)),
            Arc::new(Mutex::new(tx_store)),
            Arc::new(Mutex::new(scan_state)),
        )
    }
}

// Generic accessors and operations — available for any `P: SpStorageProfile`.
impl<P: crate::profile::SpStorageProfile> Account<P> {
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
        self.coin_store.lock().expect("poisoned").get(outpoint)
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
        self.tx_store.lock().expect("poisoned").transactions()
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
                        .outpoint(*outpoint)
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
                        .transaction(td.txid)
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
        let value = (!label.is_empty()).then_some(label);
        store.edit(LabelKey::OutPoint(outpoint), value);
        store.persist();
    }

    /// Update the label for a transaction.
    pub fn update_tx_label(&self, txid: Txid, label: String) {
        let mut store = self.label_store.lock().expect("poisoned");
        let value = (!label.is_empty()).then_some(label);
        store.edit(LabelKey::Transaction(txid), value);
        store.persist();
    }

    /// Get the label for a coin.
    pub fn get_coin_label(&self, outpoint: &OutPoint) -> Option<String> {
        self.label_store
            .lock()
            .expect("poisoned")
            .outpoint(*outpoint)
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
        if let Ok(backend) = create_backend(&self.config) {
            self.backend = backend;
        }
        if self.config.persist {
            self.persist_config();
        }
    }

    /// Push the current config to the configured [`ConfigStore`].
    ///
    /// Under [`bwk::persist::PersistenceKind::Sqlite`] the saved view has
    /// signer material stripped via [`Config::for_persistence`].
    fn persist_config(&self) {
        if let Err(e) = self.config_store.save(&self.config.for_persistence()) {
            log::warn!("config save failed: {e}");
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
        // Clear any stale cancel signal from a previous run before we hand
        // the flag down to the scanner. Without this, a caller that flipped
        // the flag via `cancel_flag()` for a prior scan would cause the next
        // OneShot to bail at the first block (spdk-core's `process_blocks`
        // returns Ok early when `should_interrupt()` is true).
        self.scanner_stop.store(false, Ordering::Relaxed);

        let start_height = self.scan_state.lock().expect("poisoned").next_scan_start();
        let end_height = self.block_height()?;

        if start_height > end_height {
            return Ok(()); // Already at tip, nothing to scan
        }

        let start = Height::from_consensus(start_height)
            .map_err(|e| AccountError::Scan(format!("invalid start height: {e}")))?;
        let end = Height::from_consensus(end_height)
            .map_err(|e| AccountError::Scan(format!("invalid end height: {e}")))?;

        let dust_limit = self.config.dust_limit.map(Amount::from_sat);
        let scan_backend = create_backend(&self.config)?;

        let with_cutthrough = scan_backend
            .info()
            .map(|info| info.tweaks_cut_through_with_dust_filter)
            .unwrap_or(false);

        let mut scanner = SpAccount::new(
            scan_backend,
            self.client.clone(),
            AccountUpdater::<P> {
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
                AccountUpdater::<P> {
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
                        log::warn!("scanner: failed to get block height: {e}");
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

    /// Returns a clone of the scanner cancellation flag.
    ///
    /// Setting this `AtomicBool` to `true` causes any in-flight OneShot or
    /// Continuous scan to bail at the next per-block checkpoint inside
    /// spdk-core's `process_blocks` (which calls `should_interrupt()` before
    /// every block). The scan call returns `Ok(())` after persisting state
    /// , i.e. cancellation is graceful, not an error.
    ///
    /// `scan_oneshot` resets this flag to `false` at the start of each run,
    /// so leaving the flag in `true` between runs is harmless.
    ///
    /// Intended for consumers that hold an `Account` behind a `Mutex` and
    /// need to interrupt a scan without first re-acquiring the mutex (which
    /// the in-flight scan call still holds via `&mut self`).
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.scanner_stop.clone()
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
            .map_err(|e| AccountError::Scan(format!("invalid start height: {e}")))?;
        let end = Height::from_consensus(end_height)
            .map_err(|e| AccountError::Scan(format!("invalid end height: {e}")))?;

        let dust_limit = self.config.dust_limit.map(Amount::from_sat);
        let scan_backend = create_backend(&self.config)?;

        let with_cutthrough = scan_backend
            .info()
            .map(|info| info.tweaks_cut_through_with_dust_filter)
            .unwrap_or(false);

        let mut scanner = SpAccount::new(
            scan_backend,
            self.client.clone(),
            AccountUpdater::<P> {
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

    /// Derive the BIP32 master xpriv from this account's mnemonic, if available.
    pub(crate) fn sp_master_xpriv(
        &self,
    ) -> Option<(bitcoin::bip32::Fingerprint, bitcoin::bip32::Xpriv)> {
        let mnemonic_str = self.config.mnemonic.as_ref()?;
        let mnemonic = bip39::Mnemonic::from_str(mnemonic_str).ok()?;
        let seed = mnemonic.to_seed("");
        let xpriv = bitcoin::bip32::Xpriv::new_master(self.config.network, &seed).ok()?;
        let secp = Secp256k1::new();
        let fg = xpriv.fingerprint(&secp);
        Some((fg, xpriv))
    }

    /// Stop electrum on all sub-accounts.
    pub fn stop_electrum(&mut self) {
        for sub in &mut self.sub_accounts {
            sub.stop_electrum();
        }
    }

    /// Start electrum on all sub-accounts.
    pub fn start_electrum(&mut self) {
        for sub in &mut self.sub_accounts {
            sub.start_electrum();
        }
    }

    /// Set electrum URL and port on all sub-accounts without writing to file.
    pub fn set_electrum_settings(&mut self, url: Option<String>, port: Option<u16>) {
        for sub in &mut self.sub_accounts {
            sub.set_electrum_config(url.clone(), port);
        }
    }

    /// Total balance across SP and all sub-accounts.
    pub fn total_balance(&self) -> u64 {
        let sp_balance = self.balance();
        let bip32_balance: u64 = self.sub_accounts.iter().map(|a| a.balance().0).sum();
        sp_balance + bip32_balance
    }

    /// All coins from the SP account and every sub-account, keyed by outpoint.
    ///
    /// Spent and being-spent coins are included; filter by
    /// [`UnifiedCoin::spendable`](crate::UnifiedCoin::spendable) to keep only
    /// live UTXOs.
    pub fn all_coins(&self) -> BTreeMap<OutPoint, crate::UnifiedCoin> {
        use crate::{CoinOrigin, UnifiedCoin};
        let mut out: BTreeMap<OutPoint, UnifiedCoin> = BTreeMap::new();

        for (outpoint, entry) in self.coins() {
            out.insert(
                outpoint,
                UnifiedCoin {
                    origin: CoinOrigin::Sp,
                    outpoint,
                    amount: Amount::from_sat(entry.amount_sat()),
                    height: Some(entry.height()),
                    spendable: entry.is_spendable(),
                    label: self.get_coin_label(&outpoint),
                },
            );
        }

        for (sub_idx, sub) in self.sub_accounts.iter().enumerate() {
            for (outpoint, entry) in sub.coins() {
                let spendable = !matches!(
                    entry.status(),
                    bwk_tx::CoinStatus::Spent | bwk_tx::CoinStatus::BeingSpend,
                );
                out.insert(
                    outpoint,
                    UnifiedCoin {
                        origin: CoinOrigin::SubAccount(sub_idx),
                        outpoint,
                        amount: entry.coin.txout.value,
                        height: entry.height().map(|h| h as u32),
                        spendable,
                        label: self.get_coin_label(&outpoint),
                    },
                );
            }
        }

        out
    }

    /// Build a configured `TxBuilder` from a [`bwk_tx::TxRequest`].
    ///
    /// The returned builder has the request's outputs added, fee policy set,
    /// and inputs selected per the rules: manual outpoints when supplied,
    /// drain when any output sets `max`, otherwise auto-select.
    pub fn tx_builder_from_request(
        &self,
        request: &bwk_tx::TxRequest,
    ) -> Result<bwk_tx::TxBuilder, bwk_tx::TxRequestError> {
        use crate::recipient::SpRecipientAddress;
        use bwk_tx::{Amount as BwkAmount, TxRequestError};
        use spdk_core::RecipientAddress;

        let network = self.network();

        let mut has_max = false;
        let mut max_addr: Option<RecipientAddress> = None;
        let mut recipients: Vec<SpRecipientAddress> = Vec::new();

        for output in &request.outputs {
            let addr = RecipientAddress::try_from(output.address.clone()).map_err(|e| {
                TxRequestError::InvalidAddress {
                    address: output.address.clone(),
                    source: Box::new(e),
                }
            })?;
            if output.max {
                if has_max {
                    return Err(TxRequestError::MultipleMaxOutputs);
                }
                has_max = true;
                max_addr = Some(addr);
            } else {
                recipients.push(SpRecipientAddress::new(addr, output.amount, network));
            }
        }

        let feerate_msats_vb = (request.fee_rate.max(1.0) * 1000.0) as u64;
        let mut builder = self.tx_builder();
        builder = if request.fee > 0 {
            builder.fee(request.fee)
        } else {
            builder.feerate(feerate_msats_vb)
        };

        for recip in recipients {
            builder.add_output(recip);
        }
        if has_max {
            let mut recip = SpRecipientAddress::new(max_addr.unwrap(), 0, network);
            recip.amount = BwkAmount::Max(None);
            builder.add_output(recip);
        }

        // Caller-specified inputs are pre-added (this bypasses auto-selection).
        // When none are given, leave the template inputs empty: the builder's
        // registered coin selector + source auto-select on simulate()/generate()
        // (Value outputs select for the target; a Max output sweeps all coins).
        if !request.input_outpoints.is_empty() {
            let sp_coins = self.coins();
            for outpoint in &request.input_outpoints {
                if let Some(entry) = sp_coins.get(outpoint) {
                    if !entry.is_spendable() {
                        return Err(TxRequestError::CoinNotSpendable(*outpoint));
                    }
                    builder.add_input(sp_coin_entry_to_coin(*outpoint, entry));
                    continue;
                }
                let mut found = false;
                for sub in self.sub_accounts() {
                    if let Some(entry) = sub.coins().get(outpoint) {
                        if matches!(
                            entry.status(),
                            bwk_tx::CoinStatus::Spent | bwk_tx::CoinStatus::BeingSpend
                        ) {
                            return Err(TxRequestError::CoinNotSpendable(*outpoint));
                        }
                        builder.add_input(entry.coin.clone());
                        found = true;
                        break;
                    }
                }
                if !found {
                    return Err(TxRequestError::CoinNotFound(*outpoint));
                }
            }
        }

        Ok(builder)
    }

    /// Simulate a transaction described by a [`bwk_tx::TxRequest`].
    pub fn simulate(
        &self,
        request: &bwk_tx::TxRequest,
    ) -> Result<bwk_tx::TxSimulation, bwk_tx::TxRequestError> {
        let builder = self.tx_builder_from_request(request)?;
        // Inputs auto-selected from the builder's registered selector/source
        // when the request did not specify input_outpoints.
        let mut result = builder.simulate();
        if let Some(err) = result.error.take() {
            return Err(err.into());
        }
        let weight = bwk_tx::transaction::tx_estimated_weight(&result.tx_template);
        let input_total: bitcoin::Amount = result
            .tx_template
            .inputs
            .iter()
            .map(|c| c.txout.value)
            .sum();
        let fee = result.fees.unwrap_or(bitcoin::Amount::ZERO);
        let output_total = input_total
            .checked_sub(fee)
            .unwrap_or(bitcoin::Amount::ZERO);
        let selected_outpoints = result
            .tx_template
            .inputs
            .iter()
            .map(|c| c.outpoint)
            .collect();
        Ok(bwk_tx::TxSimulation {
            fee,
            weight,
            input_total,
            output_total,
            selected_outpoints,
        })
    }

    /// Build an unsigned PSBT from a [`bwk_tx::TxRequest`].
    pub fn prepare(
        &self,
        request: &bwk_tx::TxRequest,
    ) -> Result<bitcoin::Psbt, bwk_tx::TxRequestError> {
        let mut builder = self.tx_builder_from_request(request)?;
        builder.generate().map_err(Into::into)
    }

    /// SP spendable summary plus every sub-account's spendable summary, summed.
    pub fn all_spendable_coins(&self) -> crate::SpendableSummary {
        use crate::SpendableSummary;
        let sp = self.spendable_coins();
        let mut summary = SpendableSummary {
            confirmed_count: sp.confirmed_coins as u64,
            confirmed_balance: Amount::from_sat(sp.confirmed_balance),
            unconfirmed_count: sp.unconfirmed_coins as u64,
            unconfirmed_balance: Amount::from_sat(sp.unconfirmed_balance),
        };
        for sub in &self.sub_accounts {
            let s = sub.spendable_coins();
            summary.confirmed_count += s.confirmed_coins as u64;
            summary.confirmed_balance += Amount::from_sat(s.confirmed_balance);
            summary.unconfirmed_count += s.unconfirmed_coins as u64;
            summary.unconfirmed_balance += Amount::from_sat(s.unconfirmed_balance);
        }
        summary
    }
    // Transaction Building

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

        // Collect master xprivs from SP account and all sub-accounts for BIP32 key derivation
        let mut all_xprivs = std::collections::BTreeMap::new();
        if let Some((fg, xpriv)) = self.sp_master_xpriv() {
            all_xprivs.insert(fg, xpriv);
        }
        for sub in &self.sub_accounts {
            all_xprivs.extend(sub.master_xprivs());
        }

        let sp_provider = Box::new(SpSecretProvider::new(
            self.coin_store.clone(),
            self.client.clone(),
            all_xprivs.clone(),
        ));

        // Merge coin sources from all sub-accounts, enriching BIP32 coins
        // with their secret keys for SP partial secret computation.
        let bip32_sources: Vec<Box<dyn bwk_tx::CoinSource>> = self
            .sub_accounts
            .iter()
            .map(|a| {
                Box::new(KeyedBip32Source::new(
                    Box::new(a.coin_source()),
                    a.master_xprivs(),
                )) as Box<dyn bwk_tx::CoinSource>
            })
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

    /// Aggregated view of every address this wallet owns, across
    /// every BIP32 sub-account plus the SP-derived taproot spks the
    /// SP coin store has seen. Each entry is tagged with the name
    /// of the account it belongs to (BIP32 sub-account name, or
    /// this SP account's name for SP-derived spks). Useful to:
    ///   - list all owned addresses in a UI;
    ///   - sanity-check before exporting / sending: refuse to expose
    ///     or self-target an address whose status is `Used` /
    ///     `Reused`.
    pub fn owned_addresses(&self) -> Vec<OwnedAddress> {
        let mut out: Vec<OwnedAddress> = Vec::new();

        for sub in &self.sub_accounts {
            let name = sub.name();
            for e in sub.address_entries() {
                out.push(OwnedAddress {
                    address: e.value(),
                    account_name: name.clone(),
                    source: AddressSource::Bip32(e.account(), e.index()),
                    status: e.status(),
                    funding_txids: e.funding_txids().clone(),
                    spending_txids: e.spending_txids().clone(),
                });
            }
        }

        let sp_name = self.name().to_string();
        let network = self.config.network;
        let sp_entries = self
            .coin_store
            .lock()
            .expect("poisoned")
            .addresses_with_status();
        for e in sp_entries {
            let address = bitcoin::Address::from_script(&e.script, network)
                .map(|a| a.to_string())
                .unwrap_or_else(|_| e.script.to_hex_string());
            // SP-derived spks have exactly one funding tx by
            // construction (per-output tweak makes them unique). If
            // the set is somehow empty, fall back to Unknown rather
            // than silently dropping the entry.
            let source = match e.funding_txids.iter().next().copied() {
                Some(txid) => AddressSource::SilentPayment(txid),
                None => AddressSource::Unknown,
            };
            out.push(OwnedAddress {
                address,
                account_name: sp_name.clone(),
                source,
                status: e.status,
                funding_txids: e.funding_txids,
                spending_txids: e.spending_txids,
            });
        }

        out
    }

    /// Sanity-check API: look `address` up in the wallet's
    /// aggregated owned-address view. Returns the entry with status
    /// and funding/spending txids if it belongs to this wallet, or
    /// `None` otherwise. Compares by canonical bech32/bech32m string.
    ///
    /// Consumer policy decides what to do with a `Used` / `Reused`
    /// hit (typical: refuse to re-export or self-target). The wallet
    /// just answers "is this mine, and if so what's its status".
    pub fn lookup_owned_address(&self, address: &str) -> Option<OwnedAddress> {
        self.owned_addresses()
            .into_iter()
            .find(|o| o.address == address)
    }
}

/// Provenance of an [`OwnedAddress`].
///
/// SP-derived spks have exactly one funding tx by protocol design
/// (each output's per-tweak ensures uniqueness), so
/// [`AddressSource::SilentPayment`] carries that single funding txid.
/// BIP32 sub-account addresses carry their keychain (receive vs
/// change) and derivation index. [`AddressSource::Unknown`] is a
/// defensive fallback for cases where neither shape applies.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AddressSource {
    SilentPayment(bitcoin::Txid),
    Bip32(bwk_tx::coin::KeyChain, u32),
    Unknown,
}

/// One address the wallet owns, aggregated across sub-accounts
/// (BIP32) and the SP wallet. `account_name` identifies the
/// originating keychain — see [`bwk::Account::name`] /
/// [`crate::Account::name`]. `source` carries the per-spk provenance
/// (see [`AddressSource`]).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OwnedAddress {
    pub address: String,
    pub account_name: String,
    pub source: AddressSource,
    pub status: bwk::address_store::AddressStatus,
    pub funding_txids: std::collections::BTreeSet<bitcoin::Txid>,
    pub spending_txids: std::collections::BTreeSet<bitcoin::Txid>,
}

impl<P: crate::profile::SpStorageProfile> Drop for Account<P> {
    fn drop(&mut self) {
        self.stop_scan();
        self.persist();
    }
}

// AccountUpdater - Implements Updater trait for Account

/// Create a [`BlindbitBackend`] from the config's URL. Pure w.r.t.
/// storage strategy — shared across all generic impls.
fn create_backend(config: &Config) -> Result<BlindbitBackend<UreqClient>, AccountError> {
    let http_client = UreqClient::new();
    BlindbitBackend::new(config.blindbit_url.clone(), http_client)
        .map_err(|e| AccountError::Network(format!("failed to create backend: {e}")))
}

/// Internal struct that implements the Updater trait for scanning.
struct AccountUpdater<P: crate::profile::SpStorageProfile> {
    coin_store: Arc<Mutex<SpCoinStore<P>>>,
    tx_store: Arc<Mutex<SpTxStore<P>>>,
    scan_state: Arc<Mutex<ScanState>>,
    sender: mpsc::Sender<Notification>,
}

impl<P: crate::profile::SpStorageProfile> Updater for AccountUpdater<P> {
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
            .map_err(|e| AccountError::Network(format!("failed to create backend: {e}")))?;
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
    let http_url = format!("http://{blindbit_url}");
    if let Ok(info) = try_url(&http_url) {
        return Ok((info, http_url));
    }

    let https_url = format!("https://{blindbit_url}");
    let info = try_url(&https_url)?;
    Ok((info, https_url))
}

/// Get block height without an Account.
pub fn backend_block_height(blindbit_url: String) -> Result<u32, AccountError> {
    let http_client = UreqClient::new();
    let backend = BlindbitBackend::new(blindbit_url, http_client)
        .map_err(|e| AccountError::Network(format!("failed to create backend: {e}")))?;
    backend
        .block_height()
        .map(|h| h.to_consensus_u32())
        .map_err(|e| AccountError::Network(e.to_string()))
}

/// Build a [`bwk_tx::Coin`] from an SP outpoint + entry, suitable for
/// `TxBuilder::add_input`. The taproot key-spend satisfaction weight
/// ([`bwk_tx::TAPROOT_KEYSPEND_SATISFACTION_WU`]) is used because SP outputs
/// are always single-key taproot.
pub fn sp_coin_entry_to_coin(outpoint: OutPoint, entry: &SpCoinEntry) -> bwk_tx::Coin {
    bwk_tx::Coin {
        txout: bitcoin::TxOut {
            value: entry.amount(),
            script_pubkey: entry.script().clone(),
        },
        outpoint,
        height: Some(entry.height() as u64),
        sequence: bitcoin::Sequence::ENABLE_RBF_NO_LOCKTIME,
        status: bwk_tx::CoinStatus::Confirmed,
        label: None,
        satisfaction_size: bwk_tx::TAPROOT_KEYSPEND_SATISFACTION_WU,
        spend_info: bwk_tx::CoinSpendInfo::Sp {
            derivation: bitcoin::bip32::DerivationPath::default(),
            tweak: *entry.tweak(),
        },
    }
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

        let debug_str = format!("{payment:?}");
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

        let mut updater =
            AccountUpdater::<crate::profile::SpRamProfile<crate::profile::DefaultBackend>> {
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

    // -----------------------------------------------------------------
    // owned_addresses — aggregate view across BIP32 subs + SP
    // -----------------------------------------------------------------

    fn build_offline_segwit_sub(name: &str) -> bwk::Account {
        use bip39::Mnemonic;
        use bwk_sign::bwk_descriptor;
        use bwk_sign::HotSigner;
        use miniscript::bitcoin::bip32::ChildNumber;

        let network = bitcoin::Network::Regtest;
        let mnemo = Mnemonic::generate(12).unwrap();
        let signer = HotSigner::new_from_mnemonics(network, &mnemo.to_string()).unwrap();
        let path =
            bwk_descriptor::wpkh_path(network, ChildNumber::from_hardened_idx(0).unwrap()).unwrap();
        let xpub = signer.xpub(&path);
        let descriptor = bwk_descriptor::SpkDerivator::new_wpkh(xpub, network)
            .unwrap()
            .descriptor();
        bwk::Account::new(bwk::Config {
            data_dir: std::path::PathBuf::new(),
            dir_name: String::new(),
            account: name.to_string(),
            electrum_url: None,
            electrum_port: None,
            offline: Some(true),
            network,
            look_ahead: 5,
            mnemonic: Some(mnemo.to_string()),
            descriptor,
            persist: false,
            skip_labels: true,
            persist_kind: bwk::persist::PersistenceKind::default(),
        })
    }

    /// Build a syntactically valid p2tr scriptPubKey from a fixed
    /// 32-byte x-only key, suitable for seeding `SpCoinStore` in
    /// tests where we don't actually scan a chain.
    fn fake_tr_spk(seed: u8) -> bitcoin::ScriptBuf {
        use bitcoin::secp256k1::{Secp256k1, SecretKey};
        let secp = Secp256k1::new();
        let sk = SecretKey::from_slice(&[seed; 32]).unwrap();
        let (xonly, _parity) = sk.public_key(&secp).x_only_public_key();
        let tweaked = bitcoin::key::TweakedPublicKey::dangerous_assume_tweaked(xonly);
        bitcoin::Address::p2tr_tweaked(tweaked, bitcoin::Network::Regtest).script_pubkey()
    }

    fn fake_sp_owned(spk: bitcoin::ScriptBuf, seed: u8) -> OwnedOutput {
        OwnedOutput {
            blockheight: Height::from_consensus(100).unwrap(),
            tweak: [seed; 32],
            amount: bitcoin::Amount::from_sat(10_000),
            script: spk,
            label: None,
            spend_status: spdk_core::OutputSpendStatus::Unspent,
        }
    }

    #[test]
    fn owned_addresses_empty_for_fresh_account() {
        let account = Account::new(test_config()).expect("Account::new");
        // No sub-accounts, no SP coins, nothing to aggregate.
        assert!(account.owned_addresses().is_empty());
    }

    #[test]
    fn owned_addresses_includes_bip32_sub_accounts() {
        let mut account = Account::new(test_config()).expect("Account::new");
        let sub_name = "sub-segwit-0".to_string();
        account.add_sub_account(build_offline_segwit_sub(&sub_name));

        let owned = account.owned_addresses();
        // Sub-accounts populate look_ahead receive + look_ahead change
        // entries on construction (via CoinStore::generate). At least
        // one of them must show up tagged with the sub's name.
        assert!(
            !owned.is_empty(),
            "sub-account address entries should be visible in owned_addresses"
        );
        assert!(
            owned.iter().all(|o| o.account_name == sub_name),
            "every aggregated entry should carry the sub-account's name"
        );
        // BIP32-derived entries carry their keychain + index via
        // AddressSource::Bip32; SP-only variants must not appear.
        assert!(
            owned
                .iter()
                .all(|o| matches!(o.source, AddressSource::Bip32(_, _))),
            "every aggregated entry should carry AddressSource::Bip32"
        );
        // No SP coins were inserted, so nothing should be tagged with
        // the SP account's own name.
        assert!(owned.iter().all(|o| o.account_name != account.name()));
    }

    #[test]
    fn owned_addresses_includes_sp_received() {
        let account = Account::new(test_config()).expect("Account::new");
        let spk = fake_tr_spk(7);
        let outpoint = bitcoin::OutPoint {
            txid: bitcoin::Txid::from_byte_array([0xAB; 32]),
            vout: 0,
        };
        account
            .coin_store
            .lock()
            .expect("poisoned")
            .insert(outpoint, fake_sp_owned(spk.clone(), 7));

        let owned = account.owned_addresses();
        assert_eq!(owned.len(), 1, "only the SP coin should show up");
        let entry = &owned[0];
        assert_eq!(entry.account_name, account.name());
        assert_eq!(entry.status, bwk::address_store::AddressStatus::Used);
        assert_eq!(entry.funding_txids.len(), 1);
        assert!(entry.funding_txids.contains(&outpoint.txid));
        assert!(entry.spending_txids.is_empty());
        // SP-derived spk: source must be SilentPayment(funding_txid).
        match entry.source {
            AddressSource::SilentPayment(txid) => assert_eq!(txid, outpoint.txid),
            ref other => panic!("expected SilentPayment source, got {other:?}"),
        }
        // Canonical form of the spk matches what the consumer would
        // see via Address::from_script.
        let expected = bitcoin::Address::from_script(&spk, bitcoin::Network::Signet)
            .unwrap()
            .to_string();
        assert_eq!(entry.address, expected);
    }

    // -----------------------------------------------------------------
    // lookup_owned_address — sanity-check API for export / send paths
    // -----------------------------------------------------------------

    #[test]
    fn lookup_returns_none_for_unknown_address() {
        let mut account = Account::new(test_config()).expect("Account::new");
        account.add_sub_account(build_offline_segwit_sub("sub-segwit-0"));
        // A signet bech32 address that the wallet definitely doesn't own.
        let unknown = "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx";
        assert!(account.lookup_owned_address(unknown).is_none());
    }

    #[test]
    fn lookup_returns_entry_for_owned_bip32_address() {
        let mut account = Account::new(test_config()).expect("Account::new");
        account.add_sub_account(build_offline_segwit_sub("sub-segwit-0"));

        // Pick any address the sub generated at construction time,
        // look it up by canonical string, and check we get it back.
        let sample = account
            .sub_accounts()
            .iter()
            .flat_map(|s| s.address_entries())
            .next()
            .expect("sub-account has entries");
        let canonical = sample.value();

        let hit = account
            .lookup_owned_address(&canonical)
            .expect("address should be owned");
        assert_eq!(hit.address, canonical);
        assert_eq!(hit.account_name, "sub-segwit-0");
    }

    #[test]
    fn lookup_returns_entry_for_sp_received_spk() {
        let account = Account::new(test_config()).expect("Account::new");
        let spk = fake_tr_spk(13);
        let outpoint = bitcoin::OutPoint {
            txid: bitcoin::Txid::from_byte_array([0xCD; 32]),
            vout: 1,
        };
        account
            .coin_store
            .lock()
            .expect("poisoned")
            .insert(outpoint, fake_sp_owned(spk.clone(), 13));

        let canonical = bitcoin::Address::from_script(&spk, bitcoin::Network::Signet)
            .unwrap()
            .to_string();
        let hit = account
            .lookup_owned_address(&canonical)
            .expect("SP-derived spk should be owned");
        assert_eq!(hit.account_name, account.name());
        assert_eq!(hit.status, bwk::address_store::AddressStatus::Used);
        assert!(hit.funding_txids.contains(&outpoint.txid));
    }
}
