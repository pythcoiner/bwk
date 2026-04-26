use std::{
    collections::BTreeMap,
    str::FromStr,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
};

use bwk_backoff::Backoff;
use bwk_descriptor::derivator::SpkDerivator;
use bwk_electrum::client::{CoinRequest, CoinResponse};
use bwk_persist::{ConfigStore, NoopConfigStore, PersistenceBackend, Store};
use bwk_sign::signing_manager::SigningManager;
use bwk_tx::{coin::KeyChain, tx_builder::TxBuilder, ChangeRecipientProvider, Coin};

use miniscript::{
    bitcoin::{self, OutPoint, ScriptBuf},
    Descriptor, DescriptorPublicKey,
};
use serde::{Deserialize, Serialize};

use crate::{
    address_store::{AddressEntry, AddressStatus, AddressTip, ChangeTipUpdater},
    coin_store::{CoinEntry, CoinStore, CoinStoreSource, Payment, PaymentType},
    config::{Config, Tip},
    label_store::{LabelKey, LabelStore},
    profile::{self, DefaultBackend, OpenFromBackend, RamProfile, StorageProfile, Stores},
    tx_store::{TxEntry, TxStore},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
pub enum AddrAccount {
    Receive,
    Change,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoinState {
    pub coins: BTreeMap<OutPoint, Coin>,
    pub confirmed_coins: usize,
    pub confirmed_balance: u64,
    pub unconfirmed_coins: usize,
    pub unconfirmed_balance: u64,
}

/// Silent Payments notification variants (behind `sp` feature).
#[cfg(feature = "sp")]
#[derive(Debug, Clone)]
pub enum SpNotification {
    /// Scanner is starting
    StartingScan,
    /// Scan has started
    ScanStarted { start: u32, end: u32 },
    /// Scanner failed to start
    FailStartScanning { message: String },
    /// Scan failed during scanning
    FailScan { message: String },
    /// Scanner is stopping
    StoppingScan,
    /// Scanner has stopped
    ScanStopped,
    /// Scan progress update
    ScanProgress { current: u32, end: u32 },
    /// Scan completed successfully
    ScanCompleted,
    /// A new output was found
    NewOutput(OutPoint),
    /// An output was spent
    OutputSpent(OutPoint),
    /// Continuous mode: at chain tip, waiting for new blocks
    WaitingForBlocks { tip_height: u32 },
    /// Continuous mode: new block(s) detected
    NewBlocksDetected { from_height: u32, to_height: u32 },
}

/// Notifications sent by an Account to signal events.
#[derive(Debug, Clone)]
pub enum Notification {
    Electrum(TxListenerNotif),
    AddressTipChanged,
    CoinUpdate,
    InvalidElectrumConfig,
    InvalidLookAhead,
    Stopped,
    Error(Error),
    #[cfg(feature = "sp")]
    Sp(SpNotification),
}

impl From<TxListenerNotif> for Notification {
    fn from(value: TxListenerNotif) -> Self {
        Notification::Electrum(value)
    }
}

impl From<Error> for Notification {
    fn from(value: Error) -> Self {
        Self::Error(value)
    }
}

#[cfg(feature = "sp")]
impl From<SpNotification> for Notification {
    fn from(sp: SpNotification) -> Self {
        Notification::Sp(sp)
    }
}

#[derive(Debug, Clone)]
pub enum Error {
    CreatePool,
    JoinPool,
    InvalidOutPoint,
    CoinMissing,
    InvalidDenomination,
    RelayMissing,
    WrongElectrumConfig,
    PoolMissing,
    WrongKeyType,
    Satisfaction,
}

/// Represents notifications related to transaction listeners.
#[derive(Debug, Clone)]
pub enum TxListenerNotif {
    Started,
    Connected(String),
    Error(String),
    Stopped,
}

pub struct Account<P: StorageProfile = RamProfile<DefaultBackend>> {
    coin_store: Arc<Mutex<CoinStore<P>>>,
    label_store: Arc<Mutex<LabelStore<P>>>,
    receiver: Option<mpsc::Receiver<Notification>>,
    sender: mpsc::Sender<Notification>,
    tx_listener: Option<JoinHandle<()>>,
    config: Config,
    /// Persistence sink for `config`. [`NoopConfigStore`] by default.
    /// Consumers wire whatever shape suits them — a
    /// [`bwk_persist::FileConfigStore`] for file-backed persistence, a
    /// [`bwk_persist::CallbackConfigStore`] to bridge save/load through
    /// host-supplied closures, or any other [`ConfigStore`] impl.
    config_store: Arc<dyn ConfigStore<Config>>,
    electrum_stop: Option<Arc<AtomicBool>>,
    signing_manager: SigningManager<P::SignerStore>,
    /// Owned by the Electrum listener thread once it spawns; `take()`-n
    /// in `start_listen_txs` and moved into `listen_txs`.
    statuses_store: Option<P::StatusesStore>,
}

impl<P: StorageProfile> std::fmt::Debug for Account<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account").finish()
    }
}

impl<P: StorageProfile> Drop for Account<P> {
    fn drop(&mut self) {
        // Signal the Electrum listener thread to stop, then block until
        // it has actually exited. The listener holds Arc clones of the
        // persistence backend; without the join, the DirLock on the
        // account directory would stay acquired past Drop and refuse a
        // subsequent reopen.
        if let Some(stop) = self.electrum_stop.as_mut() {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(handle) = self.tx_listener.take() {
            let _ = handle.join();
        }
    }
}

// RAM-specific adapter for callers that already hold a `RamStores<B>`
// bundle. Forwards to the generic `from_stores`.
impl Account<RamProfile<DefaultBackend>> {
    #[allow(dead_code)]
    fn from_ram_stores(
        config: Config,
        sender: mpsc::Sender<Notification>,
        ram: profile::RamStores<DefaultBackend>,
    ) -> Self {
        Self::from_stores(
            config,
            sender,
            default_config_store(),
            Stores {
                tx: ram.tx,
                label: ram.label,
                statuses: ram.statuses,
                account: ram.account,
                signers: ram.signers,
            },
        )
    }
}

fn default_config_store() -> Arc<dyn ConfigStore<Config>> {
    Arc::new(NoopConfigStore::<Config>::default())
}

// Generic constructors over any profile that knows how to open its
// store bundle from a single `Arc<dyn PersistenceBackend>`.
impl<P: OpenFromBackend> Account<P> {
    /// Creates a new `Account` instance with the given configuration.
    ///
    /// Opens the profile's stores against whatever backend the config
    /// selects ([`JsonBackend`][bwk_persist::JsonBackend] by default,
    /// `SqliteBackend` under `PersistenceKind::Sqlite`). Defaults to
    /// the [`RamProfile<DefaultBackend>`] storage strategy via the
    /// `Account` struct's default type parameter.
    ///
    /// Config persistence defaults to [`NoopConfigStore`]; use
    /// [`Account::with_config_store`] to wire a concrete impl
    /// ([`bwk_persist::FileConfigStore`] for file-backed,
    /// [`bwk_persist::CallbackConfigStore`] to bridge through
    /// caller-supplied closures, or any other [`ConfigStore`]).
    pub fn new(config: Config) -> Self {
        let (sender, receiver) = mpsc::channel();
        let mut account = Self::new_inner(config, sender, default_config_store());
        account.receiver = Some(receiver);
        account
    }

    /// Like [`Account::new`] but with an explicit config store.
    pub fn with_config_store(config: Config, config_store: Arc<dyn ConfigStore<Config>>) -> Self {
        let (sender, receiver) = mpsc::channel();
        let mut account = Self::new_inner(config, sender, config_store);
        account.receiver = Some(receiver);
        account
    }

    /// Creates a new `Account` using an external notification sender.
    pub fn new_with_sender(config: Config, sender: mpsc::Sender<Notification>) -> Self {
        Self::new_inner(config, sender, default_config_store())
    }

    fn new_inner(
        config: Config,
        sender: mpsc::Sender<Notification>,
        config_store: Arc<dyn ConfigStore<Config>>,
    ) -> Self {
        assert!(!config.account.is_empty());
        let backend: Arc<dyn PersistenceBackend> = config
            .build_backend()
            .expect("Account::new: failed to build persistence backend");
        // Hot-signer material must not land on the SQLite DB; route the
        // SignerStore slot through a NoopBackend in that case.
        let secrets_backend: Arc<dyn PersistenceBackend> =
            if matches!(config.persist_kind, bwk_persist::PersistenceKind::Sqlite) {
                Arc::new(bwk_persist::NoopBackend)
            } else {
                backend.clone()
            };
        let stores =
            P::open(backend, secrets_backend).expect("Account::new: failed to open stores");
        Self::from_stores(config, sender, config_store, stores)
    }

