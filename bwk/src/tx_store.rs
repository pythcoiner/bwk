use miniscript::bitcoin::{self, Txid};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt::Debug, str::FromStr, sync::Arc};

use bwk_persist::{NoopBackend, PersistError, PersistenceBackend, RamStore, Store};

use crate::{
    coin_store::Update,
    profile::{DefaultBackend, RamProfile, StorageProfile},
};

/// Logical store name used by [`PersistenceBackend`] implementations for
/// the bwk transaction store.
pub const STORE_KEY: &str = bwk_persist::TRANSACTIONS_STORE_KEY;

pub fn encode_txid(k: &Txid) -> String {
    k.to_string()
}
pub fn decode_txid(s: &str) -> Result<Txid, PersistError> {
    Txid::from_str(s).map_err(|e| PersistError::Serde(format!("bad txid pk {s:?}: {e}")))
}
pub fn encode_entry(v: &TxEntry) -> Result<Vec<u8>, PersistError> {
    serde_json::to_vec(v).map_err(|e| PersistError::Serde(format!("encode TxEntry: {e}")))
}
pub fn decode_entry(bytes: &[u8]) -> Result<TxEntry, PersistError> {
    serde_json::from_slice(bytes).map_err(|e| PersistError::Serde(format!("decode TxEntry: {e}")))
}

/// A structure to store Bitcoin transactions indexed by their txids.
///
/// Generic over any [`StorageProfile`]: the wrapper picks up the
/// profile's `TxStore` slot, so today's RAM-backed default is
/// [`RamStore`] via [`RamProfile`] but any other profile plugs in
/// without touching this wrapper or its callers.
pub struct TxStore<P: StorageProfile = RamProfile<DefaultBackend>> {
    store: P::TxStore,
}

impl<P: StorageProfile> Debug for TxStore<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TxStore")
            .field("len", &self.store.len().ok())
            .finish()
    }
}

impl TxStore<RamProfile<DefaultBackend>> {
    /// Creates a `TxStore` with no persistence (in-memory only).
    pub fn new() -> Self {
        let backend: Arc<dyn PersistenceBackend> = Arc::new(NoopBackend);
        Self {
            store: RamStore::empty(backend, STORE_KEY, encode_txid, encode_entry),
        }
    }

    /// Creates a `TxStore` that persists through the given backend and
    /// eagerly loads any existing rows from it.
    pub fn with_backend(
        backend: Arc<dyn PersistenceBackend>,
        store_key: &'static str,
    ) -> Result<Self, PersistError> {
        Ok(Self {
            store: RamStore::open(
                backend,
                store_key,
                encode_txid,
                decode_txid,
                encode_entry,
                decode_entry,
            )?,
        })
    }
}

impl Default for TxStore<RamProfile<DefaultBackend>> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: StorageProfile> TxStore<P> {
    /// Wrap any `P::TxStore` impl produced by the profile.
    pub fn from_store(store: P::TxStore) -> Self {
        Self { store }
    }

    /// Returns every transaction entry as an owned `Vec`, key-ordered.
    pub fn transactions(&self) -> Vec<TxEntry> {
        self.store
            .values()
            .ok()
            .map(|it| it.collect())
            .unwrap_or_default()
    }

    /// Fetch a cloned entry by txid. `None` if absent.
    pub fn get(&self, txid: &Txid) -> Option<TxEntry> {
        self.store.get(txid).ok().flatten()
    }

