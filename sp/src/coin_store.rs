//! Coin store for Silent Payment outputs.
//!
//! The `SpCoinStore` manages a collection of `SpCoinEntry` items, which wrap
//! `OwnedOutput` from spdk-core with additional metadata. This provides a
//! similar interface to bwk's CoinStore but for silent payment outputs.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::PathBuf;

use bitcoin::{Amount, OutPoint, ScriptBuf};
use serde::{Deserialize, Serialize};
use silentpayments::receiving::Label;
use spdk_core::{OutputSpendStatus, OwnedOutput};

// SpCoinEntry

/// A silent payment coin entry wrapping an OutPoint and OwnedOutput.
///
/// This is the equivalent of bwk's `CoinEntry` but for silent payment outputs.
/// It provides accessor methods that mirror the bwk API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpCoinEntry {
    /// The outpoint (txid:vout) identifying this UTXO
    outpoint: OutPoint,
    /// The owned output data from spdk-core
    output: OwnedOutput,
}

impl SpCoinEntry {
    // Constructors

    /// Create a new coin entry from an outpoint and owned output.
    pub fn new(outpoint: OutPoint, output: OwnedOutput) -> Self {
        Self { outpoint, output }
    } // Getters (accessors)

    /// Returns the block height where this output was confirmed.
    pub fn height(&self) -> u32 {
        self.output.blockheight.to_consensus_u32()
    }

    /// Returns the amount as a bitcoin::Amount.
    pub fn amount(&self) -> Amount {
        self.output.amount
    }

    /// Returns the amount in satoshis.
    pub fn amount_sat(&self) -> u64 {
        self.output.amount.to_sat()
    }

    /// Returns a reference to the output script.
    pub fn script(&self) -> &ScriptBuf {
        &self.output.script
    }

    /// Returns a reference to the outpoint.
    pub fn outpoint(&self) -> &OutPoint {
        &self.outpoint
    }

    /// Returns the outpoint as a string in "txid:vout" format.
    pub fn outpoint_str(&self) -> String {
        format!("{}:{}", self.outpoint.txid, self.outpoint.vout)
    }

    /// Returns a reference to the spend status.
    pub fn status(&self) -> &OutputSpendStatus {
        &self.output.spend_status
    }

    /// Returns true if the output is spendable (unspent).
    pub fn is_spendable(&self) -> bool {
        matches!(self.output.spend_status, OutputSpendStatus::Unspent)
    }

    /// Returns a reference to the tweak bytes.
    pub fn tweak(&self) -> &[u8; 32] {
        &self.output.tweak
    }

    /// Returns the SP label if present.
    pub fn label(&self) -> Option<&Label> {
        self.output.label.as_ref()
    }

    /// Returns a reference to the underlying OwnedOutput.
    pub fn owned_output(&self) -> &OwnedOutput {
        &self.output
    }

    /// Returns a mutable reference to the underlying OwnedOutput.
    pub fn owned_output_mut(&mut self) -> &mut OwnedOutput {
        &mut self.output
    }
}

// CoinState

/// Balance summary for the coin store.
///
/// Provides counts and totals for confirmed and unconfirmed coins.
/// For Silent Payments, we only see confirmed outputs, so unconfirmed
/// fields are always zero.
#[derive(Debug, Clone, Default)]
pub struct CoinState {
    /// Map of all spendable coins
    pub coins: BTreeMap<OutPoint, SpCoinEntry>,
    /// Number of confirmed (spendable) coins
    pub confirmed_coins: usize,
    /// Total balance of confirmed coins in satoshis
    pub confirmed_balance: u64,
    /// Number of unconfirmed coins (always 0 for SP)
    pub unconfirmed_coins: usize,
    /// Total balance of unconfirmed coins (always 0 for SP)
    pub unconfirmed_balance: u64,
}

// CoinStoreError

/// Errors that can occur in the coin store.
#[derive(Debug, thiserror::Error)]
pub enum CoinStoreError {
    /// IO error (file not found, permission denied, etc.)
    #[error("io error: {0}")]
    Io(String),
    /// JSON parsing error
    #[error("parse error: {0}")]
    Parse(String),
}

// SpCoinStore

/// Storage for silent payment coins.
///
/// This store maintains a map of OutPoint to SpCoinEntry, providing
/// methods for CRUD operations, queries, and persistence.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SpCoinStore {
    /// The internal store mapping outpoints to coin entries
    store: BTreeMap<OutPoint, SpCoinEntry>,

    /// Path for persistence (not serialized)
    #[serde(skip)]
    path: Option<PathBuf>,

    /// Whether persistence is enabled (not serialized)
    #[serde(skip)]
    persist: bool,
}