    /// Recreate the Account with the same config, online.
    pub fn restart_electrum(&mut self) {
        let store = self.config_store.clone();
        let mut new_account = Account::<P>::with_config_store(self.config.clone(), store);
        new_account.config.set_offline(false);
        new_account.persist_config();
        *self = new_account;
    }
}

impl<P: StorageProfile> Account<P> {
    fn from_stores(
        config: Config,
        sender: mpsc::Sender<Notification>,
        config_store: Arc<dyn ConfigStore<Config>>,
        stores: Stores<P>,
    ) -> Self {
        let tx_store = TxStore::from_store(stores.tx);
        let label_store = Arc::new(Mutex::new(LabelStore::from_store(stores.label)));
        let account_store = Arc::new(Mutex::new(stores.account));
        // Tip is loaded from the account_store.
        let tip = {
            let store = account_store.lock().expect("poisoned");
            Tip::from_account_store(&*store)
        };
        let Tip { receive, change } = tip;
        let coin_store = Arc::new(Mutex::new(CoinStore::new(
            config.network,
            config.descriptor.clone(),
            sender.clone(),
            receive,
            change,
            config.look_ahead,
            tx_store,
            label_store.clone(),
            config.clone(),
            account_store.clone(),
        )));
        coin_store.lock().expect("poisoned").generate();
        let mut signing_manager = SigningManager::from_store(stores.signers);
        if let Some(mnemo) = config.mnemonic.clone() {
            signing_manager.new_bip32_signer_from_mnemonic(config.network(), mnemo);
            signing_manager.register_bip32_descriptor(config.descriptor.clone());
        }
        let _ = account_store; // owned by CoinStore→AddressStore; not stored on Account
        let mut account = Account {
            coin_store,
            label_store,
            tx_listener: None,
            electrum_stop: None,
            receiver: None,
            sender,
            config,
            config_store,
            signing_manager,
            statuses_store: Some(stores.statuses),
        };
        if !account.config.offline() {
            account.start_electrum();
        }
        account
    }
}

impl<P: StorageProfile> Account<P> {
    /// Push the current config to the configured [`ConfigStore`].
    ///
    /// Under [`bwk_persist::PersistenceKind::Sqlite`] the saved view has
    /// signer material stripped via [`Config::for_persistence`].
    fn persist_config(&self) {
        if let Err(e) = self.config_store.save(&self.config.for_persistence()) {
            log::warn!("config save failed: {e}");
        }
    }
}

// Non (b)locking API
impl<P: StorageProfile> Account<P> {
    pub fn network(&self) -> bitcoin::Network {
        self.config.network()
    }

    pub fn name(&self) -> String {
        self.config.account.clone()
    }

    pub fn descriptor_str(&self) -> String {
        self.config.descriptor.to_string()
    }

    pub fn descriptor(&self) -> Descriptor<DescriptorPublicKey> {
        self.config.descriptor.clone()
    }

    pub fn receiver(&mut self) -> Option<mpsc::Receiver<Notification>> {
        self.receiver.take()
    }

    /// Returns the configuration of the account.
    ///
    /// # Returns
    ///
    /// A boxed `Config` instance.
    pub fn get_config(&self) -> Config {
        self.config.clone()
    }

    pub fn coin_source(&self) -> CoinStoreSource<P> {
        CoinStoreSource::<P>::new(self.coin_store.clone())
    }

    pub fn sign(&self, psbt: String) {
        self.signing_manager.sign(psbt);
    }

    pub fn sign_psbt(&self, psbt: &mut bitcoin::Psbt) {
        self.signing_manager.sign_psbt(psbt);
    }

    /// Returns master xprivs from all BIP32 hot signers, keyed by fingerprint.
    pub fn master_xprivs(&self) -> BTreeMap<bitcoin::bip32::Fingerprint, bitcoin::bip32::Xpriv> {
        self.signing_manager.master_xprivs()
    }
}

// Locking API
impl<P: StorageProfile> Account<P> {
    pub fn tx_builder(&self) -> TxBuilder {
        let tip_updater =
            ChangeTipUpdater::new(self.coin_store.lock().expect("poisoned").address_store());
        let change_provider = Box::new(ChangeRecipientProvider::new_with_updater(
            tip_updater,
            self.descriptor(),
            self.network(),
        ));
        let coin_source = Box::new(CoinStoreSource::new(self.coin_store.clone()));
        TxBuilder::new(change_provider).coin_source(coin_source)
    }

    pub fn balance(&self) -> (u64, Vec<Payment>) {
        let payments = self.payment_history();
        let balance = payments.iter().fold(0, |a, b| match b.payment_type {
            PaymentType::Receive => a + (b.amount as i128),
            PaymentType::Send => a - (b.amount as i128),
            PaymentType::ToSelf => unimplemented!(),
        }) as u64;
        (balance, payments)
    }
    /// Returns a map of coins associated with the account.
    ///
    /// # Returns
    ///
    /// A `BTreeMap` of `OutPoint` to `CoinEntry`.
    pub fn coins(&self) -> BTreeMap<OutPoint, CoinEntry> {
        self.coin_store.lock().expect("poisoned").coins()
    }

    /// Returns the coin matching the given outpoint if found, else None.
    pub fn get_coin(&self, outpoint: &OutPoint) -> Option<Coin> {
        self.coin_store
            .lock()
            .expect("poisoned")
            .get(outpoint)
            .map(|e| e.coin)
    }

    /// Returns spendable coins for the account.
    pub fn spendable_coins(&self) -> CoinState {
        self.coin_store.lock().expect("poisoned").spendable_coins()
    }

    /// Returns a list of all historical transactions
    pub fn tx_history(&self) -> Vec<TxEntry> {
        self.coin_store.lock().expect("poisoned").tx_history()
    }

    /// Returns a list of all historical payments
    pub fn payment_history(&self) -> Vec<Payment> {
        self.tx_history().into_iter().map(Into::into).collect()
    }

    /// Updates the label of a coin identified by the given outpoint.
    ///
    /// # Arguments
    ///
    /// * `outpoint` - A string representation of the outpoint for the coin.
    /// * `label` - The new label to set for the coin. If the label is empty, the label will be removed.
    pub fn update_coin_label(&self, outpoint: String, label: String) {
        if let Ok(outpoint) = bitcoin::OutPoint::from_str(&outpoint) {
            if !label.is_empty() {
                self.label_store
                    .lock()
                    .expect("poisoned")
                    .edit(LabelKey::OutPoint(outpoint), Some(label));
            } else {
                self.label_store
                    .lock()
                    .expect("poisoned")
                    .remove(LabelKey::OutPoint(outpoint));
            }
        }
        if let Ok(mut store) = self.coin_store.try_lock() {
            store.generate();
        }
    }

    /// Generates a new receiving address entry for the account.
    ///
    /// # Returns
    ///
    /// A boxed `AddressEntry` instance.
    pub fn new_addr(&mut self) -> AddressEntry {
        let addr = self.new_recv_addr();
        let index = self.coin_store.lock().expect("poisoned").recv_tip();
        AddressEntry {
            status: AddressStatus::NotUsed,
            address: addr.as_unchecked().clone(),
            account: KeyChain::Receive,
            index,
            funding_txids: std::collections::BTreeSet::new(),
            spending_txids: std::collections::BTreeSet::new(),
        }
    }
    fn new_recv_addr(&mut self) -> bitcoin::Address {
        self.coin_store.lock().expect("poisoned").new_recv_addr()
    }
    #[allow(unused)]
    fn new_change_addr(&mut self) -> bitcoin::Address {
        self.coin_store.lock().expect("poisoned").new_change_addr()
    }
}

// Derivation specific implementation
impl<P: StorageProfile> Account<P> {
    /// Returns the derivator associated with the account.
    ///
    /// # Returns
    ///
    /// A `Derivator` instance.
    pub fn derivator(&self) -> SpkDerivator {
        self.coin_store.lock().expect("poisoned").derivator()
    }
    #[allow(unused)] // Internal usage only
    fn recv_at(&self, index: u32) -> bitcoin::Address {
        self.coin_store
            .lock()
            .expect("poisoned")
            .derivator_ref()
            .receive_at(index)
    }

