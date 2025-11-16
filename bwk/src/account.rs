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
    bitcoin::{self, OutPoint, ScriptBuf, TxOut},
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

// Rust only interface
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

    pub fn receiver(&mut self) -> Option<mpsc::Receiver<Notification>> {
        self.receiver.take()
    }

    /// Returns the derivator associated with the account.
    ///
    /// # Returns
    ///
    /// A `Derivator` instance.
    pub fn derivator(&self) -> SpkDerivator {
        self.coin_store.lock().expect("poisoned").derivator()
    }

    /// Returns a map of coins associated with the account.
    ///
    /// # Returns
    ///
    /// A `BTreeMap` of `OutPoint` to `CoinEntry`.
    pub fn coins(&self) -> BTreeMap<OutPoint, CoinEntry> {
        self.coin_store.lock().expect("poisoned").coins()
    }

    /// Returns the receiving address at the specified index.
    ///
    /// # Arguments
    ///
    /// * `index` - The index of the receiving address.
    ///
    /// # Returns
    ///
    /// A `bitcoin::Address` instance.
    pub fn recv_at(&self, index: u32) -> bitcoin::Address {
        self.coin_store
            .lock()
            .expect("poisoned")
            .derivator_ref()
            .receive_at(index)
    }

    /// Returns the change address at the specified index.
    ///
    /// # Arguments
    ///
    /// * `index` - The index of the change address.
    ///
    /// # Returns
    ///
    /// A `bitcoin::Address` instance.
    pub fn change_at(&self, index: u32) -> bitcoin::Address {
        self.coin_store
            .lock()
            .expect("poisoned")
            .derivator_ref()
            .change_at(index)
    }

    /// Generates a new receiving address for the account.
    ///
    /// # Returns
    ///
    /// A `bitcoin::Address` instance.
    pub fn new_recv_addr(&mut self) -> bitcoin::Address {
        self.coin_store.lock().expect("poisoned").new_recv_addr()
    }
    /// Generates a new change address for the account.
    ///
    /// # Returns
    ///
    /// A `bitcoin::Address` instance.
    pub fn new_change_addr(&mut self) -> bitcoin::Address {
        self.coin_store.lock().expect("poisoned").new_change_addr()
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

    /// Re-generate coin_store from tx_store
    pub fn generate_coins(&mut self) {
        self.coin_store.lock().expect("poisoned").generate();
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

    /// Returns the coin matching the given outpoint if found, else None.
    pub fn get_coin(&self, outpoint: &OutPoint) -> Option<Coin> {
        self.coin_store
            .lock()
            .expect("poisoned")
            .get(outpoint)
            .map(|e| e.coin)
    }

    /// We uses receive address at index 0 as dummy spk
    fn dummy_spk(&self) -> ScriptBuf {
        self.derivator().receive_spk_at(0)
    }

    pub fn change_index(&self, tx: &bitcoin::Transaction) -> Option<usize> {
        let dummy_spk = self.dummy_spk();
        for (index, TxOut { script_pubkey, .. }) in tx.output.iter().enumerate() {
            if *script_pubkey == dummy_spk {
                return Some(index);
            }
        }
        None
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

    /// Returns the configuration of the account.
    ///
    /// # Returns
    ///
    /// A boxed `Config` instance.
    pub fn get_config(&self) -> Config {
        self.config.clone()
    }

    /// Returns the receiving address at the specified index as a string.
    ///
    /// # Arguments
    ///
    /// * `index` - The index of the receiving address.
    ///
    /// # Returns
    ///
    /// The receiving address as a string.
    pub fn recv_addr_at(&self, index: u32) -> String {
        self.coin_store
            .lock()
            .expect("poisoned")
            .derivator_ref()
            .receive_at(index)
            .to_string()
    }

    /// Returns the change address at the specified index as a string.
    ///
    /// # Arguments
    ///
    /// * `index` - The index of the change address.
    ///
    /// # Returns
    ///
    /// The change address as a string.
    pub fn change_addr_at(&self, index: u32) -> String {
        self.coin_store
            .lock()
            .expect("poisoned")
            .derivator_ref()
            .change_at(index)
            .to_string()
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

    /// Edits the label of a coin identified by the given outpoint.
    ///
    /// # Arguments
    ///
    /// * `outpoint` - A string representation of the outpoint for the coin.
    /// * `label` - The new label to set for the coin. If the label is empty, the label will be removed.
    pub fn edit_coin_label(&self, outpoint: String, label: String) {
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

    pub fn sign(&self, psbt: String) {
        self.signing_manager.sign(self.config.network(), psbt);
    }

    pub fn hot_signer(&self) -> Option<HotSigner> {
        let mnemonic_str = self.config.mnemonic.clone()?;
        let mut signer = HotSigner::new_from_mnemonics(self.network(), &mnemonic_str).ok()?;
        signer.register_descriptor(self.descriptor());
        Some(signer)
    }

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