impl SpCoinStore {
    // Constructors

    /// Create a new empty coin store.
    pub fn new() -> Self {
        Self {
            store: BTreeMap::new(),
            path: None,
            persist: false,
        }
    }

    /// Create a new coin store with a persistence path.
    pub fn with_path(path: PathBuf) -> Self {
        Self {
            store: BTreeMap::new(),
            path: Some(path),
            persist: false,
        }
    }

    /// Load a coin store from a JSON file.
    ///
    /// The loaded store will have its path set but persist disabled.
    /// Call `enable_persist(true)` to enable persistence.
    pub fn from_file(path: PathBuf) -> Result<Self, CoinStoreError> {
        let content = fs::read_to_string(&path).map_err(|e| {
            CoinStoreError::Io(format!(
                "failed to read coins from {}: {}",
                path.display(),
                e
            ))
        })?;
        let mut store: SpCoinStore = serde_json::from_str(&content)
            .map_err(|e| CoinStoreError::Parse(format!("failed to parse coins: {}", e)))?;
        store.path = Some(path);
        store.persist = false;
        Ok(store)
    }

    /// Enable or disable persistence (builder pattern).
    pub fn enable_persist(mut self, persist: bool) -> Self {
        self.persist = persist;
        self
    } // Getters

    /// Returns a reference to a coin entry by outpoint.
    pub fn get(&self, outpoint: &OutPoint) -> Option<&SpCoinEntry> {
        self.store.get(outpoint)
    }

    /// Returns a mutable reference to a coin entry by outpoint.
    pub fn get_mut(&mut self, outpoint: &OutPoint) -> Option<&mut SpCoinEntry> {
        self.store.get_mut(outpoint)
    } // Mutators

    /// Insert a new coin entry from an outpoint and owned output.
    pub fn insert(&mut self, outpoint: OutPoint, output: OwnedOutput) {
        let entry = SpCoinEntry::new(outpoint, output);
        self.store.insert(outpoint, entry);
    }

    /// Insert multiple coin entries at once.
    pub fn insert_batch(&mut self, outputs: BTreeMap<OutPoint, OwnedOutput>) {
        for (outpoint, output) in outputs {
            self.insert(outpoint, output);
        }
    }

    /// Remove a coin entry by outpoint.
    pub fn remove(&mut self, outpoint: &OutPoint) -> Option<SpCoinEntry> {
        self.store.remove(outpoint)
    }

    /// Mark a coin as spent by a transaction.
    ///
    /// Updates the spend status to `Spent(spending_txid)`.
    pub fn mark_spent(&mut self, outpoint: &OutPoint, spending_txid: [u8; 32]) {
        if let Some(entry) = self.store.get_mut(outpoint) {
            entry.output.spend_status = OutputSpendStatus::Spent(spending_txid);
        }
    }

    /// Mark a coin's spending transaction as mined.
    ///
    /// Updates the spend status to `Mined(block_hash)`.
    pub fn mark_mined(&mut self, outpoint: &OutPoint, block_hash: [u8; 32]) {
        if let Some(entry) = self.store.get_mut(outpoint) {
            entry.output.spend_status = OutputSpendStatus::Mined(block_hash);
        }
    } // Queries

    /// Returns a reference to the internal coin map.
    pub fn coins(&self) -> &BTreeMap<OutPoint, SpCoinEntry> {
        &self.store
    }

    /// Returns a CoinState with only spendable (unspent) coins.
    pub fn spendable_coins(&self) -> CoinState {
        let mut state = CoinState::default();

        for (outpoint, entry) in &self.store {
            if entry.is_spendable() {
                state.coins.insert(*outpoint, entry.clone());
                state.confirmed_coins += 1;
                state.confirmed_balance += entry.amount_sat();
            }
        }

        state
    }

    /// Returns a set of all outpoints in the store.
    pub fn all_outpoints(&self) -> HashSet<OutPoint> {
        self.store.keys().copied().collect()
    }

    /// Returns the number of coins in the store.
    pub fn len(&self) -> usize {
        self.store.len()
    }

