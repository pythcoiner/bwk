use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
};

use miniscript::bitcoin::{self, address::NetworkUnchecked, OutPoint};
use serde::{Deserialize, Serialize};

use crate::config::Config;

#[derive(Debug, Clone, Serialize, Deserialize, PartialOrd, Ord, Eq, PartialEq)]
pub enum LabelKey {
    OutPoint(bitcoin::OutPoint),
    Transaction(bitcoin::Txid),
    Address(bitcoin::Address<NetworkUnchecked>),
}

#[derive(Debug, Clone, Default)]
/// A store for managing labels associated with Bitcoin addresses, transactions, and outpoints.
pub struct LabelStore {
    store: BTreeMap<LabelKey, String>,
    config: Option<Config>,
    persist: bool,
}

impl LabelStore {
    /// Creates a new, empty `LabelStore`.
    pub fn new() -> Self {
        LabelStore {
            store: BTreeMap::new(),
            config: None,
            persist: true,
        }
    }

    /// Creates a `LabelStore` from a file specified in the given configuration.
    ///
    /// # Parameters
    /// - `config`: The configuration containing the path to the labels file.
    pub fn from_file(config: Config) -> Self {
        let file = File::open(config.labels_path());
        match file {
            Ok(mut file) => {
                let mut content = String::new();
                let _ = file.read_to_string(&mut content);
                let store: BTreeMap<LabelKey, String> =
                    serde_json::from_str(&content).unwrap_or_default();
                LabelStore {
                    store,
                    config: Some(config),
                    persist: true,
                }
            }
            Err(_) => LabelStore {
                store: Default::default(),
                config: Some(config),
                persist: true,
            },
        }
    }

    /// Allow to disable persistance of data, useful for tests
    pub fn enable_persist(mut self, persist: bool) -> Self {
        self.persist = persist;
        self
    }

    /// Persists the current labels to the file specified in the configuration.
    pub fn persist(&self) {
        if !self.persist {
            return;
        }
        if let Some(config) = self.config.as_ref() {
            let file = File::create(config.labels_path());
            match file {
                Ok(mut file) => {
                    let content = serde_json::to_string_pretty(&self.store).expect("cannot fail");
                    let _ = file.write(content.as_bytes());
                }
                Err(e) => {
                    log::error!("LabelStore::persist() fail to open file: {e}");
                }
            }
        }
    }

    /// Retrieves the label associated with the given key.
    ///
    /// # Parameters
    /// - `key`: The key for which to retrieve the label.
    ///
    /// # Returns
    /// An `Option<String>` containing the label if found, or `None` if not.
    pub fn get(&self, key: &LabelKey) -> Option<String> {
        self.store.get(key).cloned()
    }

    /// Edits the label associated with the given key.
    ///
    /// If a value is provided, it updates the label. If `None` is provided, it removes the label.
    ///
    /// # Parameters
    /// - `key`: The key for the label to edit.
    /// - `value`: An optional new value for the label.
    pub fn edit(&mut self, key: LabelKey, value: Option<String>) {
        if let Some(value) = value {
            self.store
                .entry(key)
                .and_modify(|e| *e = value.clone())
                .or_insert(value);
        } else {
            self.store.remove(&key);
        }
    }

    /// Removes the label associated with the given key.
    ///
    /// # Parameters
    /// - `key`: The key for the label to remove.
    pub fn remove(&mut self, key: LabelKey) {
        self.store.remove(&key);
    }

    /// Retrieves the label associated with the given Bitcoin address.
    ///
    /// # Parameters
    /// - `address`: The Bitcoin address for which to retrieve the label.
    ///
    /// # Returns
    /// An `Option<String>` containing the label if found, or `None` if not.
    pub fn address(&self, address: bitcoin::Address) -> Option<String> {
        self.get(&LabelKey::Address(address.as_unchecked().clone()))
    }

    /// Retrieves the label associated with the given outpoint.
    ///
    /// # Parameters
    /// - `outpoint`: The outpoint for which to retrieve the label.
    ///
    /// # Returns
    /// An `Option<String>` containing the label if found, or `None` if not.
    pub fn outpoint(&self, outpoint: OutPoint) -> Option<String> {
        self.get(&LabelKey::OutPoint(outpoint))
    }

    /// Retrieves the label associated with the given transaction ID.
    ///
    /// # Parameters
    /// - `txid`: The transaction ID for which to retrieve the label.
    ///
    /// # Returns
    /// An `Option<String>` containing the label if found, or `None` if not.
    pub fn transaction(&self, txid: bitcoin::Txid) -> Option<String> {
        self.get(&LabelKey::Transaction(txid))
    }
}