    #[allow(unused)] // Internal usage only
    fn change_at(&self, index: u32) -> bitcoin::Address {
        self.coin_store
            .lock()
            .expect("poisoned")
            .derivator_ref()
            .change_at(index)
    }

    /// Returns the current receiving watch tip index.
    ///
    /// # Returns
    ///
    /// The receiving watch tip index as a `u32`.
    pub fn recv_watch_tip(&self) -> u32 {
        self.coin_store.lock().expect("poisoned").recv_watch_tip()
    }

    /// Returns the current change watch tip index.
    ///
    /// # Returns
    ///
    /// The change watch tip index as a `u32`.
    pub fn change_watch_tip(&self) -> u32 {
        self.coin_store.lock().expect("poisoned").change_watch_tip()
    }

    pub fn generated_addresses(
        &self,
    ) -> (
        Vec<AddressEntry>, /* receive */
        Vec<AddressEntry>, /* change*/
    ) {
        self.coin_store
            .lock()
            .expect("poisoned")
            .address_store()
            .lock()
            .expect("poisoned")
            .get_generated_addresses()
    }

    /// Snapshot of every address entry the account currently tracks
    /// (receive + change, all derivation indices). Cloned, so the
    /// caller doesn't hold any lock.
    pub fn address_entries(&self) -> Vec<AddressEntry> {
        self.coin_store
            .lock()
            .expect("poisoned")
            .address_store()
            .lock()
            .expect("poisoned")
            .entries()
    }
}

// Electrum specific implementation
impl<P: StorageProfile> Account<P> {
    /// Re-generate coin_store from tx_store
    pub fn generate_coins(&mut self) {
        self.coin_store.lock().expect("poisoned").generate();
    }
    pub fn electrum_url(&self) -> String {
        self.config.electrum_url()
    }

    pub fn electrum_port(&self) -> String {
        self.config.electrum_port()
    }

    pub fn look_ahead(&self) -> String {
        self.config.look_ahead()
    }

    /// Starts listening for transactions on the specified address and port.
    ///
    /// # Arguments
    ///
    /// * `addr` - The address to listen on.
    /// * `port` - The port to listen on.
    ///
    /// # Returns
    ///
    /// A tuple containing a sender for address tips and a stop flag.
    fn start_listen_txs(
        &mut self,
        addr: String,
        port: u16,
    ) -> (mpsc::Sender<AddressTip>, Arc<AtomicBool>) {
        log::debug!("Account::start_poll_txs()");
        let (sender, address_tip) = mpsc::channel();
        let coin_store = self.coin_store.clone();
        let notification = self.sender.clone();
        let derivator = self.derivator();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_request = stop.clone();
        let statuses_store = self
            .statuses_store
            .take()
            .expect("statuses store available when starting Electrum listener");

        let poller = thread::spawn(move || {
            let client = match bwk_electrum::client::Client::new(&addr, port) {
                Ok(c) => c,
                Err(e) => {
                    log::error!("start_listen_txs(): fail to create electrum client {}", e);
                    let _ = notification.send(TxListenerNotif::Error(e.to_string()).into());
                    return;
                }
            };

            let addr = format!("{}:{}", addr, port);
            let _ = notification.send(TxListenerNotif::Connected(addr).into());

            let (request, response) = client.listen::<CoinRequest, CoinResponse>();

            listen_txs(
                coin_store,
                derivator,
                notification,
                address_tip,
                stop_request,
                request,
                response,
                statuses_store,
            );
        });
        self.tx_listener = Some(poller);
        (sender, stop)
    }
    /// Sets the Electrum server URL and port for the account.
    ///
    /// # Arguments
    ///
    /// * `url` - The URL of the Electrum server.
    /// * `port` - The port of the Electrum server.
    pub fn set_electrum(&mut self, url: String, port: String) {
        if let Ok(port) = port.parse::<u16>() {
            self.config.electrum_url = Some(url);
            self.config.electrum_port = Some(port);
            self.persist_config();
        } else {
            self.sender
                .send(Notification::InvalidElectrumConfig)
                .expect("cannot fail");
        }
    }

    /// Sets the Electrum URL and port in memory without writing to file.
    pub fn set_electrum_config(&mut self, url: Option<String>, port: Option<u16>) {
        self.config.electrum_url = url;
        self.config.electrum_port = port;
    }

    /// Starts the Electrum listener for the account.
    pub fn start_electrum(&mut self) {
        if let (None, Some(addr), Some(port)) = (
            &self.tx_listener,
            self.config.electrum_url.clone(),
            self.config.electrum_port,
        ) {
            let (tx_listener, electrum_stop) = self.start_listen_txs(addr, port);
            self.coin_store.lock().expect("poisoned").init(tx_listener);
            self.electrum_stop = Some(electrum_stop);
            if self.config.offline() {
                self.config.set_offline(false);
                self.persist_config();
            }
        }
    }

    /// Stops the Electrum listener for the account.
    pub fn stop_electrum(&mut self) {
        if let Some(stop) = self.electrum_stop.as_mut() {
            stop.store(true, Ordering::Relaxed);
        }
        self.electrum_stop = None;
        self.tx_listener = None;
        self.config.set_offline(true);
        self.persist_config();
    }

    pub fn electrum_offline(&self) -> bool {
        self.config.offline()
    }

