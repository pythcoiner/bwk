//! Transaction store for Silent Payment transactions.
//!
//! The `SpTxStore` manages a collection of `SpTxEntry` items, which track
//! transactions sent or received by the wallet. This provides similar
//! functionality to bwk's transaction tracking.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use bitcoin::{Transaction, Txid};
use serde::{Deserialize, Serialize};

// TxDirection

/// Direction of a transaction relative to the wallet.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TxDirection {
    /// Received payment (incoming funds)
    Incoming,
    /// Sent payment (outgoing funds)
    Outgoing,
    /// Self-transfer (e.g., consolidation, change-only)
    Internal,
}

// SpTxEntry

/// A transaction entry in the store.
///
/// Contains the transaction metadata and optionally the full transaction data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpTxEntry {
    /// The transaction ID
    pub txid: Txid,
    /// The full transaction if available
    pub tx: Option<Transaction>,
    /// Direction of funds flow
    pub direction: TxDirection,
    /// Net amount in satoshis (positive for incoming)
    pub amount: u64,
    /// Fee in satoshis if we sent the transaction
    pub fee: Option<u64>,
    /// Confirmation height if confirmed
    pub height: Option<u32>,
    /// Unix timestamp of the transaction
    pub timestamp: Option<u64>,
    /// User-assigned label
    pub label: Option<String>,
}

impl SpTxEntry {
    // Constructors

    /// Create a new transaction entry.
    pub fn new(txid: Txid, direction: TxDirection, amount: u64) -> Self {
        Self {
            txid,
            tx: None,
            direction,
            amount,
            fee: None,
            height: None,
            timestamp: None,
            label: None,
        }
    }

    /// Create a new transaction entry with full transaction data.
    pub fn with_tx(txid: Txid, tx: Transaction, direction: TxDirection, amount: u64) -> Self {
        Self {
            txid,
            tx: Some(tx),
            direction,
            amount,
            fee: None,
            height: None,
            timestamp: None,
            label: None,
        }
    } // Getters

    /// Returns the transaction ID.
    pub fn txid(&self) -> &Txid {
        &self.txid
    }

    /// Returns the full transaction if available.
    pub fn tx(&self) -> Option<&Transaction> {
        self.tx.as_ref()
    }

    /// Returns the transaction direction.
    pub fn direction(&self) -> &TxDirection {
        &self.direction
    }

    /// Returns the amount in satoshis.
    pub fn amount(&self) -> u64 {
        self.amount
    }

    /// Returns the fee in satoshis if known.
    pub fn fee(&self) -> Option<u64> {
        self.fee
    }

    /// Returns the confirmation height if confirmed.
    pub fn height(&self) -> Option<u32> {
        self.height
    }

    /// Returns the timestamp if known.
    pub fn timestamp(&self) -> Option<u64> {
        self.timestamp
    }

    /// Returns the label if set.
    pub fn label(&self) -> Option<&String> {
        self.label.as_ref()
    }

    /// Returns true if the transaction is confirmed.
    pub fn is_confirmed(&self) -> bool {
        self.height.is_some()
    }
}

// TxStoreError

/// Errors that can occur in the transaction store.
#[derive(Debug, thiserror::Error)]
pub enum TxStoreError {
    /// IO error (file not found, permission denied, etc.)
    #[error("io error: {0}")]
    Io(String),
    /// JSON parsing error
    #[error("parse error: {0}")]
    Parse(String),
}

// SpTxStore

/// Storage for transactions.
///
/// This store maintains a map of Txid to SpTxEntry, providing methods for
/// CRUD operations, queries, and persistence.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SpTxStore {
    /// The internal store mapping txids to transaction entries
    store: BTreeMap<Txid, SpTxEntry>,

    /// Directory containing the JSON file, if persistence is enabled (not
    /// serialized).
    #[serde(skip)]
    dir: Option<PathBuf>,

    /// Whether persistence is enabled (not serialized)
    #[serde(skip)]
    persist: bool,
}

impl SpTxStore {
    /// Filename used under the account directory for this store's JSON.
    pub const FILENAME: &'static str = "txs.json";

    // Constructors

    /// Create a new empty transaction store.
    pub fn new() -> Self {
        Self {
            store: BTreeMap::new(),
            dir: None,
            persist: false,
        }
    }