    /// Number of transactions in the store.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.store.len().unwrap_or(0)
    }

    /// Iterate `(Txid, TxEntry)` pairs, key-ordered.
    pub fn iter(&self) -> Vec<(Txid, TxEntry)> {
        self.store
            .iter()
            .ok()
            .map(|it| it.collect())
            .unwrap_or_default()
    }

    /// Inserts updates (only txids not already in the store).
    pub fn insert_updates(&mut self, updates: Vec<Update>) {
        updates.iter().for_each(|u| {
            assert!(u.is_complete());
        });

        for upd in updates {
            for (txid, tx, height) in upd.txs {
                match self.store.contains_key(&txid) {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(e) => {
                        log::error!("TxStore::insert_updates contains_key: {e}");
                        continue;
                    }
                }
                let tx = tx.expect("all txs populated");
                let weight = tx.weight().to_wu();
                let entry = TxEntry {
                    height,
                    tx,
                    merkle: Default::default(),
                    inputs: BTreeMap::new(),
                    outputs: BTreeMap::new(),
                    fees: 0,
                    weight,
                };
                if let Err(e) = self.store.insert(txid, entry) {
                    log::error!("TxStore::insert_updates insert: {e}");
                }
            }
        }
    }

    /// Updates (or inserts) a transaction entry in the store.
    pub fn update(&mut self, entry: TxEntry) {
        let txid = entry.txid();
        if let Err(e) = self.store.insert(txid, entry) {
            log::error!("TxStore::update insert: {e}");
        }
    }

    /// Retrieves the underlying Bitcoin transaction for a given txid.
    pub fn inner_get(&self, txid: &Txid) -> Option<bitcoin::Transaction> {
        self.store.get(txid).ok().flatten().map(|e| e.tx)
    }

    /// Removes a transaction from the store by its transaction ID.
    pub fn remove(&mut self, txid: &Txid) {
        if let Err(e) = self.store.remove(txid) {
            log::error!("TxStore::remove: {e}");
        }
    }

    /// Updates the height of a transaction in the store.
    pub fn update_height(&mut self, txid: &Txid, height: Option<u64>) {
        match self.store.modify(txid, |e| e.height = height) {
            Ok(true) => {}
            Ok(false) => panic!("update_height on a missing txid"),
            Err(e) => log::error!("TxStore::update_height: {e}"),
        }
    }

    /// Persists pending changes through the configured backend.
    pub fn persist(&mut self) {
        if let Err(e) = self.store.flush() {
            log::error!("TxStore::persist() flush: {e}");
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputMetadata {
    pub value: Option<u64>,
    pub owned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputMetadata {
    pub owned: bool,
}

/// A structure representing a Bitcoin transaction entry.
#[derive(Clone, Serialize, Deserialize)]
pub struct TxEntry {
    /// Blockheight at which the tx have been mined
    height: Option<u64>,
    /// Bitcoin tx
    tx: bitcoin::Transaction,
    /// Merkle proof
    merkle: Vec<Vec<u8>>,
    /// Inputs netadata
    pub inputs: BTreeMap<usize, InputMetadata>,
    /// Outputs metatdata
    pub outputs: BTreeMap<usize, OutputMetadata>,
    /// Tx fees in sats
    fees: u64,
    /// Tx weight in wu
    weight: u64,
}

impl Debug for TxEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TxEntry")
            .field("height", &self.height)
            .field("tx", &self.tx.compute_txid())
            .field("merkle", &self.merkle)
            .field("inputs", &self.inputs)
            .field("outputs", &self.outputs)
            .field("fees", &self.fees)
            .field("weight", &self.weight)
            .finish()
    }
}

impl TxEntry {
    /// Test-only constructor that wraps a raw tx + height in a
    /// fully-defaulted [`TxEntry`]. Intended for in-crate tests
    /// that need to seed the [`TxStore`] without going through the
    /// Electrum update plumbing.
    #[cfg(all(test, feature = "test"))]
    pub(crate) fn for_test(tx: bitcoin::Transaction, height: Option<u64>) -> Self {
        let weight = tx.weight().to_wu();
        Self {
            height,
            tx,
            merkle: Default::default(),
            inputs: BTreeMap::new(),
            outputs: BTreeMap::new(),
            fees: 0,
            weight,
        }
    }

    pub fn txid(&self) -> Txid {
        self.tx.compute_txid()
    }
    pub fn height(&self) -> Option<u64> {
        self.height
    }
    pub fn set_height(&mut self, height: Option<u64>) {
        self.height = height;
    }
    pub fn tx(&self) -> &bitcoin::Transaction {
        &self.tx
    }
    pub fn merkle(&self) -> Vec<Vec<u8>> {
        self.merkle.clone()
    }
    pub fn set_merkle(&mut self, merkle: Vec<Vec<u8>>) {
        self.merkle = merkle;
    }
    pub fn fees(&self) -> u64 {
        self.fees
    }
    pub fn set_fees(&mut self, fees: u64) {
        self.fees = fees;
    }
    pub fn weight(&self) -> u64 {
        self.weight
    }

    pub fn is_complete(&self) -> bool {
        let inputs_filled = self.tx().input.len() == self.inputs.len();
        let outputs_filled = self.tx().output.len() == self.outputs.len();
        inputs_filled && outputs_filled
    }
}
