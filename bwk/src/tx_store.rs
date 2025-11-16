use miniscript::bitcoin::{self, Txid};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt::Debug,
    fs::File,
    io::{Read, Write},
    path::PathBuf,
};

use crate::{coin_store::Update, config::maybe_create_dir};

#[derive(Debug)]
/// A structure to store Bitcoin transactions indexed by their transaction IDs.
pub struct TxStore {
    store: BTreeMap<Txid, TxEntry>,
    path: Option<PathBuf>,
    persist: bool,
}

impl TxStore {
    /// Creates a new `TxStore` instance.
    ///
    /// # Parameters
    /// - `store`: A BTreeMap containing the transactions indexed by their Txid.
    /// - `path`: An optional path to a file where the store can be persisted.
    pub fn new(store: BTreeMap<Txid, TxEntry>, path: Option<PathBuf>) -> Self {
        Self {
            store,
            path,
            persist: true,
        }
    }

    pub fn transactions(&self) -> Vec<TxEntry> {
        self.store.values().cloned().collect()
    }

    pub fn get(&self, txid: &Txid) -> Option<&TxEntry> {
        self.store.get(txid)
    }

    pub fn get_mut(&mut self, txid: &Txid) -> Option<&mut TxEntry> {
        self.store.get_mut(txid)
    }

    #[allow(clippy::len_without_is_empty)]
    /// Returns the number of transactions in the store.
    pub fn len(&self) -> usize {
        self.store.len()
    }

    /// Returns a reference to the inner BTreeMap of transactions.
    pub fn inner(&self) -> &BTreeMap<Txid, TxEntry> {
        &self.store
    }

    /// Inserts a vector of updates into the transaction store.
    ///
    /// # Parameters
    /// - `updates`: A vector of `Update` instances containing transactions to insert.
    pub fn insert_updates(&mut self, updates: Vec<Update>) {
        // sanitize, all Txs must Some(_)
        updates.iter().for_each(|u| {
            assert!(u.is_complete());
        });

        for upd in updates {
            for (txid, tx, height) in upd.txs {
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
                self.store.entry(txid).or_insert(entry);
            }
        }
    }

    /// Updates an existing transaction entry in the store.
    ///
    /// # Parameters
    /// - `entry`: The `TxEntry` to update in the store.
    pub fn update(&mut self, entry: TxEntry) {
        let txid = entry.txid();
        self.store.insert(txid, entry);
    }

    /// Retrieves a transaction by its transaction ID.
    ///
    /// # Parameters
    /// - `txid`: The transaction ID of the transaction to retrieve.
    ///
    /// # Returns
    /// An `Option` containing the transaction if found, or `None` if not.
    pub fn inner_get(&self, txid: &Txid) -> Option<bitcoin::Transaction> {
        self.store.get(txid).map(|e| e.tx.clone())
    }

    /// Removes a transaction from the store by its transaction ID.
    ///
    /// # Parameters
    /// - `txid`: The transaction ID of the transaction to remove.
    pub fn remove(&mut self, txid: &bitcoin::Txid) {
        self.store.remove(txid);
    }

    /// Updates the height of a transaction in the store.
    ///
    /// # Parameters
    /// - `txid`: The transaction ID of the transaction to update.
    /// - `height`: The new height to set, or `None` to clear the height.
    pub fn update_height(&mut self, txid: &bitcoin::Txid, height: Option<u64>) {
        self.store.get_mut(txid).expect("is present").height = height;
    }

    /// Loads the transaction store from a file.
    ///
    /// # Parameters
    /// - `path`: The path to the file to load the transactions from.
    pub fn store_from_file(path: PathBuf) -> BTreeMap<Txid, TxEntry> {
        let file = File::open(path);
        if let Ok(mut file) = file {
            let mut content = String::new();
            let _ = file.read_to_string(&mut content);
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Default::default()
        }
    }

    /// Allow to disable persistance of data, useful for tests
    pub fn enable_persist(mut self, persist: bool) -> Self {
        self.persist = persist;
        self
    }

    /// Persists the transaction store to a file.
    pub fn persist(&self) {
        if !self.persist {
            return;
        }
        if let Some(path) = &self.path {
            let parent = path.parent().expect("has a parent").to_path_buf();
            maybe_create_dir(&parent);
            let mut file = File::create(path.clone()).unwrap();
            let content = serde_json::to_string_pretty(&self.store).unwrap();
            let _ = file.write(content.as_bytes());
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
    /// Returns the transaction ID of the transaction entry.
    pub fn txid(&self) -> Txid {
        self.tx.compute_txid()
    }
    /// Returns the height of the transaction in the blockchain.
    pub fn height(&self) -> Option<u64> {
        self.height
    }
    /// Returns a reference to the underlying Bitcoin transaction.
    pub fn tx(&self) -> &bitcoin::Transaction {
        &self.tx
    }
    /// Returns the Merkle proof associated with the transaction entry.
    ///
    /// # Returns
    /// A vector of byte vectors representing the Merkle proof.
    pub fn merkle(&self) -> Vec<Vec<u8>> {
        self.merkle.clone()
    }

    pub fn is_complete(&self) -> bool {
        let inputs_filled = self.tx().input.len() == self.inputs.len();
        let outputs_filled = self.tx().output.len() == self.outputs.len();
        inputs_filled && outputs_filled
    }
}
