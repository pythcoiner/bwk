use bwk_descriptor::derivator::SpkDerivator;
use bwk_tx::{
    coin::{self, KeyChain},
    transaction::max_input_satisfaction_size,
    tx_builder::CoinSource,
    Coin, CoinStatus,
};
use miniscript::{
    bitcoin::{self, address::NetworkUnchecked, OutPoint, ScriptBuf, Sequence, Txid},
    Descriptor, DescriptorPublicKey,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    sync::{mpsc, Arc, Mutex},
};

use crate::{
    account::{CoinState, Notification},
    address_store::{AddressEntry, AddressStatus, AddressStore, AddressTip},
    config::Config,
    label_store::{LabelKey, LabelStore},
    profile::{DefaultBackend, RamProfile, StorageProfile},
    tx_store::{InputMetadata, OutputMetadata, TxEntry, TxStore},
};

#[derive(Debug, Clone)]
pub enum PaymentType {
    Receive,
    Send,
    ToSelf,
}
#[derive(Debug, Clone)]
pub struct Payment {
    pub txid: String,
    pub payment_type: PaymentType,
    pub amount: u64,
    pub label: String,
}

impl From<TxEntry> for Payment {
    fn from(value: TxEntry) -> Self {
        // FIXME: handle ToSelf
        assert!(value.is_complete());
        let inputs = value.inputs.iter().fold(0, |a, (_, b)| {
            let v = if b.owned { b.value.unwrap_or(0) } else { 0 };
            a + v
        });
        let mut outputs = 0;
        for index in 0..value.tx().output.len() {
            let amount = value.tx().output[index].value.to_sat();
            let owned = value.outputs.get(&index).map(|o| o.owned).unwrap_or(false);
            if owned {
                outputs += amount;
            }
        }
        let (payment_type, amount) = if inputs > outputs {
            (PaymentType::Send, inputs - outputs)
        } else {
            (PaymentType::Receive, outputs - inputs)
        };
        let txid = bitcoin::consensus::encode::serialize_hex(&value.tx().compute_txid());
        Self {
            txid,
            payment_type,
            amount,
            label: String::new(),
        }
    }
}

pub struct CoinStoreSource<P: StorageProfile = RamProfile<DefaultBackend>>(
    Arc<Mutex<CoinStore<P>>>,
);

impl<P: StorageProfile> CoinStoreSource<P> {
    pub fn new(store: Arc<Mutex<CoinStore<P>>>) -> Self {
        Self(store)
    }
}

impl<P: StorageProfile> CoinSource for CoinStoreSource<P> {
    fn spendable_coins(&self) -> Vec<Coin> {
        self.0
            .lock()
            .expect("poisoned")
            .spendable_coins()
            .coins
            .into_values()
            .collect()
    }
}

#[derive(Debug)]
/// Represents a store for managing coins and their associated data.
///
/// The `CoinStore` is generated from the transaction store after every
/// TxStore update and acts as a cache for coins. It maintains mappings
/// of outpoints to coin entries and tracks the history of script public
/// keys (SPKs).
pub struct CoinStore<P: StorageProfile = RamProfile<DefaultBackend>> {
    store: BTreeMap<OutPoint, CoinEntry>,
    label_store: Arc<Mutex<LabelStore<P>>>,
    spk_to_outpoint: BTreeMap<ScriptBuf, HashSet<OutPoint>>,
    address_store: Arc<Mutex<AddressStore<P>>>,
    tx_store: TxStore<P>,
    spk_history: BTreeMap<ScriptBuf, SpkHistory>,
    updates: Vec<Update>,
    derivator: SpkDerivator,
    notification: mpsc::Sender<Notification>,
    config: Config,
}

#[derive(Debug, Default)]
/// Represents the history of transactions for a specific script public key (SPK).
///
/// The `SpkHistory` stores a history of txids and their associated
/// heights, allowing for tracking incremental changes over time.
pub struct SpkHistory {
    history: Vec<BTreeMap<bitcoin::Txid, Option<u64>>>,
}

#[derive(Debug, Default)]
/// Represents the differences in transaction history for a script public key.
///
/// The `HistoryDiff` struct contains the added, changed, and removed
/// transactions, allowing for easy tracking of updates to the SPK history.
pub struct HistoryDiff {
    pub added: BTreeMap<bitcoin::Txid, Option<u64>>,
    pub changed: BTreeMap<bitcoin::Txid, Option<u64>>,
    pub removed: BTreeMap<bitcoin::Txid, Option<u64>>,
}

