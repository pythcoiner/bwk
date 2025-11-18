use std::{
    collections::BTreeMap,
    path::PathBuf,
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
use bwk_sign::{signing_manager::SigningManager, HotSigner, Signer};
use bwk_tx::{coin::KeyChain, tx_builder::TxBuilder, Coin};
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
    tx_store::{TxEntry, TxStore},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
pub enum AddrAccount {
    Receive,
    Change,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CoinState {
    pub coins: BTreeMap<OutPoint, Coin>,
    pub confirmed_coins: usize,
    pub confirmed_balance: u64,
    pub unconfirmed_coins: usize,
    pub unconfirmed_balance: u64,
}

/// Represents different types of errors that can occur.
#[derive(Debug)]
pub enum Notification {
    Electrum(TxListenerNotif),
    AddressTipChanged,
    CoinUpdate,
    InvalidElectrumConfig,
    InvalidLookAhead,
    Stopped,
    Error(Error),
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

#[derive(Debug)]
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

#[derive(Debug)]
pub struct Account {
    coin_store: Arc<Mutex<CoinStore>>,
    label_store: Arc<Mutex<LabelStore>>,
    receiver: Option<mpsc::Receiver<Notification>>,
    sender: mpsc::Sender<Notification>,
    tx_listener: Option<JoinHandle<()>>,
    config: Config,
    electrum_stop: Option<Arc<AtomicBool>>,
    signing_manager: SigningManager,
}

impl Drop for Account {
    fn drop(&mut self) {
        if let Some(stop) = self.electrum_stop.as_mut() {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

// Constructor
impl Account {
    /// Creates a new `Account` instance with the given configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - The configuration for the account.
    ///
    /// # Returns
    ///
    /// A new `Account` instance.
    pub fn new(config: Config) -> Self {
        assert!(!config.account.is_empty());
        let (sender, receiver) = mpsc::channel();
        let tx_data = if config.persist {
            TxStore::store_from_file(config.transactions_path())
        } else {
            BTreeMap::new()
        };
        let tx_store =
            TxStore::new(tx_data, Some(config.transactions_path())).enable_persist(config.persist);
        let (receive, change) = if config.persist {
            let Tip { receive, change } = config.tip_from_file();
            (receive, change)
        } else {
            (0, 0)
        };
        let label_store = Arc::new(Mutex::new(
            LabelStore::from_file(config.clone()).enable_persist(config.persist),
        ));
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
        )));
        coin_store.lock().expect("poisoned").generate();
        let mut signing_manager =
            SigningManager::new(PathBuf::new(), config.dir_name()).enable_persist(config.persist);
        if let Some(mnemo) = config.mnemonic.clone() {
            signing_manager.new_hot_signer_from_mnemonic(config.network(), mnemo);
        }
        let mut account = Account {
            coin_store,
            label_store,
            tx_listener: None,
            electrum_stop: None,
            receiver: Some(receiver),
            sender,
            config,
            signing_manager,
        };
        account.start_electrum();
        account
    }
}

// Non (b)locking API
impl Account {
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

    pub fn hot_signer(&self) -> Option<HotSigner> {
        let mnemonic_str = self.config.mnemonic.clone()?;
        let mut signer = HotSigner::new_from_mnemonics(self.network(), &mnemonic_str).ok()?;
        signer.register_descriptor(self.descriptor());
        Some(signer)
    }

    pub fn sign(&self, psbt: String) {
        self.signing_manager.sign(self.config.network(), psbt);
    }
}

// Locking API
impl Account {
    pub fn tx_builder(&self) -> Result<TxBuilder, bwk_tx::transaction::Error> {
        let tip_handle = Box::new(ChangeTipUpdater::new(
            self.coin_store.lock().expect("poisoned").address_store(),
        ));
        let coin_source = Box::new(CoinStoreSource::new(self.coin_store.clone()));
        Ok(TxBuilder::new(self.descriptor(), tip_handle, self.network())?.coin_source(coin_source))
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
impl Account {
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
}

// Electrum specific implementation
impl Account {
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
        config: Config,
    ) -> (mpsc::Sender<AddressTip>, Arc<AtomicBool>) {
        log::debug!("Account::start_poll_txs()");
        let (sender, address_tip) = mpsc::channel();
        let coin_store = self.coin_store.clone();
        let notification = self.sender.clone();
        let derivator = self.derivator();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_request = stop.clone();

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
                Some(config),
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
            self.config.to_file();
        } else {
            self.sender
                .send(Notification::InvalidElectrumConfig)
                .expect("cannot fail");
        }
    }

    /// Starts the Electrum listener for the account.
    pub fn start_electrum(&mut self) {
        if let (None, Some(addr), Some(port)) = (
            &self.tx_listener,
            self.config.electrum_url.clone(),
            self.config.electrum_port,
        ) {
            let (tx_listener, electrum_stop) =
                self.start_listen_txs(addr, port, self.config.clone());
            self.coin_store.lock().expect("poisoned").init(tx_listener);
            self.electrum_stop = Some(electrum_stop);
        }
    }

    /// Stops the Electrum listener for the account.
    pub fn stop_electrum(&mut self) {
        if let Some(stop) = self.electrum_stop.as_mut() {
            stop.store(true, Ordering::Relaxed);
        }
        self.electrum_stop = None;
    }

    /// Sets the look-ahead value for the account.
    ///
    /// # Arguments
    ///
    /// * `look_ahead` - The look-ahead value to set.
    pub fn set_look_ahead(&mut self, look_ahead: String) {
        if let Ok(la) = look_ahead.parse::<u32>() {
            self.config.look_ahead = la;
            self.config.to_file();
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
fn listen_txs<T: From<TxListenerNotif>>(
    coin_store: Arc<Mutex<CoinStore>>,
    derivator: SpkDerivator,
    notification: mpsc::Sender<T>,
    address_tip: mpsc::Receiver<AddressTip>,
    stop_request: Arc<AtomicBool>,
    request: mpsc::Sender<CoinRequest>,
    response: mpsc::Receiver<CoinResponse>,
    config: Option<Config>,
) {
    log::info!("listen_txs(): started");
    send_notif!(notification, request, TxListenerNotif::Started);

    let mut statuses = if let Some(config) = &config {
        config.statuses_from_file()
    } else {
        BTreeMap::<ScriptBuf, (Option<String>, u32, u32)>::new()
    };

    if !statuses.is_empty() {
        let sub: Vec<_> = statuses.keys().cloned().collect();
        send_electrum!(request, notification, CoinRequest::Subscribe(sub));
    }

    fn persist_status(
        config: &Option<Config>,
        statuses: &BTreeMap<ScriptBuf, (Option<String>, u32, u32)>,
    ) {
        if let Some(cfg) = config.as_ref() {
            cfg.persist_statuses(statuses);
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
                if !statuses.contains_key(&r_spk) {
                    // FIXME: here we can be smart an not start at 0 but at `actual_tip`
                    for i in 0..recv {
                        let spk = derivator.receive_at(i).script_pubkey();
                        if !statuses.contains_key(&spk) {
                            statuses.insert(spk.clone(), (None, 0, i));
                            persist_status(&config, &statuses);
                            sub.push(spk);
                        }
                    }
                }
                let c_spk = derivator.change_at(recv).script_pubkey();
                if !statuses.contains_key(&c_spk) {
                    // FIXME: here we can be smart an not start at 0 but at `actual_tip`
                    for i in 0..change {
                        let spk = derivator.change_at(i).script_pubkey();
                        if !statuses.contains_key(&spk) {
                            statuses.insert(spk.clone(), (None, 1, i));
                            persist_status(&config, &statuses);
                            sub.push(spk);
                        }
                    }
                }
                if !sub.is_empty() {
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
                        for (spk, status) in elct_status {
                            if let Some((s, _, _)) = statuses.get_mut(&spk) {
                                // status is registered
                                if *s != status {
                                    // status changed
                                    if status.is_some() {
                                        // status is not empty so we ask for txs changes
                                        history.push(spk);
                                    } else {
                                        // status change from Some(_) to None we directly update
                                        // coin_store
                                        let mut store = coin_store.lock().expect("poisoned");
                                        let mut map = BTreeMap::new();
                                        map.insert(spk.clone(), vec![]);
                                        let _ = store.handle_history_response(map);
                                        store.generate();
                                    }
                                    // record the local status change
                                    *s = status;
                                }
                            } else if status.is_some() {
                                // status is not None & not registered
                                statuses.entry(spk.clone()).and_modify(|s| s.0 = status);
                                persist_status(&config, &statuses);
                                history.push(spk);
                            } else {
                                // status is None & not registered

                                // record local status
                                statuses.entry(spk.clone()).and_modify(|s| s.0 = status);
                                persist_status(&config, &statuses);

                                // update coin_store
                                let mut store = coin_store.lock().expect("poisoned");
                                let mut map = BTreeMap::new();
                                map.insert(spk.clone(), vec![]);
                                let _ = store.handle_history_response(map);
                            }
                        }
                        if !history.is_empty() {
                            let hist = CoinRequest::History(history);
                            log::debug!("listen_txs() send {:#?}", hist);
                            send_electrum!(request, notification, hist);
                        }
                        persist_status(&config, &statuses);
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
    use bwk_sign::hot_signer::HotSigner;
    use bwk_tx::CoinStatus;
    use bwk_utils::test::{funding_tx, setup_logger, spending_tx};
    use miniscript::bitcoin::{bip32::ChildNumber, Network};
    use std::{str::FromStr, sync::mpsc::TryRecvError, time::Duration};
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
                "",
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

            let tx_store = TxStore::new(Default::default(), None);
            let label_store = Arc::new(Mutex::new(LabelStore::new()));
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
            )));
            coin_store.lock().expect("poisoned").init(tip_sender);
            let store = coin_store.clone();
            let cloned_stop = stop.clone();
            let cloned_derivator = derivator.clone();

            let listener_handle = thread::spawn(move || {
                listen_txs(
                    coin_store,
                    cloned_derivator,
                    notif_sender,
                    tip_receiver,
                    stop,
                    req_sender,
                    resp_receiver,
                    None,
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

        const TIMEOUT: u64 = 5;
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
            ".bwk",
            true,
        )
        .unwrap();
        config.network = Network::Regtest;
        config.look_ahead = look_ahead;
        config.set_electrum_url(url);
        config.set_electrum_port(port.to_string());
        config.set_mnemonic(mnemonic.to_string());
        let mut account = Account::new(config);
        account.start_electrum();
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

        const TIMEOUT: u64 = 5;

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
            ".bwk",
            true,
        )
        .unwrap();
        config.look_ahead = look_ahead;
        config.set_electrum_url(url);
        config.set_electrum_port(port.to_string());
        config.set_mnemonic(mnemonic.to_string());
        let mut account = Account::new(config);
        account.start_electrum();
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
    ) -> bitcoin::Txid {
        let coins = account.spendable_coins().coins.into_values().collect();
        builder.new_template();
        builder.tx_template.inputs = coins;
        builder.dummy_external_output(amount);
        let mut psbt = builder.generate().unwrap();
        let signer = account.hot_signer().unwrap();
        signer.sign(&mut psbt);
        let tx = signer.finalize(&mut psbt).unwrap();
        let txid = bitcoind.client.send_raw_transaction(&tx).unwrap();
        let blocks: u32 = random_range(2..15);
        generate(bitcoind, blocks);
        txid
    }

    fn receive(account: &mut Account, bitcoind: &BitcoinD, amount: u64) {
        let recv_addr = account.new_recv_addr();
        send_to_address(bitcoind, &recv_addr, Amount::from_sat(amount));
        let blocks: u32 = random_range(2..15);
        generate(bitcoind, blocks);
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
            ".bwk",
            true,
        )
        .unwrap();
        config.network = Network::Regtest;
        config.look_ahead = look_ahead;
        config.set_electrum_url(url);
        config.set_electrum_port(port.to_string());
        config.set_mnemonic(mnemonic.to_string());
        let mut account = Account::new(config);
        account.start_electrum();
        sleep(Duration::from_millis(300));
        let mut builder =
            TxBuilder::new_standalone(account.descriptor(), Network::Regtest).unwrap();

        receive(&mut account, &bitcoind, 200_000);
        wait_until_timeout(
            || {
                let coins = account.coins();
                coins.len() == 1
            },
            5,
        );
        spend(&mut account, &mut builder, &bitcoind, 100_000);
        wait_until_timeout(
            || {
                let payments = account.payment_history();
                payments.len() == 2
            },
            5,
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
            ".bwk",
            true,
        )
        .unwrap();
        config.network = Network::Regtest;
        config.look_ahead = look_ahead;
        config.set_electrum_url(url);
        config.set_electrum_port(port.to_string());
        config.set_mnemonic(mnemonic.to_string());
        let saved_config = config.clone();
        let mut account = Account::new(config);
        sleep(Duration::from_millis(300));
        let mut builder = account.tx_builder().unwrap();
        let signer = account.hot_signer().unwrap();

        receive(&mut account, &bitcoind, 100_000_000);
        for _ in 0..15 {
            wait_until_timeout(|| !account.spendable_coins().coins.is_empty(), 5);
            sleep(Duration::from_millis(1000));
            let coins = account.spendable_coins();
            let balance = coins
                .coins
                .into_iter()
                .fold(0, |a, (_, c)| a + c.txout.value.to_sat());
            assert!(balance > 1_100_000);
            let pay: bool = random();
            if pay {
                let addr = bitcoind
                    .client
                    .get_new_address(None, None)
                    .unwrap()
                    .assume_checked();
                let mut psbt = builder
                    .pay(random_range(10_000..1_000_000), addr, 1000)
                    .unwrap();
                signer.sign(&mut psbt);
                let tx = signer.finalize(&mut psbt).unwrap();
                let _txid = bitcoind.client.send_raw_transaction(&tx).unwrap();
                generate(&bitcoind, random_range(1..5));
            } else {
                receive(&mut account, &bitcoind, random_range(10_000..1_000_000));
            }
        }
        wait_until_timeout(
            || {
                let payments = account.payment_history();
                payments.len() == 15
            },
            5,
        );
        sleep(Duration::from_secs(3));
        let payments = account.payment_history();
        assert_eq!(payments.len(), 16);
        drop(account);

        let account = Account::new(saved_config);
        sleep(Duration::from_millis(300));
        let payments = account.payment_history();
        assert_eq!(payments.len(), 16);
    }
}