    /// Sets the look-ahead value for the account.
    ///
    /// # Arguments
    ///
    /// * `look_ahead` - The look-ahead value to set.
    pub fn set_look_ahead(&mut self, look_ahead: String) {
        if let Ok(la) = look_ahead.parse::<u32>() {
            self.config.look_ahead = la;
            self.persist_config();
        } else {
            self.sender
                .send(Notification::InvalidLookAhead)
                .expect("cannot fail");
        }
    }
}

// /// Creates a new account with the specified account name.
// ///
// /// # Arguments
// ///
// /// * `account` - The name of the account.
// ///
// /// # Returns
// ///
// /// A boxed `Account` instance.
// pub fn new_account(account: String) -> Box<Account> {
//     let config = Config::from_file(account);
//
//     let account = Account::new(config);
//     account.boxed()
// }

macro_rules! send_notif {
    ($notification:expr, $request:expr, $msg:expr) => {
        let res = $notification.send($msg.into());
        if res.is_err() {
            // stop detached client
            let _ = $request.send(CoinRequest::Stop);
            return;
        }
    };
}

macro_rules! send_electrum {
    ($request:expr, $notification:expr, $msg:expr) => {
        if $request.send($msg).is_err() {
            send_notif!($notification, $request, TxListenerNotif::Stopped);
            return;
        }
    };
}

/// Listens for transactions on the specified address and port.
///
/// # Arguments
///
/// * `addr` - The address to listen on.
/// * `port` - The port to listen on.
/// * `coin_store` - The coin store to update with transaction data.
/// * `signer` - The signer for the account.
/// * `notification` - The sender for notifications.
/// * `address_tip` - The receiver for address tips.
/// * `stop_request` - The stop flag for the listener.
#[allow(clippy::too_many_arguments)]
fn listen_txs<T, P>(
    coin_store: Arc<Mutex<CoinStore<P>>>,
    derivator: SpkDerivator,
    notification: mpsc::Sender<T>,
    address_tip: mpsc::Receiver<AddressTip>,
    stop_request: Arc<AtomicBool>,
    request: mpsc::Sender<CoinRequest>,
    response: mpsc::Receiver<CoinResponse>,
    mut statuses: P::StatusesStore,
) where
    T: From<TxListenerNotif>,
    P: StorageProfile,
{
    log::info!("listen_txs(): started");
    send_notif!(notification, request, TxListenerNotif::Started);

    let initial_keys: Vec<ScriptBuf> = match statuses.keys() {
        Ok(it) => it.collect(),
        Err(e) => {
            log::error!("listen_txs(): statuses keys: {e}");
            Vec::new()
        }
    };
    if !initial_keys.is_empty() {
        send_electrum!(request, notification, CoinRequest::Subscribe(initial_keys));
    }

    fn flush_statuses<S: bwk_persist::Store>(statuses: &mut S) {
        if let Err(e) = statuses.flush() {
            log::error!("listen_txs(): statuses flush: {e}");
        }
    }

    let mut backoff = Backoff::new_ms(20);
    loop {
        // stop request from consumer side
        if stop_request.load(Ordering::Relaxed) {
            send_notif!(notification, request, TxListenerNotif::Stopped);
            let _ = request.send(CoinRequest::Stop);
            return;
        }

        let mut received = false;

        // listen for AddressTip update
        match address_tip.try_recv() {
            Ok(tip) => {
                log::debug!("listen_txs() receive {tip:?}");
                let AddressTip { recv, change } = tip;
                received = true;
                let mut sub = vec![];
                let r_spk = derivator.receive_at(recv).script_pubkey();
                if !statuses.contains_key(&r_spk).unwrap_or(false) {
                    // FIXME: here we can be smart an not start at 0 but at `actual_tip`
                    for i in 0..recv {
                        let spk = derivator.receive_at(i).script_pubkey();
                        if !statuses.contains_key(&spk).unwrap_or(false) {
                            if let Err(e) = statuses.insert(spk.clone(), (None, 0, i)) {
                                log::error!("listen_txs(): statuses insert: {e}");
                                continue;
                            }
                            sub.push(spk);
                        }
                    }
                }
                let c_spk = derivator.change_at(recv).script_pubkey();
                if !statuses.contains_key(&c_spk).unwrap_or(false) {
                    // FIXME: here we can be smart an not start at 0 but at `actual_tip`
                    for i in 0..change {
                        let spk = derivator.change_at(i).script_pubkey();
                        if !statuses.contains_key(&spk).unwrap_or(false) {
                            if let Err(e) = statuses.insert(spk.clone(), (None, 1, i)) {
                                log::error!("listen_txs(): statuses insert: {e}");
                                continue;
                            }
                            sub.push(spk);
                        }
                    }
                }
                if !sub.is_empty() {
                    flush_statuses(&mut statuses);
                    send_electrum!(request, notification, CoinRequest::Subscribe(sub));
                }
            }
            Err(e) => match e {
                mpsc::TryRecvError::Empty => {}
                mpsc::TryRecvError::Disconnected => {
                    log::error!("listen_txs(): address store disconnected");
                    send_notif!(
                        notification,
                        request,
                        TxListenerNotif::Error("AddressStore disconnected".to_string())
                    );
                    // FIXME: what should we do there?
                    // it's AddressStore being dropped, but she should keep upating
                    // the actual spk set even if it cannot grow anymore
                }
            },
        }

        // listen for response
        match response.try_recv() {
            Ok(rsp) => {
                log::debug!("listen_txs() receive {rsp:#?}");
                received = true;
                match rsp {
                    CoinResponse::Status(elct_status) => {
                        let mut history = vec![];
                        let mut dirty = false;
                        for (spk, status) in elct_status {
                            match statuses.get(&spk) {
                                Ok(Some((s, _, _))) => {
                                    // status is registered
                                    if s != status {
                                        // status changed
                                        if status.is_some() {
                                            // not empty: ask for tx changes
                                            history.push(spk.clone());
                                        } else {
                                            // Some(_) -> None: clear coin_store
                                            let mut store = coin_store.lock().expect("poisoned");
                                            let mut map = BTreeMap::new();
                                            map.insert(spk.clone(), vec![]);
                                            let _ = store.handle_history_response(map);
                                            store.generate();
                                        }
                                        // record the local status change
                                        let new_status = status.clone();
                                        match statuses.modify(&spk, |v| v.0 = new_status.clone()) {
                                            Ok(true) => dirty = true,
                                            Ok(false) => {
                                                // race-free under single-listener thread:
                                                // we just observed the entry above
                                            }
                                            Err(e) => {
                                                log::error!("listen_txs(): statuses modify: {e}");
                                            }
                                        }
                                    }
                                }
                                Ok(None) => {
                                    // not registered: previous behaviour was an
                                    // `entry(spk).and_modify(...)`, which is a no-op
                                    // for vacant entries. Preserve that — only the
                                    // history-side effect remains.
                                    if status.is_some() {
                                        history.push(spk);
                                    } else {
                                        let mut store = coin_store.lock().expect("poisoned");
                                        let mut map = BTreeMap::new();
                                        map.insert(spk.clone(), vec![]);
                                        let _ = store.handle_history_response(map);
                                    }
                                }
                                Err(e) => {
                                    log::error!("listen_txs(): statuses get: {e}");
                                }
                            }
                        }
                        if !history.is_empty() {
                            let hist = CoinRequest::History(history);
                            log::debug!("listen_txs() send {:#?}", hist);
                            send_electrum!(request, notification, hist);
                        }
                        if dirty {
                            flush_statuses(&mut statuses);
                        }
                    }
                    CoinResponse::History(map) => {
                        let mut store = coin_store.lock().expect("poisoned");
                        let (height_updated, missing_txs) = store.handle_history_response(map);
                        if !missing_txs.is_empty() {
                            send_electrum!(request, notification, CoinRequest::Txs(missing_txs));
                        }
                        if height_updated {
                            store.generate();
                        }
                    }
                    CoinResponse::Txs(txs) => {
                        let mut store = coin_store.lock().expect("poisoned");
                        store.handle_txs_response(txs);
                    }
                    CoinResponse::Stopped => {
                        send_notif!(notification, request, TxListenerNotif::Stopped);
                        let _ = request.send(CoinRequest::Stop);
                        return;
                    }
                    CoinResponse::Error(e) => {
                        send_notif!(notification, request, TxListenerNotif::Error(e));
                    }
                }
            }
            Err(e) => match e {
                mpsc::TryRecvError::Empty => {}
                mpsc::TryRecvError::Disconnected => {
                    // NOTE: here the electrum client is dropped, we cannot continue
                    log::error!("listen_txs() electrum client stopped unexpectedly");
                    send_notif!(notification, request, TxListenerNotif::Stopped);
                    let _ = request.send(CoinRequest::Stop);
                    return;
                }
            },
        }

        if received {
            continue;
        }
        backoff.snooze();
    }
}

#[cfg(all(test, feature = "test"))]
mod tests {
    use super::*;
    use crate::tx_store::TxStore;
    use bip39::Mnemonic;
    use bwk_descriptor::descriptor::{wpkh, ScriptType};
    use bwk_persist::NoopBackend;
    use bwk_sign::hot_signer::HotSigner;
    use bwk_tx::CoinStatus;
    use bwk_utils::test::{funding_tx, setup_logger, spending_tx};
    use miniscript::bitcoin::{bip32::ChildNumber, Network};
    use std::{path::PathBuf, str::FromStr, sync::mpsc::TryRecvError, time::Duration};
    use {bip39, miniscript::bitcoin::bip32::DerivationPath};

    struct CoinStoreMock {
        pub store: Arc<Mutex<CoinStore>>,
        pub notif: mpsc::Receiver<Notification>,
        pub request: mpsc::Receiver<CoinRequest>,
        pub response: mpsc::Sender<CoinResponse>,
        pub listener: JoinHandle<()>,
        pub stop: Arc<AtomicBool>,
        pub derivator: SpkDerivator,
    }

    impl Drop for CoinStoreMock {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
        }
    }

    impl CoinStoreMock {
        fn new(recv_tip: u32, change_tip: u32, look_ahead: u32) -> Self {
            let (notif_sender, notif_recv) = mpsc::channel();
            let (tip_sender, tip_receiver) = mpsc::channel();
            let (req_sender, req_receiver) = mpsc::channel();
            let (resp_sender, resp_receiver) = mpsc::channel();
            let mnemo = Mnemonic::generate(12).unwrap();
            let dummy_config = Config::new(
                Some(mnemo.to_string()),
                "dummy".into(),
                Network::Regtest,
                ScriptType::Segwit(ChildNumber::from_hardened_idx(0).unwrap()),
                PathBuf::default(),
                String::new(),
                false,
            )
            .unwrap();

            let mnemonic = bip39::Mnemonic::generate(12).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let signer =
                HotSigner::new_from_mnemonics(bitcoin::Network::Regtest, &mnemonic.to_string())
                    .unwrap();
            let xpub = signer.xpub(&DerivationPath::from_str("m/84'/0'/0'/1").unwrap());
            let descriptor = wpkh(xpub);
            let derivator =
                SpkDerivator::new(descriptor.clone(), bitcoin::Network::Regtest).unwrap();

            let tx_store = TxStore::new();
            let label_store = Arc::new(Mutex::new(LabelStore::new()));
            let mock_backend: Arc<dyn PersistenceBackend> = Arc::new(NoopBackend);
            let account_store = Arc::new(Mutex::new(bwk_persist::RamStore::empty(
                mock_backend.clone(),
                bwk_persist::ACCOUNT_STORE_KEY,
                crate::profile::encode_account_key,
                crate::profile::encode_account_value,
            )));
            let statuses_store = bwk_persist::RamStore::open(
                mock_backend,
                bwk_persist::STATUSES_STORE_KEY,
                crate::profile::encode_status_key,
                crate::profile::decode_status_key,
                crate::profile::encode_status_value,
                crate::profile::decode_status_value,
            )
            .expect("open statuses RamStore");
            let coin_store = Arc::new(Mutex::new(CoinStore::new(
                bitcoin::Network::Regtest,
                descriptor.clone(),
                notif_sender.clone(),
                recv_tip,
                change_tip,
                look_ahead,
                tx_store,
                label_store,
                dummy_config,
                account_store,
            )));
            coin_store.lock().expect("poisoned").init(tip_sender);
            let store = coin_store.clone();
            let cloned_stop = stop.clone();
            let cloned_derivator = derivator.clone();

            let listener_handle = thread::spawn(move || {
                listen_txs::<Notification, RamProfile<DefaultBackend>>(
                    coin_store,
                    cloned_derivator,
                    notif_sender,
                    tip_receiver,
                    stop,
                    req_sender,
                    resp_receiver,
                    statuses_store,
                );
            });

            CoinStoreMock {
                store,
                notif: notif_recv,
                request: req_receiver,
                response: resp_sender,
                listener: listener_handle,
                stop: cloned_stop,
                derivator,
            }
        }