impl SpkHistory {
    /// Creates a new instance of `SpkHistory`.
    ///
    /// This method initializes the history with default values.
    pub fn new() -> Self {
        Self {
            history: vec![BTreeMap::default()],
        }
    }
    /// Inserts new transaction data into the SPK history and returns the differences.
    ///
    /// This method compares the new transaction data with the existing history
    /// and returns a `HistoryDiff` struct indicating added, changed, and removed
    /// transactions.
    pub fn insert(&mut self, new: Vec<(bitcoin::Txid, Option<u64>)>) -> HistoryDiff {
        let new: BTreeMap<_, _> = new.into_iter().collect();
        assert!(!self.history.is_empty());

        // last state have no txs
        let diff = if self.history.last().expect("not empty").is_empty() {
            if new.is_empty() {
                HistoryDiff::default()
            } else {
                self.history.push(new.clone());
                HistoryDiff {
                    added: new.clone(),
                    ..Default::default()
                }
            }
        } else {
            let mut diff = HistoryDiff::default();
            {
                let previous = self.history.last().expect("at least one element");

                new.iter().for_each(|(txid, height)| {
                    if !previous.contains_key(txid) {
                        diff.added.insert(*txid, *height);
                    } else {
                        let prev_height = previous.get(txid).expect("present");
                        if height != prev_height {
                            diff.changed.insert(*txid, *height);
                        }
                    }
                });

                previous.iter().for_each(|(txid, height)| {
                    if !new.contains_key(txid) {
                        diff.removed.insert(*txid, *height);
                    }
                });
            }
            // FIXME: do not insert if last == new
            self.history.push(new);
            diff
        };
        diff
    }
}

