//! Label store for user-facing text labels.
//!
//! The `SpLabelStore` manages user-assigned labels for outpoints and transactions.
//! This provides UI-level labeling similar to bwk's LabelStore.
//!
//! **Note:** These are user-facing text labels (UI), NOT SP protocol labels
//! (m-value for address derivation). Those are separate concepts.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

use bitcoin::{OutPoint, Txid};
use serde::{Deserialize, Serialize};

// LabelKey

/// A key for labeling either an outpoint or a transaction.
///
/// SP wallets have a single address, so no Address variant is needed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialOrd, Ord, Eq, PartialEq, Hash)]
pub enum LabelKey {
    /// Label for a specific UTXO
    OutPoint(OutPoint),
    /// Label for a transaction
    Transaction(Txid),
}

impl LabelKey {
    /// Create a LabelKey for an outpoint.
    pub fn outpoint(outpoint: OutPoint) -> Self {
        Self::OutPoint(outpoint)
    }

    /// Create a LabelKey for a transaction.
    pub fn transaction(txid: Txid) -> Self {
        Self::Transaction(txid)
    }

    /// Convert to a string representation for use as JSON map key.
    fn to_string_key(&self) -> String {
        match self {
            LabelKey::OutPoint(op) => format!("op:{}:{}", op.txid, op.vout),
            LabelKey::Transaction(txid) => format!("tx:{}", txid),
        }
    }

    /// Parse from a string key representation.
    fn from_string_key(s: &str) -> Option<Self> {
        if let Some(rest) = s.strip_prefix("op:") {
            // Format: "op:<txid>:<vout>"
            let parts: Vec<&str> = rest.rsplitn(2, ':').collect();
            if parts.len() == 2 {
                let vout: u32 = parts[0].parse().ok()?;
                let txid = Txid::from_str(parts[1]).ok()?;
                return Some(LabelKey::OutPoint(OutPoint { txid, vout }));
            }
        } else if let Some(rest) = s.strip_prefix("tx:") {
            // Format: "tx:<txid>"
            let txid = Txid::from_str(rest).ok()?;
            return Some(LabelKey::Transaction(txid));
        }
        None
    }
}

/// Custom serialization module for BTreeMap<LabelKey, String> as JSON object with string keys.
mod label_map_serde {
    use super::*;
    use serde::de::{MapAccess, Visitor};
    use serde::ser::SerializeMap;
    use serde::{Deserializer, Serializer};
    use std::fmt;

    pub fn serialize<S>(map: &BTreeMap<LabelKey, String>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut ser_map = serializer.serialize_map(Some(map.len()))?;
        for (key, value) in map {
            ser_map.serialize_entry(&key.to_string_key(), value)?;
        }
        ser_map.end()
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<BTreeMap<LabelKey, String>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct LabelMapVisitor;

        impl<'de> Visitor<'de> for LabelMapVisitor {
            type Value = BTreeMap<LabelKey, String>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a map with string keys representing LabelKey")
            }

            fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut map = BTreeMap::new();
                while let Some((key_str, value)) = access.next_entry::<String, String>()? {
                    let key = LabelKey::from_string_key(&key_str).ok_or_else(|| {
                        serde::de::Error::custom(format!("invalid label key: {}", key_str))
                    })?;
                    map.insert(key, value);
                }
                Ok(map)
            }
        }

        deserializer.deserialize_map(LabelMapVisitor)
    }
}

// LabelStoreError

/// Errors that can occur in the label store.
#[derive(Debug, thiserror::Error)]
pub enum LabelStoreError {
    /// IO error (file not found, permission denied, etc.)
    #[error("io error: {0}")]
    Io(String),
    /// JSON parsing error
    #[error("parse error: {0}")]
    Parse(String),
}

// SpLabelStore