        fn coins(&mut self) -> BTreeMap<OutPoint, CoinEntry> {
            self.store.lock().expect("poisoned").coins()
        }

        fn stop(&self) {
            self.stop.store(true, Ordering::Relaxed);
        }
    }

    #[test]
    fn simple_start_stop() {
        setup_logger();
        let mock = CoinStoreMock::new(0, 0, 20);
        thread::sleep(Duration::from_millis(10));
        assert!(!mock.listener.is_finished());
        assert!(matches!(
            mock.notif.try_recv().unwrap(),
            Notification::AddressTipChanged,
        ));
        assert!(matches!(
            mock.notif.try_recv().unwrap(),
            Notification::Electrum(TxListenerNotif::Started)
        ));
        mock.stop();
        thread::sleep(Duration::from_secs(1));
        assert!(mock.listener.is_finished());
    }

    fn simple_recv() -> (bitcoin::Transaction, CoinStoreMock) {
        setup_logger();
        let look_ahead = 5;
        let mut mock = CoinStoreMock::new(0, 0, look_ahead);
        thread::sleep(Duration::from_millis(500));
        assert!(!mock.listener.is_finished());
        assert!(matches!(
            mock.notif.try_recv().unwrap(),
            Notification::AddressTipChanged,
        ));
        assert!(matches!(
            mock.notif.try_recv().unwrap(),
            Notification::Electrum(TxListenerNotif::Started)
        ));

        let mut init_spks = vec![];
        for i in 0..(look_ahead + 1) {
            let spk = mock.derivator.receive_spk_at(i);
            init_spks.push(spk);
        }
        for i in 0..(look_ahead + 1) {
            let spk = mock.derivator.change_spk_at(i);
            init_spks.push(spk);
        }

        // receive initial subscriptions
        if let Ok(CoinRequest::Subscribe(v)) = mock.request.try_recv() {
            // NOTE: we expect (tip + 1 + look_ahead )
            assert_eq!(v.len(), 12);
            for spk in &init_spks {
                assert!(v.contains(spk));
            }
        } else {
            panic!()
        }

        // electrum server send spks statuses (None)
        let statuses: BTreeMap<_, _> = init_spks.clone().into_iter().map(|s| (s, None)).collect();
        mock.response.send(CoinResponse::Status(statuses)).unwrap();

        thread::sleep(Duration::from_millis(100));

        assert!(mock.coins().is_empty());

        let spk_recv_0 = mock.derivator.receive_spk_at(0);

        // server send a status update at recv(0)
        let mut statuses = BTreeMap::new();
        statuses.insert(spk_recv_0.clone(), Some("1_tx_unco".to_string()));
        mock.response.send(CoinResponse::Status(statuses)).unwrap();
        thread::sleep(Duration::from_millis(100));

        // server should receive an history request for this spk
        if let Ok(CoinRequest::History(v)) = mock.request.try_recv() {
            assert!(v == vec![spk_recv_0.clone()]);
        } else {
            panic!()
        }

        thread::sleep(Duration::from_millis(100));

        let tx_0 = funding_tx(spk_recv_0.clone(), 0.1);

        // server must send history response
        let mut history = BTreeMap::new();
        history.insert(spk_recv_0.clone(), vec![(tx_0.compute_txid(), None)]);
        mock.response.send(CoinResponse::History(history)).unwrap();

        thread::sleep(Duration::from_millis(100));

        // server should receive a tx request
        if let Ok(CoinRequest::Txs(v)) = mock.request.try_recv() {
            assert!(v == vec![tx_0.compute_txid()]);
        } else {
            panic!()
        }

        thread::sleep(Duration::from_millis(100));

        // server send the requested tx
        mock.response
            .send(CoinResponse::Txs(vec![tx_0.clone()]))
            .unwrap();

        thread::sleep(Duration::from_millis(100));

        // now the store contain one coin
        let mut coins = mock.coins();
        assert_eq!(coins.len(), 1);
        let coin = coins.pop_first().unwrap().1;

        // the coin is unconfirmed
        assert_eq!(coin.height(), None);
        assert_eq!(coin.status(), CoinStatus::Unconfirmed);

        // NOTE: the coin is now confirmed

        // server send a status update at recv(0)
        let mut statuses = BTreeMap::new();
        statuses.insert(spk_recv_0.clone(), Some("1_tx_conf".to_string()));
        mock.response.send(CoinResponse::Status(statuses)).unwrap();
        thread::sleep(Duration::from_millis(100));

        // server should receive an history request for this spk
        if let Ok(CoinRequest::History(v)) = mock.request.try_recv() {
            assert!(v == vec![spk_recv_0.clone()]);
        } else {
            panic!()
        }

        thread::sleep(Duration::from_millis(100));

        // server must send history response
        let mut history = BTreeMap::new();
        // the coin have now 1 confirmation
        history.insert(spk_recv_0.clone(), vec![(tx_0.compute_txid(), Some(1))]);
        mock.response.send(CoinResponse::History(history)).unwrap();

        thread::sleep(Duration::from_millis(100));

        // NOTE: coin_store already have the tx it should not ask it
        assert!(matches!(mock.request.try_recv(), Err(TryRecvError::Empty)));

        // the coin is now confirmed
        let mut coins = mock.coins();
        assert_eq!(coins.len(), 1);
        let coin = coins.pop_first().unwrap().1;
        assert_eq!(coin.height(), Some(1));
        assert_eq!(coin.status(), CoinStatus::Confirmed);
        (tx_0, mock)
    }

    #[test]
    fn recv_and_spend() {
        // init & receive one coin
        let (tx_0, mut mock) = simple_recv();
        let spk_recv_0 = mock.derivator.receive_spk_at(0);

        // spend this coin
        let outpoint = mock.coins().pop_first().unwrap().0;
        let tx_1 = spending_tx(outpoint);

        // NOTE: the coin is now spent

        // server send a status update at recv(0)
        let mut statuses = BTreeMap::new();
        statuses.insert(spk_recv_0.clone(), Some("1_tx_spent".to_string()));
        mock.response.send(CoinResponse::Status(statuses)).unwrap();
        thread::sleep(Duration::from_millis(100));

        // server should receive an history request for this spk
        if let Ok(CoinRequest::History(v)) = mock.request.try_recv() {
            assert!(v == vec![spk_recv_0.clone()]);
        } else {
            panic!()
        }

        thread::sleep(Duration::from_millis(100));

        // server must send history response
        let mut history = BTreeMap::new();
        // the coin have now 1 confirmation
        history.insert(
            spk_recv_0.clone(),
            vec![(tx_0.compute_txid(), Some(1)), (tx_1.compute_txid(), None)],
        );
        mock.response.send(CoinResponse::History(history)).unwrap();

        thread::sleep(Duration::from_millis(100));

        // server should receive a tx request only for tx_1
        if let Ok(CoinRequest::Txs(v)) = mock.request.try_recv() {
            assert!(v == vec![tx_1.compute_txid()]);
        } else {
            panic!()
        }

        // server send the requested tx
        mock.response
            .send(CoinResponse::Txs(vec![tx_1.clone()]))
            .unwrap();

        thread::sleep(Duration::from_millis(100));

        // now the store contain one spent coin
        let mut coins = mock.coins();
        assert_eq!(coins.len(), 1);
        let coin = coins.pop_first().unwrap().1;

        // the coin is unconfirmed
        assert_eq!(coin.status(), CoinStatus::Spent);
    }