    /// Returns true if the store is empty.
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// Returns the total balance of spendable coins.
    pub fn balance(&self) -> Amount {
        let sats: u64 = self
            .store
            .values()
            .filter(|entry| entry.is_spendable())
            .map(|entry| entry.amount_sat())
            .sum();
        Amount::from_sat(sats)
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
                    log::error!("SpCoinStore::persist() failed to write: {}", e);
                }
            }
            Err(e) => log::error!("SpCoinStore::persist() failed to serialize: {}", e),
        }
    }

    /// Serialize the store to a JSON value.
    pub fn dump(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    /// Restore the store from a JSON value.
    pub fn restore(&mut self, value: serde_json::Value) -> Result<(), CoinStoreError> {
        let restored: SpCoinStore = serde_json::from_value(value)
            .map_err(|e| CoinStoreError::Parse(format!("failed to restore coins: {}", e)))?;
        self.store = restored.store;
        Ok(())
    }
}

// SpCoinSource (CoinSource for TxBuilder)

use std::sync::{Arc, Mutex};

use bwk_tx::coin::{CoinSpendInfo, CoinStatus};
use bwk_tx::tx_builder::CoinSource;
use bwk_tx::Coin;

/// Taproot keyspend satisfaction weight in WU (1 Schnorr signature = 66 WU)
const TR_KEYSPEND_SATISFACTION_WEIGHT: u64 = 66;

/// Implements [`CoinSource`] for the SP coin store, providing spendable coins
/// to [`TxBuilder`](bwk_tx::TxBuilder).
pub struct SpCoinSource(Arc<Mutex<SpCoinStore>>);

impl SpCoinSource {
    pub fn new(store: Arc<Mutex<SpCoinStore>>) -> Self {
        Self(store)
    }
}

impl CoinSource for SpCoinSource {
    fn spendable_coins(&self) -> Vec<Coin> {
        let store = self.0.lock().expect("poisoned");
        store
            .spendable_coins()
            .coins
            .into_iter()
            .map(|(outpoint, entry)| Coin {
                txout: bitcoin::TxOut {
                    value: entry.amount(),
                    script_pubkey: entry.script().clone(),
                },
                outpoint,
                height: Some(entry.height() as u64),
                sequence: bitcoin::Sequence::ENABLE_RBF_NO_LOCKTIME,
                status: CoinStatus::Confirmed,
                label: None,
                satisfaction_size: TR_KEYSPEND_SATISFACTION_WEIGHT,
                spend_info: CoinSpendInfo::Sp {
                    derivation: bitcoin::bip32::DerivationPath::default(),
                    tweak: *entry.tweak(),
                },
            })
            .collect()
    }
}

// MergedCoinSource (SP + BIP32 sub-accounts)

/// A [`CoinSource`] that merges coins from the SP coin store and zero or more
/// BIP32 sub-account coin sources (segwit, taproot, etc.).
pub struct MergedCoinSource {
    sp_source: SpCoinSource,
    bip32_sources: Vec<Box<dyn CoinSource>>,
}

impl MergedCoinSource {
    pub fn new(sp_source: SpCoinSource, bip32_sources: Vec<Box<dyn CoinSource>>) -> Self {
        Self {
            sp_source,
            bip32_sources,
        }
    }
}