/// Storage for user-facing text labels.
///
/// This store maintains a map of LabelKey to String labels, providing methods
/// for CRUD operations and persistence. Labels are user-facing text for UI
/// purposes, not to be confused with SP protocol labels (m-values).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpLabelStore {
    /// The internal store mapping keys to label strings
    #[serde(with = "label_map_serde", default)]
    store: BTreeMap<LabelKey, String>,

    /// Path for persistence (not serialized)
    #[serde(skip)]
    path: Option<PathBuf>,

    /// Whether persistence is enabled (not serialized)
    #[serde(skip)]
    persist: bool,
}

impl SpLabelStore {
    // Constructors

    /// Create a new empty label store.
    pub fn new() -> Self {
        Self {
            store: BTreeMap::new(),
            path: None,
            persist: false,
        }
    }

    /// Create a new label store with a persistence path.
    pub fn with_path(path: PathBuf) -> Self {
        Self {
            store: BTreeMap::new(),
            path: Some(path),
            persist: false,
        }
    }

    /// Load a label store from a JSON file.
    ///
    /// The loaded store will have its path set but persist disabled.
    /// Call `enable_persist(true)` to enable persistence.
    pub fn from_file(path: PathBuf) -> Result<Self, LabelStoreError> {
        let content = fs::read_to_string(&path).map_err(|e| {
            LabelStoreError::Io(format!(
                "failed to read labels from {}: {}",
                path.display(),
                e
            ))
        })?;
        let mut store: SpLabelStore = serde_json::from_str(&content)
            .map_err(|e| LabelStoreError::Parse(format!("failed to parse labels: {}", e)))?;
        store.path = Some(path);
        store.persist = false;
        Ok(store)
    }

    /// Enable or disable persistence (builder pattern).
    pub fn enable_persist(mut self, persist: bool) -> Self {
        self.persist = persist;
        self
    } // Getters

    /// Returns a reference to a label by key.
    pub fn get(&self, key: &LabelKey) -> Option<&String> {
        self.store.get(key)
    } // Mutators

    /// Set a label for a key.
    ///
    /// If a label already exists for the key, it will be replaced.
    pub fn set(&mut self, key: LabelKey, value: String) {
        self.store.insert(key, value);
    }

    /// Remove a label by key.
    ///
    /// Returns the removed label if it existed.
    pub fn remove(&mut self, key: &LabelKey) -> Option<String> {
        self.store.remove(key)
    } // Convenience methods

    /// Get a label for an outpoint.
    pub fn outpoint(&self, outpoint: &OutPoint) -> Option<&String> {
        self.store.get(&LabelKey::OutPoint(*outpoint))
    }

    /// Get a label for a transaction.
    pub fn transaction(&self, txid: &Txid) -> Option<&String> {
        self.store.get(&LabelKey::Transaction(*txid))
    }

    /// Set a label for an outpoint.
    pub fn set_outpoint(&mut self, outpoint: OutPoint, label: String) {
        self.store.insert(LabelKey::OutPoint(outpoint), label);
    }

    /// Set a label for a transaction.
    pub fn set_transaction(&mut self, txid: Txid, label: String) {
        self.store.insert(LabelKey::Transaction(txid), label);
    } // Queries

    /// Returns the number of labels in the store.
    pub fn len(&self) -> usize {
        self.store.len()
    }