    #[test]
    fn simple_reorg() {
        // init & receive one coin
        let (tx_0, mut mock) = simple_recv();
        let spk_recv_0 = mock.derivator.receive_spk_at(0);

        // NOTE: the coin is now spent we can reorg it

        // server send a status update at recv(0)
        let mut statuses = BTreeMap::new();
        statuses.insert(spk_recv_0.clone(), Some("1_tx_reorg".to_string()));
        mock.response.send(CoinResponse::Status(statuses)).unwrap();
        thread::sleep(Duration::from_millis(100));

        // server should receive an history request for this spk
        if let Ok(CoinRequest::History(v)) = mock.request.try_recv() {
            assert!(v == vec![spk_recv_0.clone()]);
        } else {
            panic!()
        }

        thread::sleep(Duration::from_millis(100));

        // server must send history response
        let mut history = BTreeMap::new();
        // NOTE: confirmation height is changed to 2
        history.insert(spk_recv_0.clone(), vec![(tx_0.compute_txid(), Some(2))]);
        mock.response.send(CoinResponse::History(history)).unwrap();

        thread::sleep(Duration::from_millis(100));

        // server do not receive a tx request as the store already go the tx
        assert!(matches!(mock.request.try_recv(), Err(TryRecvError::Empty)));

        // the store still contain one spent coin
        let mut coins = mock.coins();
        assert_eq!(coins.len(), 1);
        let coin = coins.pop_first().unwrap().1;

        // the coin have a confirmation height of 2
        assert_eq!(coin.height(), Some(2));
    }
}

#[cfg(test)]
mod integration_tests {

    use rand::random_range;
    use std::{collections::BTreeMap, env, path::PathBuf, thread::sleep, time::Duration};

    use crate::{
        coin_store::Payment,
        config::{maybe_create_dir, Config},
        log::INIT,
        Account,
    };
    use bip39::Mnemonic;
    use bwk_descriptor::descriptor::ScriptType;
    use bwk_electrum::client::Client;
    use miniscript::bitcoin::{
        self, bip32::ChildNumber, Address, Amount, Network, Transaction, Txid,
    };
    use miniscript::psbt::PsbtExt;
    use temp_dir::TempDir;

    use electrsd::{
        bitcoind::{
            bitcoincore_rpc::{jsonrpc::serde_json::Value, RpcApi},
            BitcoinD, P2P,
        },
        ElectrsD,
    };

    pub fn bootstrap_electrs() -> (
        String, /* url */
        u16,    /* port */
        ElectrsD,
        BitcoinD,
    ) {
        let mut cwd: PathBuf = env::current_dir().expect("Failed to get current directory");
        cwd.push("tests");

        let mut electrs_path = cwd.clone();
        electrs_path.push("bin");
        electrs_path.push("electrs_0_9_11");

        let mut bitcoind_path = cwd.clone();
        bitcoind_path.push("bin");
        bitcoind_path.push("bitcoind_25_2");

        let mut conf = electrsd::bitcoind::Conf::default();
        conf.p2p = P2P::Yes;
        let bitcoind = BitcoinD::with_conf(bitcoind_path, &conf).unwrap();

        let mut electrsd_conf = electrsd::Conf::default();
        electrsd_conf.args = vec!["--log-filters", "DEBUG"];
        electrsd_conf.buffered_logs = true;

        let electrsd = ElectrsD::with_conf(electrs_path, &bitcoind, &electrsd_conf).unwrap();
        let (url, port) = electrsd.electrum_url.split_once(':').unwrap();
        let port = port.parse::<u16>().unwrap();

        // mine 101 blocks
        let node_address = bitcoind.client.call::<Value>("getnewaddress", &[]).unwrap();
        bitcoind
            .client
            .call::<Value>("generatetoaddress", &[101.into(), node_address])
            .unwrap();

        (url.into(), port, electrsd, bitcoind)
    }

    #[allow(unused)]
    pub fn tcp_client() -> (Client, ElectrsD, BitcoinD) {
        let (url, port, e, b) = bootstrap_electrs();
        let client = Client::new(&url, port).unwrap();

        (client, e, b)
    }

    pub fn send_to_address(bitcoind: &BitcoinD, addr: &Address, amount: Amount) -> Txid {
        let txid = bitcoind
            .client
            .send_to_address(addr, amount, None, None, None, None, None, None)
            .unwrap();
        log::debug!("send_to_address({}, {}) => {}", addr, amount, txid);
        txid
    }

    #[allow(unused)]
    pub fn get_tx(bitcoind: &BitcoinD, txid: Txid) -> Transaction {
        bitcoind.client.get_raw_transaction(&txid, None).unwrap()
    }

    #[allow(unused)]
    pub fn broadcast(bitcoind: &BitcoinD, transaction: Transaction) {
        let _txid = bitcoind.client.send_raw_transaction(&transaction).unwrap();
    }

    pub fn get_block_hash(bitcoind: &BitcoinD, height: u32) -> String {
        bitcoind
            .client
            .call("getblockhash", &[height.into()])
            .unwrap()
    }

    pub fn get_block_height(bitcoind: &BitcoinD) -> u32 {
        bitcoind.client.call("getblockcount", &[]).unwrap()
    }

    pub fn generate(bitcoind: &BitcoinD, blocks: u32) {
        let node_address = bitcoind.client.call::<Value>("getnewaddress", &[]).unwrap();
        bitcoind
            .client
            .call::<Value>("generatetoaddress", &[blocks.into(), node_address])
            .unwrap();
    }

    pub fn reorg_chain(bitcoind: &BitcoinD, blocks: u32) {
        let chain_height: u32 = get_block_height(bitcoind);
        let reorg_height = chain_height - blocks;
        let block_hash = get_block_hash(bitcoind, reorg_height);

        invalidate_block(bitcoind, block_hash);

        generate(bitcoind, blocks);
    }

    pub fn invalidate_block(bitcoind: &BitcoinD, block_hash: String) {
        bitcoind
            .client
            .call::<Value>("invalidateblock", &[block_hash.clone().into()])
            .unwrap();

        log::info!("Invalidated block with hash: {}", block_hash);
    }

    pub fn dump_logs(e: &mut ElectrsD) {
        while let Ok(msg) = e.logs.try_recv() {
            println!("{}", msg);
        }
    }

    #[allow(unused)]
    pub fn setup_logger() {
        INIT.call_once(|| {
            env_logger::builder()
                .is_test(true)
                .filter_level(log::LevelFilter::Debug)
                .filter_module("bitcoind", log::LevelFilter::Info)
                .filter_module("bitcoincore_rpc", log::LevelFilter::Info)
                .filter_module("bwk::account", log::LevelFilter::Debug)
                .filter_module("bwk-electrum::electrum", log::LevelFilter::Debug)
                .filter_module("bwk-electrum::raw_client", log::LevelFilter::Debug)
                .init();
        });
    }

    pub fn wait_until_timeout<F>(condition: F, timeout: u64)
    where
        F: Fn() -> bool,
    {
        let delay = Duration::from_millis(100);
        let start_time = std::time::Instant::now();

        while start_time.elapsed().as_secs() < timeout {
            if condition() {
                return;
            }
            sleep(delay);
        }
        panic!("Timeout elapsed while waiting for condition.");
    }

    #[test]
    fn test_reorg() {
        // setup_logger();
        let (_, _, _electrsd, bitcoind) = bootstrap_electrs();
        generate(&bitcoind, 100);

        reorg_chain(&bitcoind, 5);
    }