    /// Create a new transaction store rooted at the given directory.
    ///
    /// The store persists to `{dir}/{FILENAME}`.
    pub fn with_path(dir: PathBuf) -> Self {
        Self {
            store: BTreeMap::new(),
            dir: Some(dir),
            persist: false,
        }
    }

    /// Load a transaction store from `{dir}/{FILENAME}`.
    ///
    /// The loaded store will have its dir set but persist disabled.
    /// Call `enable_persist(true)` to enable persistence.
    pub fn from_file(dir: PathBuf) -> Result<Self, TxStoreError> {
        let path = dir.join(Self::FILENAME);
        let content = fs::read_to_string(&path).map_err(|e| {
            TxStoreError::Io(format!("failed to read txs from {}: {}", path.display(), e))
        })?;
        let mut store: SpTxStore = serde_json::from_str(&content)
            .map_err(|e| TxStoreError::Parse(format!("failed to parse txs: {}", e)))?;
        store.dir = Some(dir);
        store.persist = false;
        Ok(store)
    }

    /// Enable or disable persistence (builder pattern).
    pub fn enable_persist(mut self, persist: bool) -> Self {
        self.persist = persist;
        self
    } // Getters

    /// Returns a reference to a transaction entry by txid.
    pub fn get(&self, txid: &Txid) -> Option<&SpTxEntry> {
        self.store.get(txid)
    }

    /// Returns a mutable reference to a transaction entry by txid.
    pub fn get_mut(&mut self, txid: &Txid) -> Option<&mut SpTxEntry> {
        self.store.get_mut(txid)
    } // Mutators

    /// Insert a transaction entry.
    ///
    /// If an entry with the same txid already exists, it will be replaced.
    pub fn insert(&mut self, entry: SpTxEntry) {
        self.store.insert(entry.txid, entry);
    }

    /// Remove a transaction entry by txid.
    pub fn remove(&mut self, txid: &Txid) -> Option<SpTxEntry> {
        self.store.remove(txid)
    }

    /// Update the confirmation height of a transaction.
    pub fn update_height(&mut self, txid: &Txid, height: Option<u32>) {
        if let Some(entry) = self.store.get_mut(txid) {
            entry.height = height;
        }
    }

    /// Update the label of a transaction.
    pub fn update_label(&mut self, txid: &Txid, label: String) {
        if let Some(entry) = self.store.get_mut(txid) {
            entry.label = Some(label);
        }
    } // Queries

    /// Returns all transaction entries.
    pub fn transactions(&self) -> Vec<&SpTxEntry> {
        self.store.values().collect()
    }

    /// Returns the number of transactions in the store.
    pub fn len(&self) -> usize {
        self.store.len()
    }