    /// Returns true if the store is empty.
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    } // Persistence

    /// Persist the store to disk.
    ///
    /// Does nothing if persistence is disabled or no path is set.
    pub fn persist(&self) {
        if !self.persist {
            return;
        }
        let Some(path) = &self.path else {
            return;
        };

        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        match serde_json::to_string_pretty(self) {
            Ok(content) => {
                if let Err(e) = fs::write(path, content) {
                    log::error!("SpLabelStore::persist() failed to write: {}", e);
                }
            }
            Err(e) => log::error!("SpLabelStore::persist() failed to serialize: {}", e),
        }
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::hashes::Hash;

    fn test_outpoint() -> OutPoint {
        OutPoint {
            txid: Txid::from_byte_array([1u8; 32]),
            vout: 0,
        }
    }

    fn test_outpoint_2() -> OutPoint {
        OutPoint {
            txid: Txid::from_byte_array([2u8; 32]),
            vout: 1,
        }
    }

    fn test_txid() -> Txid {
        Txid::from_byte_array([3u8; 32])
    }

    fn test_txid_2() -> Txid {
        Txid::from_byte_array([4u8; 32])
    }

    #[test]
    fn test_label_key_variants() {
        let outpoint = test_outpoint();
        let txid = test_txid();

        let key_op = LabelKey::outpoint(outpoint);
        let key_tx = LabelKey::transaction(txid);

        assert!(matches!(key_op, LabelKey::OutPoint(_)));
        assert!(matches!(key_tx, LabelKey::Transaction(_)));
    }

    #[test]
    fn test_label_key_ord() {
        // Verify that LabelKey implements Ord correctly for BTreeMap usage
        let key1 = LabelKey::OutPoint(test_outpoint());
        let key2 = LabelKey::Transaction(test_txid());

        // Just verify comparison works without panic
        let _ = key1.cmp(&key2);
        let _ = key1 == key2;
    }

    #[test]
    fn test_label_store_new_empty() {
        let store = SpLabelStore::new();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_label_store_set_get() {
        let mut store = SpLabelStore::new();
        let key = LabelKey::OutPoint(test_outpoint());

        store.set(key.clone(), "my label".to_string());

        assert_eq!(store.len(), 1);
        assert_eq!(store.get(&key), Some(&"my label".to_string()));
    }

    #[test]
    fn test_label_store_set_get_outpoint() {
        let mut store = SpLabelStore::new();
        let outpoint = test_outpoint();

        store.set_outpoint(outpoint, "utxo label".to_string());

        assert_eq!(store.outpoint(&outpoint), Some(&"utxo label".to_string()));
    }

    #[test]
    fn test_label_store_set_get_transaction() {
        let mut store = SpLabelStore::new();
        let txid = test_txid();

        store.set_transaction(txid, "tx label".to_string());

        assert_eq!(store.transaction(&txid), Some(&"tx label".to_string()));
    }

    #[test]
    fn test_label_store_remove() {
        let mut store = SpLabelStore::new();
        let key = LabelKey::OutPoint(test_outpoint());

        store.set(key.clone(), "label".to_string());
        assert!(!store.is_empty());

        let removed = store.remove(&key);
        assert_eq!(removed, Some("label".to_string()));
        assert!(store.is_empty());
        assert!(store.get(&key).is_none());
    }

    #[test]
    fn test_label_store_overwrite() {
        let mut store = SpLabelStore::new();
        let key = LabelKey::OutPoint(test_outpoint());

        store.set(key.clone(), "label a".to_string());
        assert_eq!(store.get(&key), Some(&"label a".to_string()));

        store.set(key.clone(), "label b".to_string());
        assert_eq!(store.get(&key), Some(&"label b".to_string()));

        // Still only one entry
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn test_label_store_multiple_entries() {
        let mut store = SpLabelStore::new();

        store.set_outpoint(test_outpoint(), "outpoint 1".to_string());
        store.set_outpoint(test_outpoint_2(), "outpoint 2".to_string());
        store.set_transaction(test_txid(), "tx 1".to_string());
        store.set_transaction(test_txid_2(), "tx 2".to_string());

        assert_eq!(store.len(), 4);
        assert_eq!(
            store.outpoint(&test_outpoint()),
            Some(&"outpoint 1".to_string())
        );
        assert_eq!(
            store.outpoint(&test_outpoint_2()),
            Some(&"outpoint 2".to_string())
        );
        assert_eq!(store.transaction(&test_txid()), Some(&"tx 1".to_string()));
        assert_eq!(store.transaction(&test_txid_2()), Some(&"tx 2".to_string()));
    }

    #[test]
    fn test_label_store_get_nonexistent() {
        let store = SpLabelStore::new();

        assert!(store.outpoint(&test_outpoint()).is_none());
        assert!(store.transaction(&test_txid()).is_none());
        assert!(store.get(&LabelKey::OutPoint(test_outpoint())).is_none());
    }

    #[test]
    fn test_label_store_serde_roundtrip() {
        let mut store = SpLabelStore::new();
        store.set_outpoint(test_outpoint(), "utxo".to_string());
        store.set_transaction(test_txid(), "tx".to_string());

        let json = serde_json::to_string(&store).expect("serialize");
        let loaded: SpLabelStore = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.outpoint(&test_outpoint()), Some(&"utxo".to_string()));
        assert_eq!(loaded.transaction(&test_txid()), Some(&"tx".to_string()));
    }

    #[test]
    fn test_label_store_persistence() {
        use std::env;

        let temp_dir = env::temp_dir().join("bwk-sp-label-store-test");
        let _ = fs::remove_dir_all(&temp_dir);
        let _ = fs::create_dir_all(&temp_dir);

        let path = temp_dir.join("labels.json");

        // Create and populate store
        let mut store = SpLabelStore::with_path(path.clone()).enable_persist(true);
        store.set_outpoint(test_outpoint(), "my utxo".to_string());
        store.set_transaction(test_txid(), "my tx".to_string());
        store.persist();

        // Load from file
        let loaded = SpLabelStore::from_file(path).expect("load");

        assert_eq!(loaded.len(), 2);
        assert_eq!(
            loaded.outpoint(&test_outpoint()),
            Some(&"my utxo".to_string())
        );
        assert_eq!(loaded.transaction(&test_txid()), Some(&"my tx".to_string()));

        // Clean up
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_label_store_persist_disabled() {
        use std::env;

        let temp_dir = env::temp_dir().join("bwk-sp-label-store-no-persist-test");
        let _ = fs::remove_dir_all(&temp_dir);
        let _ = fs::create_dir_all(&temp_dir);

        let path = temp_dir.join("labels.json");

        // Create store with persist disabled
        let mut store = SpLabelStore::with_path(path.clone()).enable_persist(false);
        store.set_outpoint(test_outpoint(), "label".to_string());
        store.persist();

        // File should not exist
        assert!(!path.exists());

        // Clean up
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_label_store_from_file_not_found() {
        let result = SpLabelStore::from_file(PathBuf::from("/nonexistent/path/labels.json"));
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(matches!(e, LabelStoreError::Io(_)));
        }
    }

    #[test]
    fn test_label_store_remove_nonexistent() {
        let mut store = SpLabelStore::new();
        let key = LabelKey::OutPoint(test_outpoint());

        // Remove from empty store should return None
        let removed = store.remove(&key);
        assert!(removed.is_none());
        assert!(store.is_empty());
    }

    #[test]
    fn test_label_key_serde_roundtrip() {
        let key_op = LabelKey::OutPoint(test_outpoint());
        let key_tx = LabelKey::Transaction(test_txid());

        let json_op = serde_json::to_string(&key_op).expect("serialize outpoint key");
        let json_tx = serde_json::to_string(&key_tx).expect("serialize tx key");

        let loaded_op: LabelKey = serde_json::from_str(&json_op).expect("deserialize outpoint key");
        let loaded_tx: LabelKey = serde_json::from_str(&json_tx).expect("deserialize tx key");

        assert_eq!(key_op, loaded_op);
        assert_eq!(key_tx, loaded_tx);
    }

    #[test]
    fn test_label_store_error_display() {
        // Test Io error variant
        let err = LabelStoreError::Io("disk full".to_string());
        let msg = err.to_string();
        assert!(msg.contains("io error"));
        assert!(msg.contains("disk full"));

        // Test Parse error variant
        let err = LabelStoreError::Parse("invalid label format".to_string());
        let msg = err.to_string();
        assert!(msg.contains("parse error"));
        assert!(msg.contains("invalid label format"));
    }
}