    #[test]
    fn simple_wallet() {
        // setup_logger();
        let (url, port, _electrsd, bitcoind) = bootstrap_electrs();
        generate(&bitcoind, 100);

        const TIMEOUT: u64 = 15;
        const BLOCKS: u32 = 1;

        let look_ahead = 20;

        let dir = TempDir::new().unwrap();
        let mut path = dir.path().to_path_buf();
        path.push(".bwk");
        maybe_create_dir(&path);
        let path = path.parent().unwrap().to_path_buf();

        let mnemonic = Mnemonic::generate(12).unwrap();
        let mut config = Config::new(
            Some(mnemonic.to_string()),
            "account_dir".to_string(),
            bitcoin::Network::Regtest,
            ScriptType::Segwit(ChildNumber::from_hardened_idx(0).unwrap()),
            path,
            ".bwk".to_string(),
            true,
        )
        .unwrap();
        config.network = Network::Regtest;
        config.look_ahead = look_ahead;
        config.set_electrum_url(url);
        config.set_electrum_port(port.to_string());
        config.set_mnemonic(mnemonic.to_string());
        let mut account: Account = Account::new(config);
        sleep(Duration::from_millis(300));

        let recv_addr = account.new_recv_addr();
        let change_addr = account.new_change_addr();

        send_to_address(&bitcoind, &recv_addr, Amount::from_btc(0.1).unwrap());
        generate(&bitcoind, BLOCKS);
        wait_until_timeout(
            || {
                let coins = account.coins();
                coins.len() == 1
            },
            TIMEOUT,
        );

        // Test change address
        send_to_address(&bitcoind, &change_addr, Amount::from_btc(0.1).unwrap());
        generate(&bitcoind, BLOCKS);
        wait_until_timeout(
            || {
                let coins = account.coins();
                coins.len() == 2
            },
            TIMEOUT,
        );

        // receive at look_ahead bound
        let recv_addr = account.recv_at(look_ahead);
        send_to_address(&bitcoind, &recv_addr, Amount::from_btc(0.1).unwrap());
        generate(&bitcoind, BLOCKS);
        wait_until_timeout(
            || {
                let coins = account.coins();
                coins.len() == 3
            },
            TIMEOUT,
        );

        // change at look_ahead bound
        let change_addr = account.change_at(look_ahead);
        send_to_address(&bitcoind, &change_addr, Amount::from_btc(0.1).unwrap());
        generate(&bitcoind, BLOCKS);
        wait_until_timeout(
            || {
                let coins = account.coins();
                coins.len() == 4
            },
            TIMEOUT,
        );

        let undiscovered_tip = account.recv_watch_tip() + 1;

        // receive beyond the look_ahead bound
        let recv_addr = account.recv_at(undiscovered_tip);
        send_to_address(&bitcoind, &recv_addr, Amount::from_btc(0.1).unwrap());
        generate(&bitcoind, BLOCKS);
        let coins = account.coins();
        // the coin is not detected for receiving address
        assert_eq!(coins.len(), 4);

        // change beyond the look_ahead bound
        let change_addr = account.change_at(undiscovered_tip);
        send_to_address(&bitcoind, &change_addr, Amount::from_btc(0.1).unwrap());
        generate(&bitcoind, BLOCKS);
        let coins = account.coins();
        // the coin is not detected for change address
        assert_eq!(coins.len(), 4);

        // move the watch tip forward
        account.new_recv_addr();
        account.new_recv_addr();
        wait_until_timeout(
            || {
                let coins = account.coins();
                coins.len() == 5
            },
            TIMEOUT,
        );

        account.new_change_addr();
        account.new_change_addr();
        wait_until_timeout(
            || {
                let coins = account.coins();
                coins.len() == 6
            },
            TIMEOUT,
        );
    }

    #[test]
    fn simple_reorg() {
        // setup_logger();
        let (url, port, mut electrsd, bitcoind) = bootstrap_electrs();
        generate(&bitcoind, 110);

        const TIMEOUT: u64 = 15;

        let look_ahead = 20;

        let dir = TempDir::new().unwrap();
        let mut path = dir.path().to_path_buf();
        path.push(".bwk");
        maybe_create_dir(&path);
        let path = path.parent().unwrap().to_path_buf();

        let mnemonic = Mnemonic::generate(12).unwrap();
        let mut config = Config::new(
            Some(mnemonic.to_string()),
            "account".to_string(),
            bitcoin::Network::Regtest,
            ScriptType::Segwit(ChildNumber::from_hardened_idx(0).unwrap()),
            path,
            ".bwk".to_string(),
            true,
        )
        .unwrap();
        config.look_ahead = look_ahead;
        config.set_electrum_url(url);
        config.set_electrum_port(port.to_string());
        config.set_mnemonic(mnemonic.to_string());
        let mut account: Account = Account::new(config);
        sleep(Duration::from_millis(300));

        let recv_addr = account.new_recv_addr();
        let change_addr = account.new_change_addr();

        sleep(Duration::from_secs(1));

        // send to recv address
        let recv_txid = send_to_address(&bitcoind, &recv_addr, Amount::from_btc(0.1).unwrap());
        let recv_tx = bitcoind
            .client
            .get_raw_transaction(&recv_txid, None)
            .unwrap();

        generate(&bitcoind, 1);

        sleep(Duration::from_secs(1));
        dump_logs(&mut electrsd);

        // send to change address
        let change_txid = send_to_address(&bitcoind, &change_addr, Amount::from_btc(0.1).unwrap());
        let change_tx = bitcoind
            .client
            .get_raw_transaction(&change_txid, None)
            .unwrap();
        generate(&bitcoind, 1);

        wait_until_timeout(
            || {
                let coins = account.coins();
                coins.len() == 2
            },
            TIMEOUT,
        );

        let coins = account.coins();
        let coins_height: BTreeMap<_, _> =
            coins.into_iter().map(|(c, e)| (c, e.height())).collect();

        // all coins are confirmed
        assert!(coins_height.iter().all(|(_, e)| e.is_some()));

        let height_before_reorg = get_block_height(&bitcoind);
        let h_before_reorg = get_block_hash(&bitcoind, height_before_reorg);

        sleep(Duration::from_secs(2));

        electrsd.clear_logs();
        log::warn!(" ------------------------------- reorg now ------------------------");
        reorg_chain(&bitcoind, 7);
        generate(&bitcoind, 2);
        dump_logs(&mut electrsd);
        sleep(Duration::from_secs(2));
        dump_logs(&mut electrsd);

        // FIXME:
        // NOTE: here we likely hitting an `electrs` bug:
        // - we can see in the electrs logs that 2 status (None) updates are assumed sent
        //   from electrs end
        // - only 1 status update is received on our raw client TCP stream end

        log::warn!(" ------------------------------- rebroadcast recv ------------------------");
        let _ = bitcoind.client.send_raw_transaction(&recv_tx);
        generate(&bitcoind, 1);
        sleep(Duration::from_secs(2));
        dump_logs(&mut electrsd);

        log::warn!(" ------------------------------- rebroadcast change ------------------------");
        let _ = bitcoind.client.send_raw_transaction(&change_tx);
        generate(&bitcoind, 1);
        sleep(Duration::from_secs(2));
        dump_logs(&mut electrsd);

        let new_h = get_block_hash(&bitcoind, height_before_reorg);
        assert_ne!(h_before_reorg, new_h);

        let coins = account.coins();
        // there is still 2 coins
        assert_eq!(coins.len(), 2);
    }

    #[cfg(feature = "test")]
    use bwk_tx::tx_builder::TxBuilder;

    #[cfg(feature = "test")]
    fn spend(
        account: &mut Account,
        builder: &mut TxBuilder,
        bitcoind: &BitcoinD,
        amount: u64,
    ) -> (bitcoin::Txid, u32) {
        let coins = account.spendable_coins().coins.into_values().collect();
        builder.new_template();
        builder.tx_template.inputs = coins;
        builder.dummy_external_output(amount);
        let mut psbt = builder.generate().unwrap();
        account.sign_psbt(&mut psbt);
        PsbtExt::finalize_mut(&mut psbt, &bitcoin::secp256k1::Secp256k1::new()).unwrap();
        let tx = psbt.extract_tx_unchecked_fee_rate();
        let txid = bitcoind.client.send_raw_transaction(&tx).unwrap();
        let blocks: u32 = random_range(2..15);
        generate(bitcoind, blocks);
        (txid, blocks)
    }

    fn receive(account: &mut Account, bitcoind: &BitcoinD, amount: u64) -> u32 {
        let recv_addr = account.new_recv_addr();
        send_to_address(bitcoind, &recv_addr, Amount::from_sat(amount));
        let blocks: u32 = random_range(2..15);
        generate(bitcoind, blocks);
        blocks
    }

    #[allow(unused)]
    fn sort_payments(payments: &Vec<Payment>) -> (usize, usize) {
        let mut recv = 0;
        let mut sent = 0;
        for p in payments {
            match p.payment_type {
                crate::coin_store::PaymentType::Receive => recv += 1,
                crate::coin_store::PaymentType::Send => sent += 1,
                crate::coin_store::PaymentType::ToSelf => {}
            }
        }
        (recv, sent)
    }