impl<P: StorageProfile> CoinStore<P> {
    /// Creates a new instance of `CoinStore`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        network: bitcoin::Network,
        descriptor: Descriptor<DescriptorPublicKey>,
        notification: mpsc::Sender<Notification>,
        recv_tip: u32,
        change_tip: u32,
        look_ahead: u32,
        tx_store: TxStore<P>,
        label_store: Arc<Mutex<LabelStore<P>>>,
        config: Config,
        account_store: Arc<Mutex<P::AccountStore>>,
    ) -> Self {
        let derivator = SpkDerivator::new(descriptor, network).unwrap();
        let address_store = Arc::new(Mutex::new(AddressStore::with_account_store(
            derivator.clone(),
            notification.clone(),
            recv_tip,
            change_tip,
            look_ahead,
            config.clone(),
            account_store,
        )));
        Self {
            store: BTreeMap::new(),
            spk_to_outpoint: BTreeMap::new(),
            address_store,
            label_store,
            tx_store,
            updates: Vec::new(),
            spk_history: BTreeMap::new(),
            notification,
            derivator,
            config,
        }
    }

    /// Initializes the address store with a channel to the tx listener.
    ///
    /// This method sets up the address store to send updates to the
    /// specified transaction listener.
    pub fn init(&mut self, tx_listener: mpsc::Sender<AddressTip>) {
        self.address_store
            .lock()
            .expect("poisoned")
            .init(tx_listener);
    }
    /// Returns a clone of the derivator used for generating addresses.
    ///
    /// # Returns
    /// A `Derivator` instance.
    pub fn derivator(&self) -> SpkDerivator {
        self.derivator.clone()
    }
    /// Returns a reference to the derivator used for generating addresses.
    ///
    /// # Returns
    /// A reference to a `SpkDerivator`.
    pub fn derivator_ref(&self) -> &SpkDerivator {
        &self.derivator
    }
    /// Returns the current receiving watch tip index.
    ///
    /// # Returns
    /// The index of the last generated receiving address.
    pub fn recv_watch_tip(&self) -> u32 {
        self.address_store
            .lock()
            .expect("poisoned")
            .recv_watch_tip()
    }

    /// Returns the current change watch tip index.
    ///
    /// # Returns
    /// The index of the last generated change address.
    pub fn change_watch_tip(&self) -> u32 {
        self.address_store
            .lock()
            .expect("poisoned")
            .change_watch_tip()
    }

    /// Generates a new receiving address.
    ///
    /// # Returns
    /// A new `bitcoin::Address` for receiving funds.
    pub fn new_recv_addr(&mut self) -> bitcoin::Address {
        self.address_store.lock().expect("poisoned").new_recv_addr()
    }

    /// Returns the current receiving address tip index.
    ///
    /// # Returns
    /// The index of the last generated receiving address.
    pub fn recv_tip(&self) -> u32 {
        self.address_store.lock().expect("poisoned").recv_tip()
    }

    /// Generates a new change address.
    ///
    /// # Returns
    /// A new `bitcoin::Address` for change outputs.
    pub fn new_change_addr(&mut self) -> bitcoin::Address {
        self.address_store
            .lock()
            .expect("poisoned")
            .new_change_addr()
    }

    /// Retrieves information about an address associated with the given script public key (SPK).
    ///
    /// This method queries the address store to find the entry corresponding to the provided SPK.
    ///
    /// # Parameters
    /// - `spk`: A reference to the `ScriptBuf` representing the script public key for which to retrieve the address information.
    ///
    /// # Returns
    /// An `Option<AddressEntry>` containing the address information if found, or `None` if no entry exists for the given SPK.
    pub fn address_info(&self, spk: &ScriptBuf) -> Option<AddressEntry> {
        self.address_store.lock().expect("poisoned").get_entry(spk)
    }

    /// Processes a received coin at the specified script public key.
    ///
    /// # Parameters
    /// - `spk`: The script public key of the received coin.
    pub fn recv_coin_at(&mut self, spk: &ScriptBuf) {
        self.address_store
            .lock()
            .expect("poisoned")
            .recv_coin_at(spk);
    }

    /// Handles the response containing transaction history for SPKs.
    ///
    /// This method processes the history and updates the internal state of the
    /// `CoinStore`. It returns a list of transaction IDs that are missing.
    ///
    /// # Parameters
    /// - `hist`: A map of script public keys to their transaction history.
    ///
    /// # Returns
    /// A vector of `Txid` representing missing transactions.
    pub fn handle_history_response(
        &mut self,
        hist: BTreeMap<ScriptBuf, Vec<(bitcoin::Txid, Option<u64>)>>,
    ) -> (bool /* height_updated */, Vec<Txid>) {
        let mut updates = vec![];
        let mut height_updated = false;

        // generate diff & drop double spent txs
        for (spk, history) in hist {
            self.recv_coin_at(&spk);
            let update = self.update_spk_history(spk, history);
            updates.push(update);
        }

        let mut updates: Vec<_> = updates
            .into_iter()
            .map(|(height, upd)| {
                if height {
                    height_updated = true;
                }
                upd
            })
            .collect();

        {
            // pre fill with tx we already have
            let store = &self.tx_store;
            for upd in &mut updates {
                for tx in &mut upd.txs {
                    if let Some(store_tx) = store.inner_get(&tx.0) {
                        tx.1 = Some(store_tx);
                    }
                }
            }
        } // <- release tx_store ref

        // request missing txs
        let mut txids = vec![];
        for upd in &updates {
            txids.append(&mut upd.missing());
        }

        {
            // apply updates that are already completes
            let store = &mut self.tx_store;
            updates = updates
                .into_iter()
                .filter_map(|u| {
                    if u.is_complete() {
                        store.insert_updates(vec![u]);
                        None
                    } else {
                        Some(u)
                    }
                })
                .collect();
        } // <- release &mut tx_store

        self.updates.append(&mut updates);
        (height_updated, txids)
    }

    /// Updates the history for a specific script public key (SPK).
    ///
    /// This method generates a diff of the SPK history and updates the
    /// transaction store accordingly.
    ///
    /// # Parameters
    /// - `spk`: The script public key to update.
    /// - `history`: The new transaction history for the SPK.
    ///
    /// # Returns
    /// An `Update` representing the changes made to the SPK history.
    ///
    /// Note: triggered on history_get response
    pub fn update_spk_history(
        &mut self,
        spk: ScriptBuf,
        history: Vec<(Txid, Option<u64> /* height */)>,
    ) -> (bool /* height_updated */, Update) {
        // insert a blank history if no one
        if !self.spk_history.contains_key(&spk) {
            self.spk_history.insert(spk.clone(), SpkHistory::new());
        }

        // generate the diff w/ the last spk history
        let diff = self
            .spk_history
            .get_mut(&spk)
            .expect("already inserted")
            .insert(history);

        {
            // drop tx in the tx_store & update heights
            let store = &mut self.tx_store;
            for txid in diff.removed.keys() {
                store.remove(txid);
            }
            for (txid, height) in &diff.changed {
                store.update_height(txid, *height);
            }
        } // <- release &mut tx_store

        (!diff.changed.is_empty(), Update::from_diff(spk, diff))
    }

    /// Handles the response containing transactions.
    ///
    /// This method processes the received transactions and updates the
    /// internal state of the `CoinStore`. It regenerates the coin store
    /// from the transaction store.
    ///
    /// # Parameters
    /// - `txs`: A vector of Bitcoin transactions received.
    pub fn handle_txs_response(&mut self, txs: Vec<bitcoin::Transaction>) {
        // iter over updates & populate where the transaction is required
        for new_tx in txs {
            let new_txid = new_tx.compute_txid();
            for Update { txs, .. } in &mut self.updates {
                txs.iter_mut().for_each(|(txid, tx, _)| {
                    if (*txid == new_txid) && tx.is_none() {
                        *tx = Some(new_tx.clone());
                    }
                });
            }
        }
        {
            // push every complete update to the tx store
            let store = &mut self.tx_store;
            self.updates = self
                .updates
                .clone()
                .into_iter()
                .filter_map(|update| {
                    if update.is_complete() {
                        store.insert_updates(vec![update]);
                        None
                    } else {
                        Some(update)
                    }
                })
                .collect();
        } // <- release &mut tx_store

        // re-generate coin store from tx store
        self.generate();
    }

    /// Generates the coin store from the transaction store.
    ///
    /// This method populates the coin store with coins based on the
    /// transactions in the transaction store and updates the address
    /// statuses accordingly.
    pub fn generate(&mut self) {
        let addr_store = &mut self.address_store;
        let tx_store = &self.tx_store;

        let mut coins = BTreeMap::<OutPoint, CoinEntry>::new();
        let descriptor = self.config.descriptor.clone();

        // NOTE: here we take the max satisfaction size at default, for descriptor with several
        // spending conditions, the correct satisfaction must be filled at tx crafting time.
        let satisfaction = max_input_satisfaction_size(&descriptor);

        // list all received coins
        for (_, entry) in tx_store.iter() {
            let tx = entry.tx();
            let txid = tx.compute_txid();
            for (vout, txout) in tx.output.iter().enumerate() {
                if let Some(addr) = addr_store
                    .lock()
                    .expect("poisoned")
                    .get_entry(&txout.script_pubkey)
                {
                    let txout = txout.clone();
                    let outpoint = OutPoint {
                        txid,
                        vout: vout as u32,
                    };
                    let height = entry.height();
                    let status = if height.is_some() {
                        CoinStatus::Confirmed
                    } else {
                        CoinStatus::Unconfirmed
                    };
                    let spk = match addr.account() {
                        coin::KeyChain::Receive => self.derivator.receive_at(addr.index()),
                        coin::KeyChain::Change => self.derivator.change_at(addr.index()),
                        coin::KeyChain::Custom(_) => unimplemented!(),
                    }
                    .script_pubkey();
                    assert!(spk == txout.script_pubkey);

                    let label = self
                        .label_store
                        .lock()
                        .expect("poisoned")
                        .outpoint(outpoint);
                    let coin = Coin {
                        txout,
                        outpoint,
                        height,
                        // Sequence is overwritten at spend time anyway
                        sequence: Sequence::ZERO,
                        status,
                        label,
                        satisfaction_size: satisfaction as u64,
                        spend_info: bwk_tx::CoinSpendInfo::Bip32 {
                            coin_path: (addr.account(), addr.index()),
                            descriptor: descriptor.clone(),
                            secret_key: None,
                        },
                    };
                    let coin = CoinEntry {
                        coin,
                        address: addr.address(),
                    };
                    coins.insert(outpoint, coin);
                }
            }
        }
        // list all spent coins + collect spending txids per spk:
        // every input whose previous_output points at one of our
        // coins is, by definition, a tx that spent that spk.
        let mut spk_to_spending = BTreeMap::<ScriptBuf, BTreeSet<Txid>>::new();
        for (tx_txid, tx_entry) in tx_store.iter() {
            for inp in &tx_entry.tx().input {
                if let Some(spent_ce) = coins.get_mut(&inp.previous_output) {
                    spent_ce.coin.status = CoinStatus::Spent;
                    spk_to_spending
                        .entry(spent_ce.spk())
                        .or_default()
                        .insert(tx_txid);
                }
            }
        }
        let mut spk_to_outpoint = BTreeMap::<ScriptBuf, HashSet<OutPoint>>::new();
        let mut spk_to_funding = BTreeMap::<ScriptBuf, BTreeSet<Txid>>::new();
        coins.iter().for_each(|(op, ce)| {
            let spk = ce.spk();
            spk_to_outpoint
                .entry(spk.clone())
                .and_modify(|e| {
                    e.insert(*op);
                })
                .or_insert({
                    let mut h = HashSet::new();
                    h.insert(*op);
                    h
                });
            spk_to_funding.entry(spk).or_default().insert(op.txid);
        });

        // populate labels
        {
            let store = self.label_store.lock().expect("poisoned");
            for (op, coin) in &mut coins {
                coin.coin.label = store.get(&LabelKey::OutPoint(*op)).clone();
            }
        } // => release label_store lock

        self.store = coins;
        self.spk_to_outpoint = spk_to_outpoint;

        // update address_store statuses + per-address tx history
        self.spk_to_outpoint.iter().for_each(|(spk, op)| {
            let status = match op.len() {
                0 => AddressStatus::NotUsed,
                1 => AddressStatus::Used,
                _ => AddressStatus::Reused,
            };
            if let Some(e) = addr_store.lock().expect("poisoned").get_entry_mut(spk) {
                e.set_status(status);
                e.set_funding_txids(spk_to_funding.get(spk).cloned().unwrap_or_default());
                e.set_spending_txids(spk_to_spending.get(spk).cloned().unwrap_or_default());
            }
        });

        self.populate_tx_metadata();
        self.tx_store.persist();

        // FIXME: update statuses of those w/ CoinStatus::BeeingSpent

        if let Err(e) = self.notification.send(Notification::CoinUpdate) {
            log::error!("CoinStore::generate() fail to send notification: {e:?}");
        }
    }

    pub fn populate_tx_metadata(&mut self) {
        let unpopulated: Vec<_> = self
            .tx_store
            .transactions()
            .into_iter()
            .filter_map(|tx| (!tx.is_complete()).then_some(tx.txid()))
            .collect();

        if unpopulated.is_empty() {
            return;
        }

        // We get all outpoints info first
        let mut outpoints = BTreeSet::new();
        for txid in &unpopulated {
            if let Some(tx) = self.tx_store.get(txid) {
                for inp in &tx.tx().input {
                    outpoints.insert(inp.previous_output);
                }
            }
        }
        let mut txouts = BTreeMap::new();
        for op in outpoints {
            if let Some(tx) = self.tx_store.get(&op.txid) {
                let index = op.vout as usize;
                txouts.insert(op, tx.tx().output[index].clone());
            }
        }

        // Populate all transactions metadata
        for txid in unpopulated {
            let mut tx = self.tx_store.get(&txid).expect("present").clone();
            if tx.is_complete() {
                continue;
            }
            // Populate all outputs (looking for received coins)
            for (i, txout) in tx.tx().output.clone().iter().enumerate() {
                let spk = txout.script_pubkey.clone();
                let owned = self
                    .address_store
                    .lock()
                    .expect("poisoned")
                    .contains_spk(&spk);
                let output = OutputMetadata { owned };
                tx.outputs.insert(i, output);
            }
            // Populate all inputs
            for (i, txin) in tx.tx().input.clone().iter().enumerate() {
                let input = if let Some(txout) = txouts.get(&txin.previous_output) {
                    let spk = txout.script_pubkey.clone();
                    let owned = self
                        .address_store
                        .lock()
                        .expect("poisoned")
                        .contains_spk(&spk);
                    InputMetadata {
                        value: owned.then_some(txout.value.to_sat()),
                        owned,
                    }
                } else {
                    InputMetadata {
                        value: None,
                        owned: false,
                    }
                };
                tx.inputs.insert(i, input);
            }
            // Update the store (replace the entry entirely)
            self.tx_store.update(tx);
        }
    }

    /// Retrieves coins by their status.
    ///
    /// This method filters the coins in the store based on the specified
    /// status and returns them as a `Coins` object.
    ///
    /// # Parameters
    /// - `status`: The status of the coins to retrieve.
    pub fn get_by_status(&self, status: CoinStatus) -> Vec<CoinEntry> {
        self.store
            .clone()
            .into_iter()
            .filter_map(|(_, coin)| {
                if coin.coin.status == status {
                    Some(coin)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
    }

    /// Retrieves a coin entry from the store by its outpoint.
    ///
    /// # Parameters
    /// - `outpoint`: A reference to the `OutPoint` of the coin to retrieve.
    ///
    /// # Returns
    /// An `Option<CoinEntry>` containing the coin entry if found, or `None` if no entry exists for the given outpoint.
    pub fn get(&self, outpoint: &bitcoin::OutPoint) -> Option<CoinEntry> {
        self.store.get(outpoint).cloned()
    }

    /// Retrieves spendable coins from the store.
    ///
    /// This method filters the coins that are either unconfirmed or
    /// confirmed and returns them as a `Coins` object.
    pub fn spendable_coins(&self) -> CoinState {
        let mut coins: Vec<_> = self
            .store
            .clone()
            .into_iter()
            .filter_map(|(_, coin)| match coin.coin.status {
                CoinStatus::Unconfirmed | CoinStatus::Confirmed | CoinStatus::BeingSpend => {
                    Some(coin)
                }
                CoinStatus::Spent => None,
            })
            .collect();
        coins.sort_by(|a, b| a.coin.outpoint.cmp(&b.coin.outpoint));
        let mut state = CoinState {
            coins: Default::default(),
            confirmed_coins: 0,
            confirmed_balance: 0,
            unconfirmed_coins: 0,
            unconfirmed_balance: 0,
        };
        for entry in &coins {
            match entry.coin.status {
                CoinStatus::Unconfirmed => {
                    state.unconfirmed_coins += 1;
                    state.unconfirmed_balance += entry.coin.txout.value.to_sat();
                }
                CoinStatus::Confirmed => {
                    state.confirmed_coins += 1;
                    state.confirmed_balance += entry.coin.txout.value.to_sat();
                }
                _ => {}
            }
        }
        state.coins = coins.into_iter().map(|c| (*c.outpoint(), c.coin)).collect();
        state
    }

    /// Returns a list of all historical transactions
    pub fn tx_history(&self) -> Vec<TxEntry> {
        self.tx_store.transactions()
    }

    /// Returns all coins in the store.
    ///
    /// # Returns
    /// A `BTreeMap` of outpoints to their corresponding `CoinEntry`.
    pub fn coins(&self) -> BTreeMap<bitcoin::OutPoint, CoinEntry> {
        self.store.clone()
    }

    /// Dumps the coin store as a JSON value.
    ///
    /// # Returns
    /// A `Result` containing the serialized JSON value of the coin store
    /// or an error if serialization fails.
    pub fn dump(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(&self.store)
    }

    /// Restores the coin store from a JSON value.
    ///
    /// # Parameters
    /// - `value`: The JSON value to restore the coin store from.
    ///
    /// # Returns
    /// A `Result` indicating success or failure of the restoration.
    pub fn restore(&mut self, value: serde_json::Value) -> Result<(), serde_json::Error> {
        self.store = serde_json::from_value(value)?;
        Ok(())
    }

    pub fn address_store(&self) -> Arc<Mutex<AddressStore<P>>> {
        self.address_store.clone()
    }
}

#[derive(Debug, Clone)]
/// Represents an update to the transaction history for a script public key (SPK).
///
/// The `Update` struct contains the script public key and a list of
/// transactions that have been added or changed.
pub struct Update {
    #[allow(unused)]
    spk: ScriptBuf,
    pub txs: Vec<(bitcoin::Txid, Option<bitcoin::Transaction>, Option<u64>)>,
}

impl Update {
    /// Creates an `Update` from the differences in SPK history.
    ///
    /// # Parameters
    /// - `spk`: The script public key associated with the update.
    /// - `diff`: The differences in the SPK history.
    ///
    /// # Returns
    /// A new `Update` instance.
    pub fn from_diff(spk: ScriptBuf, diff: HistoryDiff) -> Self {
        Update {
            spk,
            txs: diff
                .added
                .into_iter()
                .map(|(txid, height)| (txid, None, height))
                .collect(),
        }
    }

    /// Checks if the update is complete.
    ///
    /// An update is considered complete if all transactions have been
    /// received.
    ///
    /// # Returns
    /// `true` if the update is complete, otherwise `false`.
    pub fn is_complete(&self) -> bool {
        self.txs.iter().all(|(_, tx, _)| tx.is_some())
    }

    /// Returns a list of missing transaction IDs in the update.
    ///
    /// # Returns
    /// A vector of `Txid` representing transactions that are missing.
    pub fn missing(&self) -> Vec<Txid> {
        self.txs
            .iter()
            .filter_map(|(txid, tx, _)| if tx.is_none() { Some(*txid) } else { None })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Represents a coin entry in the coin store.
///
/// The `CoinEntry` struct contains information about the coin's height,
/// status, associated coin data, and the address it belongs to.
pub struct CoinEntry {
    pub coin: coin::Coin,
    pub address: bitcoin::Address<NetworkUnchecked>,
}

impl From<CoinEntry> for Coin {
    fn from(value: CoinEntry) -> Self {
        value.coin
    }
}

impl CoinEntry {
    /// Returns the height of the coin in the blockchain.
    ///
    /// # Returns
    /// An `Option<u64>` representing the height of the coin,
    /// or `None` if the coin is not confirmed.
    pub fn height(&self) -> Option<u64> {
        self.coin.height
    }
    /// Returns the status of the coin.
    ///
    /// # Returns
    /// The `CoinStatus` of the coin.
    pub fn status(&self) -> CoinStatus {
        self.coin.status
    }
    /// Returns a string representation of the coin's status.
    ///
    /// # Returns
    /// A string describing the coin's status.
    pub fn status_str(&self) -> String {
        format!("{:?}", self.coin.status)
    }
    /// Returns the label associated with the coin.
    ///
    /// # Returns
    /// A string representation of the coin's label, or an empty string if no label is set.
    pub fn label(&self) -> String {
        self.coin.label.clone().unwrap_or_default()
    }
    /// Returns the amount of the coin in satoshis.
    ///
    /// # Returns
    /// The value of the coin in satoshis.
    pub fn amount_sat(&self) -> u64 {
        self.coin.txout.value.to_sat()
    }
    /// Returns the amount of the coin in Bitcoin.
    ///
    /// # Returns
    /// The value of the coin in Bitcoin.
    pub fn amount_btc(&self) -> f64 {
        self.coin.txout.value.to_btc()
    }
    /// Returns a reference to the coin's outpoint.
    ///
    /// # Returns
    /// A reference to the `OutPoint` of the coin.
    pub fn outpoint(&self) -> &OutPoint {
        &self.coin.outpoint
    }
    /// Returns a string representation of the coin's outpoint.
    ///
    /// # Returns
    /// A string describing the coin's outpoint.
    pub fn outpoint_str(&self) -> String {
        self.outpoint().to_string()
    }
    /// Generate the TxIn from the coin.
    ///
    /// # Returns
    /// A `bitcoin::TxIn` representing the input transaction associated with the coin.
    pub fn txin(&self) -> bitcoin::TxIn {
        bitcoin::TxIn {
            previous_output: self.coin.outpoint,
            script_sig: ScriptBuf::new(),
            sequence: bitcoin::Sequence::ZERO,
            witness: bitcoin::Witness::new(),
        }
    }
    /// Returns the TxOut of this coin.
    ///
    /// # Returns
    /// A `bitcoin::TxOut` representing the output transaction associated with the coin.
    pub fn txout(&self) -> bitcoin::TxOut {
        self.coin.txout.clone()
    }
    /// Returns the derivation path associated with the coin.
    ///
    /// # Returns
    /// A tuple containing the `AddrAccount` and the index of the coin's derivation path.
    /// Returns `None` for non-descriptor coins (e.g., Silent Payment coins).
    pub fn deriv(&self) -> Option<(KeyChain, u32)> {
        match &self.coin.spend_info {
            bwk_tx::CoinSpendInfo::Bip32 { coin_path, .. } => Some(*coin_path),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }
    /// Returns a boxed version of the coin entry.
    ///
    /// # Returns
    /// A `Box` containing the coin entry.
    pub fn boxed(&self) -> Box<CoinEntry> {
        Box::new(self.clone())
    }
    /// Returns the address associated with the coin.
    ///
    /// # Returns
    /// A string representation of the coin's address.
    pub fn address(&self) -> String {
        self.address.clone().assume_checked().to_string()
    }
    /// Returns the address associated with the coin as an 'RustAddress'
    ///
    /// # Returns
    /// A boxed AddressEntry representation of the coin's address.
    /// Panics for non-descriptor coins.
    pub fn rust_address(&self) -> AddressEntry {
        let (account, index) = match &self.coin.spend_info {
            bwk_tx::CoinSpendInfo::Bip32 { coin_path, .. } => *coin_path,
            #[allow(unreachable_patterns)]
            _ => panic!("rust_address not supported for non-descriptor coins"),
        };
        AddressEntry {
            status: AddressStatus::Unknown,
            address: self.address.clone(),
            account,
            index,
            funding_txids: std::collections::BTreeSet::new(),
            spending_txids: std::collections::BTreeSet::new(),
        }
    }
    /// Returns the script public key (SPK) associated with the coin.
    ///
    /// # Returns
    /// The `ScriptBuf` representing the coin's SPK.
    pub fn spk(&self) -> ScriptBuf {
        self.address.clone().assume_checked().script_pubkey()
    }
}

#[cfg(all(test, feature = "test"))]
mod tests {
    //! Coverage for the per-address tx-history population in
    //! [`CoinStore::generate`]. Constructs a CoinStore directly
    //! with a NoopBackend (no electrsd, no listener thread) and
    //! drives funding/spending txs through the in-memory
    //! [`TxStore`].
    use super::*;
    use crate::config::Config;
    use bip39::Mnemonic;
    use bwk_descriptor::{descriptor::ScriptType, wpkh_path};
    use bwk_sign::HotSigner;
    use bwk_utils::test::{funding_tx, spending_tx};
    use miniscript::bitcoin::bip32::ChildNumber;
    use std::sync::mpsc;

    fn build_coin_store() -> (CoinStore, SpkDerivator) {
        let network = bitcoin::Network::Regtest;
        let mnemo = Mnemonic::generate(12).unwrap();
        let signer = HotSigner::new_from_mnemonics(network, &mnemo.to_string()).unwrap();
        let path = wpkh_path(network, ChildNumber::from_hardened_idx(0).unwrap()).unwrap();
        let xpub = signer.xpub(&path);
        let derivator = SpkDerivator::new_wpkh(xpub, network).unwrap();
        let descriptor = derivator.descriptor();
        let dummy_config = Config::new(
            Some(mnemo.to_string()),
            "addr_history_test".into(),
            network,
            ScriptType::Segwit(ChildNumber::from_hardened_idx(0).unwrap()),
            std::path::PathBuf::default(),
            String::new(),
            false,
        )
        .unwrap();

        let (notif_sender, _notif_recv) = mpsc::channel();
        let tx_store = TxStore::new();
        let label_store = Arc::new(Mutex::new(LabelStore::new()));
        let mock_backend: Arc<dyn bwk_persist::PersistenceBackend> =
            Arc::new(bwk_persist::NoopBackend);
        let account_store = Arc::new(Mutex::new(bwk_persist::RamStore::empty(
            mock_backend,
            bwk_persist::ACCOUNT_STORE_KEY,
            crate::profile::encode_account_key,
            crate::profile::encode_account_value,
        )));
        let cs = CoinStore::new(
            network,
            descriptor,
            notif_sender,
            0, // recv_tip
            0, // change_tip
            5, // look_ahead — populates spk indices 0..=5
            tx_store,
            label_store,
            dummy_config,
            account_store,
        );
        (cs, derivator)
    }

    #[test]
    fn funding_txids_empty_for_unused_address() {
        let (mut cs, deriv) = build_coin_store();
        cs.generate();
        let spk = deriv.receive_spk_at(2);
        let entry = cs.address_info(&spk).expect("populated by look-ahead");
        assert_eq!(entry.status(), AddressStatus::NotUsed);
        assert!(entry.funding_txids().is_empty());
        assert!(entry.spending_txids().is_empty());
    }

    #[test]
    fn funding_txids_single_for_used_address() {
        let (mut cs, deriv) = build_coin_store();
        let spk = deriv.receive_spk_at(2);
        let tx = funding_tx(spk.clone(), 0.5);
        let txid = tx.compute_txid();
        cs.tx_store
            .update(crate::tx_store::TxEntry::for_test(tx, Some(101)));
        cs.generate();
        let entry = cs.address_info(&spk).expect("entry");
        assert_eq!(entry.status(), AddressStatus::Used);
        assert_eq!(entry.funding_txids().len(), 1);
        assert!(entry.funding_txids().contains(&txid));
        assert!(entry.spending_txids().is_empty());
    }

    #[test]
    fn funding_txids_multiple_for_reused_address() {
        let (mut cs, deriv) = build_coin_store();
        let spk = deriv.receive_spk_at(2);
        let tx_a = funding_tx(spk.clone(), 0.5);
        let tx_b = funding_tx(spk.clone(), 0.25);
        let (txid_a, txid_b) = (tx_a.compute_txid(), tx_b.compute_txid());
        cs.tx_store
            .update(crate::tx_store::TxEntry::for_test(tx_a, Some(101)));
        cs.tx_store
            .update(crate::tx_store::TxEntry::for_test(tx_b, Some(102)));
        cs.generate();
        let entry = cs.address_info(&spk).expect("entry");
        assert_eq!(entry.status(), AddressStatus::Reused);
        assert_eq!(entry.funding_txids().len(), 2);
        assert!(entry.funding_txids().contains(&txid_a));
        assert!(entry.funding_txids().contains(&txid_b));
    }

    #[test]
    fn spending_txids_empty_when_unspent() {
        let (mut cs, deriv) = build_coin_store();
        let spk = deriv.receive_spk_at(3);
        let tx = funding_tx(spk.clone(), 0.5);
        cs.tx_store
            .update(crate::tx_store::TxEntry::for_test(tx, Some(101)));
        cs.generate();
        let entry = cs.address_info(&spk).expect("entry");
        assert!(entry.spending_txids().is_empty());
    }

    #[test]
    fn spending_txids_recorded_when_outpoint_consumed() {
        let (mut cs, deriv) = build_coin_store();
        let spk = deriv.receive_spk_at(4);

        // Fund index 4.
        let funding = funding_tx(spk.clone(), 0.5);
        let funding_txid = funding.compute_txid();
        // The spk we paid is at the LAST output (funding_tx appends it).
        let funded_vout = (funding.output.len() - 1) as u32;
        cs.tx_store
            .update(crate::tx_store::TxEntry::for_test(funding, Some(101)));

        // Spend the freshly-funded outpoint.
        let outpoint = OutPoint {
            txid: funding_txid,
            vout: funded_vout,
        };
        let spending = spending_tx(outpoint);
        let spending_txid = spending.compute_txid();
        cs.tx_store
            .update(crate::tx_store::TxEntry::for_test(spending, Some(102)));

        cs.generate();
        let entry = cs.address_info(&spk).expect("entry");
        assert_eq!(entry.spending_txids().len(), 1);
        assert!(entry.spending_txids().contains(&spending_txid));
        // Funding still recorded too.
        assert!(entry.funding_txids().contains(&funding_txid));
    }
}