impl CoinSource for MergedCoinSource {
    fn spendable_coins(&self) -> Vec<Coin> {
        let mut coins = self.sp_source.spendable_coins();
        for source in &self.bip32_sources {
            coins.extend(source.spendable_coins());
        }
        coins
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::absolute::Height;
    use bitcoin::hashes::Hash;
    use bitcoin::Txid;

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

    fn test_outpoint_3() -> OutPoint {
        OutPoint {
            txid: Txid::from_byte_array([3u8; 32]),
            vout: 2,
        }
    }

    fn test_owned_output(amount_sats: u64) -> OwnedOutput {
        OwnedOutput {
            blockheight: Height::from_consensus(100).unwrap(),
            tweak: [0u8; 32],
            amount: Amount::from_sat(amount_sats),
            script: ScriptBuf::new(),
            label: None,
            spend_status: OutputSpendStatus::Unspent,
        }
    }

    fn test_spent_output(amount_sats: u64) -> OwnedOutput {
        OwnedOutput {
            blockheight: Height::from_consensus(100).unwrap(),
            tweak: [0u8; 32],
            amount: Amount::from_sat(amount_sats),
            script: ScriptBuf::new(),
            label: None,
            spend_status: OutputSpendStatus::Spent([0u8; 32]),
        }
    }

    #[test]
    fn test_coin_entry_accessors() {
        let outpoint = test_outpoint();
        let output = test_owned_output(10000);
        let entry = SpCoinEntry::new(outpoint, output);

        assert_eq!(entry.height(), 100);
        assert_eq!(entry.amount(), Amount::from_sat(10000));
        assert_eq!(entry.amount_sat(), 10000);
        assert_eq!(entry.outpoint(), &outpoint);
        assert!(entry.outpoint_str().contains(":0"));
        assert!(entry.is_spendable());
        assert_eq!(entry.tweak(), &[0u8; 32]);
        assert!(entry.label().is_none());
    }

    #[test]
    fn test_coin_entry_is_spendable() {
        let outpoint = test_outpoint();

        // Unspent should be spendable
        let unspent = SpCoinEntry::new(outpoint, test_owned_output(10000));
        assert!(unspent.is_spendable());

        // Spent should not be spendable
        let spent = SpCoinEntry::new(outpoint, test_spent_output(10000));
        assert!(!spent.is_spendable());
    }

    #[test]
    fn test_coin_store_new_empty() {
        let store = SpCoinStore::new();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
        assert_eq!(store.balance(), Amount::from_sat(0));
    }

    #[test]
    fn test_coin_store_insert_get() {
        let mut store = SpCoinStore::new();
        let outpoint = test_outpoint();
        let output = test_owned_output(50000);

        store.insert(outpoint, output.clone());

        assert_eq!(store.len(), 1);
        let entry = store.get(&outpoint).expect("entry should exist");
        assert_eq!(entry.amount_sat(), 50000);
    }

    #[test]
    fn test_coin_store_insert_batch() {
        let mut store = SpCoinStore::new();
        let mut outputs = BTreeMap::new();
        outputs.insert(test_outpoint(), test_owned_output(10000));
        outputs.insert(test_outpoint_2(), test_owned_output(20000));
        outputs.insert(test_outpoint_3(), test_owned_output(30000));

        store.insert_batch(outputs);

        assert_eq!(store.len(), 3);
        assert!(store.get(&test_outpoint()).is_some());
        assert!(store.get(&test_outpoint_2()).is_some());
        assert!(store.get(&test_outpoint_3()).is_some());
    }

    #[test]
    fn test_coin_store_remove() {
        let mut store = SpCoinStore::new();
        let outpoint = test_outpoint();
        store.insert(outpoint, test_owned_output(10000));

        assert!(!store.is_empty());

        let removed = store.remove(&outpoint);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().amount_sat(), 10000);
        assert!(store.is_empty());
        assert!(store.get(&outpoint).is_none());
    }

    #[test]
    fn test_coin_store_mark_spent() {
        let mut store = SpCoinStore::new();
        let outpoint = test_outpoint();
        store.insert(outpoint, test_owned_output(10000));

        assert!(store.get(&outpoint).unwrap().is_spendable());

        let spending_txid = [42u8; 32];
        store.mark_spent(&outpoint, spending_txid);

        let entry = store.get(&outpoint).unwrap();
        assert!(!entry.is_spendable());
        assert!(matches!(entry.status(), OutputSpendStatus::Spent(txid) if *txid == spending_txid));
    }

    #[test]
    fn test_coin_store_mark_mined() {
        let mut store = SpCoinStore::new();
        let outpoint = test_outpoint();
        store.insert(outpoint, test_owned_output(10000));

        let block_hash = [99u8; 32];
        store.mark_mined(&outpoint, block_hash);

        let entry = store.get(&outpoint).unwrap();
        assert!(matches!(entry.status(), OutputSpendStatus::Mined(hash) if *hash == block_hash));
    }

    #[test]
    fn test_coin_store_spendable_coins() {
        let mut store = SpCoinStore::new();

        // Insert 2 unspent and 1 spent
        store.insert(test_outpoint(), test_owned_output(10000));
        store.insert(test_outpoint_2(), test_owned_output(20000));
        store.insert(test_outpoint_3(), test_spent_output(30000));

        let state = store.spendable_coins();

        assert_eq!(state.confirmed_coins, 2);
        assert_eq!(state.confirmed_balance, 30000); // 10000 + 20000
        assert_eq!(state.unconfirmed_coins, 0);
        assert_eq!(state.unconfirmed_balance, 0);
        assert_eq!(state.coins.len(), 2);
    }

    #[test]
    fn test_coin_store_balance() {
        let mut store = SpCoinStore::new();

        store.insert(test_outpoint(), test_owned_output(10000));
        store.insert(test_outpoint_2(), test_owned_output(20000));
        store.insert(test_outpoint_3(), test_spent_output(50000)); // Spent, not counted

        assert_eq!(store.balance(), Amount::from_sat(30000));
    }