    #[cfg(feature = "test")]
    #[test]
    fn test_list_payments() {
        // setup_logger();
        let (url, port, _electrsd, bitcoind) = bootstrap_electrs();
        generate(&bitcoind, 100);

        let look_ahead = 20;

        let dir = TempDir::new().unwrap();
        let mut path = dir.path().to_path_buf();
        path.push(".bwk");
        maybe_create_dir(&path);
        let path = path.parent().unwrap().to_path_buf();

        let mnemonic = Mnemonic::generate(12).unwrap();
        let mut config = Config::new(
            Some(mnemonic.to_string()),
            "account_dir".to_string(),
            bitcoin::Network::Regtest,
            ScriptType::Segwit(ChildNumber::from_hardened_idx(0).unwrap()),
            path,
            ".bwk".to_string(),
            true,
        )
        .unwrap();
        config.network = Network::Regtest;
        config.look_ahead = look_ahead;
        config.set_electrum_url(url);
        config.set_electrum_port(port.to_string());
        config.set_mnemonic(mnemonic.to_string());
        let mut account = Account::new(config);
        sleep(Duration::from_millis(300));
        let mut builder = account.tx_builder();

        let blocks = receive(&mut account, &bitcoind, 200_000);
        wait_until_timeout(
            || {
                let coins = account.coins();
                coins.len() == 1
            },
            (blocks as u64) * 3,
        );
        let (_, blocks) = spend(&mut account, &mut builder, &bitcoind, 100_000);
        wait_until_timeout(
            || {
                let payments = account.payment_history();
                payments.len() == 2
            },
            (blocks as u64) * 3,
        );

        let payments = account.payment_history();
        assert_eq!(2, payments.len());
        let sorted = sort_payments(&payments);
        assert_eq!(sorted, (1, 1));
    }

    #[test]
    fn test_persist_payments() {
        use rand::random;

        // setup_logger();
        let (url, port, _electrsd, bitcoind) = bootstrap_electrs();
        generate(&bitcoind, 100);

        let look_ahead = 20;

        let dir = TempDir::new().unwrap();
        let mut path = dir.path().to_path_buf();
        path.push(".bwk");
        maybe_create_dir(&path);
        let path = path.parent().unwrap().to_path_buf();

        let mnemonic = Mnemonic::generate(12).unwrap();
        let mut config = Config::new(
            Some(mnemonic.to_string()),
            "account_dir".to_string(),
            bitcoin::Network::Regtest,
            ScriptType::Segwit(ChildNumber::from_hardened_idx(0).unwrap()),
            path,
            ".bwk".to_string(),
            true,
        )
        .unwrap();
        config.network = Network::Regtest;
        config.look_ahead = look_ahead;
        config.set_electrum_url(url);
        config.set_electrum_port(port.to_string());
        config.set_mnemonic(mnemonic.to_string());
        let saved_config = config.clone();
        // Scoped so `builder` and `account` drop in reverse
        // declaration order at the closing brace, the tx_builder
        // holds Arc<Mutex<CoinStore>> clones that would otherwise
        // keep the backend (and its DirLock on the account dir)
        // alive past account's explicit drop.
        {
            let mut account = Account::new(config);
            sleep(Duration::from_millis(300));
            let mut builder = account.tx_builder();

            let mut prev_blocks = receive(&mut account, &bitcoind, 100_000_000);
            for _ in 0..15 {
                wait_until_timeout(
                    || !account.spendable_coins().coins.is_empty(),
                    (prev_blocks as u64) * 3,
                );
                sleep(Duration::from_millis(1000));
                let coins = account.spendable_coins();
                let balance = coins
                    .coins
                    .into_iter()
                    .fold(0, |a, (_, c)| a + c.txout.value.to_sat());
                assert!(balance > 1_100_000);
                let pay: bool = random();
                if pay {
                    let blocks: u32 = random_range(1..5);
                    let addr = bitcoind
                        .client
                        .get_new_address(None, None)
                        .unwrap()
                        .assume_checked();
                    let mut psbt = builder
                        .pay(random_range(10_000..1_000_000), addr, 1000)
                        .unwrap();
                    account.sign_psbt(&mut psbt);
                    PsbtExt::finalize_mut(&mut psbt, &bitcoin::secp256k1::Secp256k1::new())
                        .unwrap();
                    let tx = psbt.extract_tx_unchecked_fee_rate();
                    let _txid = bitcoind.client.send_raw_transaction(&tx).unwrap();
                    generate(&bitcoind, blocks);
                    prev_blocks = blocks;
                } else {
                    prev_blocks = receive(&mut account, &bitcoind, random_range(10_000..1_000_000));
                }
            }
            wait_until_timeout(
                || {
                    let payments = account.payment_history();
                    payments.len() == 15
                },
                (prev_blocks as u64) * 3,
            );
            sleep(Duration::from_secs(3));
            let payments = account.payment_history();
            assert_eq!(payments.len(), 16);
        }

        let account: Account = Account::new(saved_config);
        sleep(Duration::from_millis(300));
        let payments = account.payment_history();
        assert_eq!(payments.len(), 16);
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod sqlite_signer_exclusion {
    use super::*;
    use crate::config::{Config, CONFIG_FILENAME};
    use bip39::Mnemonic;
    use bwk_descriptor::descriptor::ScriptType;
    use bwk_persist::{FileConfigStore, PersistenceKind};
    use miniscript::bitcoin::{bip32::ChildNumber, Network};
    use temp_dir::TempDir;

    /// Recursively scan all files under `dir` and assert `needle` is not
    /// present in any of their bytes (text or binary).
    fn assert_needle_absent(dir: &std::path::Path, needle: &str) {
        let needle_bytes = needle.as_bytes();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(p) = stack.pop() {
            for entry in std::fs::read_dir(&p).expect("read_dir") {
                let entry = entry.expect("dir entry");
                let path = entry.path();
                let ft = entry.file_type().expect("file_type");
                if ft.is_dir() {
                    stack.push(path);
                } else if ft.is_file() {
                    let bytes = std::fs::read(&path).expect("read file");
                    let found = bytes.windows(needle_bytes.len()).any(|w| w == needle_bytes);
                    assert!(
                        !found,
                        "needle {needle:?} found in on-disk file {}",
                        path.display()
                    );
                }
            }
        }
    }

    #[test]
    fn sqlite_mode_keeps_mnemonic_off_disk() {
        let temp = TempDir::new().expect("tempdir");
        let unique = Mnemonic::generate(12).expect("mnemonic").to_string();

        let mut cfg = Config::new(
            Some(unique.clone()),
            "alice".to_string(),
            Network::Regtest,
            ScriptType::Segwit(ChildNumber::from_hardened_idx(0).unwrap()),
            temp.path().to_path_buf(),
            "wallet".to_string(),
            true,
        )
        .expect("config");
        cfg.set_offline(true);
        cfg.persist_kind = PersistenceKind::Sqlite;

        // Wire a FileConfigStore against the account dir's config.json,
        // build the account, drive a config save + a label write to
        // exercise multiple persist paths. SQLite mode must not write
        // the mnemonic anywhere under the account dir.
        let account_dir = cfg.account_dir();
        let config_store: Arc<dyn ConfigStore<Config>> = Arc::new(FileConfigStore::<Config>::new(
            account_dir.join(CONFIG_FILENAME),
        ));
        let account: Account = Account::with_config_store(cfg.clone(), config_store);
        account.persist_config();
        account.label_store.lock().expect("poisoned").persist();
        drop(account);

        assert!(account_dir.exists(), "account dir created");
        assert_needle_absent(&account_dir, &unique);
    }

    #[test]
    fn json_mode_writes_mnemonic_to_config_json() {
        let temp = TempDir::new().expect("tempdir");
        let unique = Mnemonic::generate(12).expect("mnemonic").to_string();

        let cfg = Config::new(
            Some(unique.clone()),
            "alice".to_string(),
            Network::Regtest,
            ScriptType::Segwit(ChildNumber::from_hardened_idx(0).unwrap()),
            temp.path().to_path_buf(),
            "wallet".to_string(),
            true,
        )
        .expect("config")
        .with_persist_kind(PersistenceKind::Json);

        let account_dir = cfg.account_dir();
        let config_path = account_dir.join(CONFIG_FILENAME);
        let config_store: Arc<dyn ConfigStore<Config>> =
            Arc::new(FileConfigStore::<Config>::new(config_path.clone()));
        let account: Account = Account::with_config_store(cfg.clone(), config_store);
        account.persist_config();
        drop(account);

        let on_disk = std::fs::read_to_string(&config_path).expect("config.json");
        assert!(
            on_disk.contains(&unique),
            "mnemonic must appear in config.json under JSON mode (default)"
        );
    }
}