    /// Returns true if the store is empty.
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    } // Persistence

    /// Persist the store to `{dir}/{FILENAME}`.
    ///
    /// Does nothing if persistence is disabled or no directory is set.
    pub fn persist(&self) {
        if !self.persist {
            return;
        }
        let Some(dir) = &self.dir else {
            return;
        };
        let _ = fs::create_dir_all(dir);
        let path = dir.join(Self::FILENAME);

        match serde_json::to_string_pretty(self) {
            Ok(content) => {
                if let Err(e) = fs::write(&path, content) {
                    log::error!("SpTxStore::persist() failed to write: {}", e);
                }
            }
            Err(e) => log::error!("SpTxStore::persist() failed to serialize: {}", e),
        }
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::hashes::Hash;

    fn test_txid() -> Txid {
        Txid::from_byte_array([1u8; 32])
    }

    fn test_txid_2() -> Txid {
        Txid::from_byte_array([2u8; 32])
    }

    fn test_txid_3() -> Txid {
        Txid::from_byte_array([3u8; 32])
    }

    fn test_entry(txid: Txid, amount: u64, direction: TxDirection) -> SpTxEntry {
        SpTxEntry::new(txid, direction, amount)
    }

    #[test]
    fn test_tx_direction_serde() {
        let incoming = TxDirection::Incoming;
        let outgoing = TxDirection::Outgoing;
        let internal = TxDirection::Internal;

        let json_in = serde_json::to_string(&incoming).unwrap();
        let json_out = serde_json::to_string(&outgoing).unwrap();
        let json_int = serde_json::to_string(&internal).unwrap();

        assert_eq!(
            serde_json::from_str::<TxDirection>(&json_in).unwrap(),
            TxDirection::Incoming
        );
        assert_eq!(
            serde_json::from_str::<TxDirection>(&json_out).unwrap(),
            TxDirection::Outgoing
        );
        assert_eq!(
            serde_json::from_str::<TxDirection>(&json_int).unwrap(),
            TxDirection::Internal
        );
    }

    #[test]
    fn test_tx_entry_new() {
        let txid = test_txid();
        let entry = SpTxEntry::new(txid, TxDirection::Incoming, 50000);

        assert_eq!(entry.txid(), &txid);
        assert_eq!(entry.amount(), 50000);
        assert_eq!(entry.direction(), &TxDirection::Incoming);
        assert!(entry.tx().is_none());
        assert!(entry.fee().is_none());
        assert!(entry.height().is_none());
        assert!(entry.timestamp().is_none());
        assert!(entry.label().is_none());
        assert!(!entry.is_confirmed());
    }

    #[test]
    fn test_tx_entry_fields() {
        let txid = test_txid();
        let mut entry = SpTxEntry::new(txid, TxDirection::Outgoing, 10000);
        entry.fee = Some(500);
        entry.height = Some(800000);
        entry.timestamp = Some(1700000000);
        entry.label = Some("test payment".to_string());

        assert_eq!(entry.fee(), Some(500));
        assert_eq!(entry.height(), Some(800000));
        assert!(entry.is_confirmed());
        assert_eq!(entry.timestamp(), Some(1700000000));
        assert_eq!(entry.label(), Some(&"test payment".to_string()));
    }

    #[test]
    fn test_tx_store_new_empty() {
        let store = SpTxStore::new();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_tx_store_insert_get() {
        let mut store = SpTxStore::new();
        let txid = test_txid();
        let entry = test_entry(txid, 50000, TxDirection::Incoming);

        store.insert(entry);

        assert_eq!(store.len(), 1);
        let retrieved = store.get(&txid).expect("entry should exist");
        assert_eq!(retrieved.amount(), 50000);
        assert_eq!(retrieved.direction(), &TxDirection::Incoming);
    }

    #[test]
    fn test_tx_store_get_mut() {
        let mut store = SpTxStore::new();
        let txid = test_txid();
        let entry = test_entry(txid, 50000, TxDirection::Incoming);

        store.insert(entry);

        // Modify through get_mut
        if let Some(e) = store.get_mut(&txid) {
            e.height = Some(100);
        }

        assert_eq!(store.get(&txid).unwrap().height(), Some(100));
    }

    #[test]
    fn test_tx_store_remove() {
        let mut store = SpTxStore::new();
        let txid = test_txid();
        let entry = test_entry(txid, 50000, TxDirection::Incoming);

        store.insert(entry);
        assert!(!store.is_empty());

        let removed = store.remove(&txid);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().amount(), 50000);
        assert!(store.is_empty());
        assert!(store.get(&txid).is_none());
    }

    #[test]
    fn test_tx_store_update_height() {
        let mut store = SpTxStore::new();
        let txid = test_txid();
        let entry = test_entry(txid, 50000, TxDirection::Incoming);

        store.insert(entry);
        assert!(store.get(&txid).unwrap().height().is_none());

        store.update_height(&txid, Some(800000));
        assert_eq!(store.get(&txid).unwrap().height(), Some(800000));

        // Update to None (unconfirmed)
        store.update_height(&txid, None);
        assert!(store.get(&txid).unwrap().height().is_none());
    }

    #[test]
    fn test_tx_store_update_label() {
        let mut store = SpTxStore::new();
        let txid = test_txid();
        let entry = test_entry(txid, 50000, TxDirection::Incoming);

        store.insert(entry);
        assert!(store.get(&txid).unwrap().label().is_none());

        store.update_label(&txid, "payment for coffee".to_string());
        assert_eq!(
            store.get(&txid).unwrap().label(),
            Some(&"payment for coffee".to_string())
        );
    }

    #[test]
    fn test_tx_store_transactions() {
        let mut store = SpTxStore::new();

        store.insert(test_entry(test_txid(), 10000, TxDirection::Incoming));
        store.insert(test_entry(test_txid_2(), 20000, TxDirection::Outgoing));
        store.insert(test_entry(test_txid_3(), 30000, TxDirection::Internal));

        let txs = store.transactions();
        assert_eq!(txs.len(), 3);

        // Verify all amounts are present
        let amounts: Vec<u64> = txs.iter().map(|e| e.amount()).collect();
        assert!(amounts.contains(&10000));
        assert!(amounts.contains(&20000));
        assert!(amounts.contains(&30000));
    }

    #[test]
    fn test_tx_store_serde_roundtrip() {
        let mut store = SpTxStore::new();
        let mut entry = test_entry(test_txid(), 50000, TxDirection::Incoming);
        entry.height = Some(800000);
        entry.label = Some("test".to_string());
        store.insert(entry);

        let json = serde_json::to_string(&store).expect("serialize");
        let loaded: SpTxStore = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(loaded.len(), 1);
        let loaded_entry = loaded.get(&test_txid()).unwrap();
        assert_eq!(loaded_entry.amount(), 50000);
        assert_eq!(loaded_entry.height(), Some(800000));
        assert_eq!(loaded_entry.label(), Some(&"test".to_string()));
    }

    #[test]
    fn test_tx_store_persistence() {
        use std::env;

        let temp_dir = env::temp_dir().join("bwk-sp-tx-store-test");
        let _ = fs::remove_dir_all(&temp_dir);
        let _ = fs::create_dir_all(&temp_dir);

        // Create and populate store
        let mut store = SpTxStore::with_path(temp_dir.clone()).enable_persist(true);
        let mut entry = test_entry(test_txid(), 50000, TxDirection::Incoming);
        entry.height = Some(800000);
        store.insert(entry);
        store.persist();

        assert!(temp_dir.join(SpTxStore::FILENAME).exists());

        // Load from dir
        let loaded = SpTxStore::from_file(temp_dir.clone()).expect("load");

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.get(&test_txid()).unwrap().amount(), 50000);
        assert_eq!(loaded.get(&test_txid()).unwrap().height(), Some(800000));

        // Clean up
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_tx_store_persist_disabled() {
        use std::env;

        let temp_dir = env::temp_dir().join("bwk-sp-tx-store-no-persist-test");
        let _ = fs::remove_dir_all(&temp_dir);
        let _ = fs::create_dir_all(&temp_dir);

        // Create store with persist disabled
        let mut store = SpTxStore::with_path(temp_dir.clone()).enable_persist(false);
        store.insert(test_entry(test_txid(), 50000, TxDirection::Incoming));
        store.persist();

        // File should not exist
        assert!(!temp_dir.join(SpTxStore::FILENAME).exists());

        // Clean up
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_tx_store_from_file_not_found() {
        // Directory has no txs.json under it, so load must fail with Io.
        let result = SpTxStore::from_file(PathBuf::from("/nonexistent/path"));
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(matches!(e, TxStoreError::Io(_)));
        }
    }

    #[test]
    fn test_tx_store_insert_replaces() {
        let mut store = SpTxStore::new();
        let txid = test_txid();

        // Insert first entry
        store.insert(test_entry(txid, 10000, TxDirection::Incoming));
        assert_eq!(store.get(&txid).unwrap().amount(), 10000);

        // Insert second entry with same txid
        store.insert(test_entry(txid, 20000, TxDirection::Outgoing));
        assert_eq!(store.get(&txid).unwrap().amount(), 20000);
        assert_eq!(
            store.get(&txid).unwrap().direction(),
            &TxDirection::Outgoing
        );

        // Still only one entry
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn test_tx_store_update_nonexistent() {
        let mut store = SpTxStore::new();
        let txid = test_txid();

        // These should do nothing (no panic)
        store.update_height(&txid, Some(100));
        store.update_label(&txid, "label".to_string());

        assert!(store.is_empty());
    }

    #[test]
    fn test_tx_store_error_display() {
        // Test Io error variant
        let err = TxStoreError::Io("cannot read file".to_string());
        let msg = err.to_string();
        assert!(msg.contains("io error"));
        assert!(msg.contains("cannot read file"));

        // Test Parse error variant
        let err = TxStoreError::Parse("malformed transaction data".to_string());
        let msg = err.to_string();
        assert!(msg.contains("parse error"));
        assert!(msg.contains("malformed transaction data"));
    }
}