    #[test]
    fn test_coin_store_all_outpoints() {
        let mut store = SpCoinStore::new();
        store.insert(test_outpoint(), test_owned_output(10000));
        store.insert(test_outpoint_2(), test_owned_output(20000));

        let outpoints = store.all_outpoints();
        assert_eq!(outpoints.len(), 2);
        assert!(outpoints.contains(&test_outpoint()));
        assert!(outpoints.contains(&test_outpoint_2()));
    }

    #[test]
    fn test_coin_store_serde_roundtrip() {
        let mut store = SpCoinStore::new();
        store.insert(test_outpoint(), test_owned_output(10000));
        store.insert(test_outpoint_2(), test_owned_output(20000));

        let json = serde_json::to_string(&store).expect("serialize");
        let loaded: SpCoinStore = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.get(&test_outpoint()).unwrap().amount_sat(), 10000);
        assert_eq!(loaded.get(&test_outpoint_2()).unwrap().amount_sat(), 20000);
    }

    #[test]
    fn test_coin_store_dump_restore() {
        let mut store = SpCoinStore::new();
        store.insert(test_outpoint(), test_owned_output(10000));

        let dumped = store.dump();

        let mut new_store = SpCoinStore::new();
        new_store.restore(dumped).expect("restore");

        assert_eq!(new_store.len(), 1);
        assert_eq!(new_store.get(&test_outpoint()).unwrap().amount_sat(), 10000);
    }

    #[test]
    fn test_coin_store_persistence() {
        use std::env;

        let temp_dir = env::temp_dir().join("bwk-sp-coin-store-test");
        let _ = fs::remove_dir_all(&temp_dir);
        let _ = fs::create_dir_all(&temp_dir);

        let path = temp_dir.join("coins.json");

        // Create and populate store
        let mut store = SpCoinStore::with_path(path.clone()).enable_persist(true);
        store.insert(test_outpoint(), test_owned_output(10000));
        store.insert(test_outpoint_2(), test_owned_output(20000));
        store.persist();

        // Load from file
        let loaded = SpCoinStore::from_file(path).expect("load");

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.get(&test_outpoint()).unwrap().amount_sat(), 10000);
        assert_eq!(loaded.get(&test_outpoint_2()).unwrap().amount_sat(), 20000);

        // Clean up
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_coin_store_persist_disabled() {
        use std::env;

        let temp_dir = env::temp_dir().join("bwk-sp-coin-store-no-persist-test");
        let _ = fs::remove_dir_all(&temp_dir);
        let _ = fs::create_dir_all(&temp_dir);

        let path = temp_dir.join("coins.json");

        // Create store with persist disabled
        let mut store = SpCoinStore::with_path(path.clone()).enable_persist(false);
        store.insert(test_outpoint(), test_owned_output(10000));
        store.persist();

        // File should not exist
        assert!(!path.exists());

        // Clean up
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_coin_store_get_mut() {
        let mut store = SpCoinStore::new();
        let outpoint = test_outpoint();
        store.insert(outpoint, test_owned_output(10000));

        // Modify through get_mut
        if let Some(entry) = store.get_mut(&outpoint) {
            entry.owned_output_mut().spend_status = OutputSpendStatus::Spent([1u8; 32]);
        }

        // Verify modification
        let entry = store.get(&outpoint).unwrap();
        assert!(!entry.is_spendable());
    }

    #[test]
    fn test_coin_state_default() {
        let state = CoinState::default();

        assert!(state.coins.is_empty());
        assert_eq!(state.confirmed_coins, 0);
        assert_eq!(state.confirmed_balance, 0);
        assert_eq!(state.unconfirmed_coins, 0);
        assert_eq!(state.unconfirmed_balance, 0);
    }

    #[test]
    fn test_coin_store_from_file_not_found() {
        let result = SpCoinStore::from_file(PathBuf::from("/nonexistent/path/coins.json"));
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(matches!(e, CoinStoreError::Io(_)));
        }
    }

    #[test]
    fn test_coin_store_coins_reference() {
        let mut store = SpCoinStore::new();
        store.insert(test_outpoint(), test_owned_output(10000));

        let coins = store.coins();
        assert_eq!(coins.len(), 1);
        assert!(coins.contains_key(&test_outpoint()));
    }

    #[test]
    fn test_coin_store_error_display() {
        // Test Io error variant
        let err = CoinStoreError::Io("permission denied".to_string());
        let msg = err.to_string();
        assert!(msg.contains("io error"));
        assert!(msg.contains("permission denied"));

        // Test Parse error variant
        let err = CoinStoreError::Parse("invalid json structure".to_string());
        let msg = err.to_string();
        assert!(msg.contains("parse error"));
        assert!(msg.contains("invalid json structure"));
    }
}
