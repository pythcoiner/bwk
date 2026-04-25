//! Coin store for Silent Payment outputs.
//!
//! The `SpCoinStore` manages a collection of `SpCoinEntry` items, which wrap
//! `OwnedOutput` from spdk-core with additional metadata. This provides a
//! similar interface to bwk's CoinStore but for silent payment outputs.

use std::{
    collections::{BTreeMap, HashSet},
    str::FromStr,
    sync::Arc,
};

use bitcoin::{Amount, OutPoint, ScriptBuf};
use bwk::persist::{NoopBackend, PersistError, PersistenceBackend, RamStore, Store};
use serde::{Deserialize, Serialize};
use silentpayments::receiving::Label;
use spdk_core::{OutputSpendStatus, OwnedOutput};

use crate::profile::{DefaultBackend, SpRamProfile, SpStorageProfile};

/// Logical store name used by [`PersistenceBackend`] implementations for
/// the silent-payment coin store. Re-export of the canonical constant
/// in [`bwk::persist`] so callers keep a single source of truth.
pub const STORE_KEY: &str = bwk::persist::COINS_STORE_KEY;

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

// SpCoinStore

pub fn encode_outpoint(k: &OutPoint) -> String {
    k.to_string()
}
pub fn decode_outpoint(s: &str) -> Result<OutPoint, PersistError> {
    OutPoint::from_str(s).map_err(|e| PersistError::Serde(format!("bad OutPoint pk {s:?}: {e}")))
}
pub fn encode_coin(v: &SpCoinEntry) -> Result<Vec<u8>, PersistError> {
    serde_json::to_vec(v).map_err(|e| PersistError::Serde(format!("encode SpCoinEntry: {e}")))
}
pub fn decode_coin(bytes: &[u8]) -> Result<SpCoinEntry, PersistError> {
    serde_json::from_slice(bytes)
        .map_err(|e| PersistError::Serde(format!("decode SpCoinEntry: {e}")))
}

/// Storage for silent payment coins. Generic over any
/// `S: Store<Key = OutPoint, Value = SpCoinEntry>`.
pub struct SpCoinStore<P: SpStorageProfile = SpRamProfile<DefaultBackend>> {
    store: P::CoinStore,
}

impl Default for SpCoinStore<SpRamProfile<DefaultBackend>> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: SpStorageProfile> std::fmt::Debug for SpCoinStore<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpCoinStore")
            .field("len", &self.store.len().ok())
            .finish()
    }
}

impl SpCoinStore<SpRamProfile<DefaultBackend>> {
    pub fn new() -> Self {
        let backend: Arc<dyn PersistenceBackend> = Arc::new(NoopBackend);
        Self {
            store: RamStore::empty(backend, STORE_KEY, encode_outpoint, encode_coin),
        }
    }

    pub fn with_backend(backend: Arc<dyn PersistenceBackend>, store_key: &'static str) -> Self {
        Self {
            store: RamStore::empty(backend, store_key, encode_outpoint, encode_coin),
        }
    }

    pub fn load_from_backend(
        backend: Arc<dyn PersistenceBackend>,
        store_key: &'static str,
    ) -> Self {
        let store = RamStore::open(
            backend.clone(),
            store_key,
            encode_outpoint,
            decode_outpoint,
            encode_coin,
            decode_coin,
        )
        .unwrap_or_else(|e| {
            log::error!("SpCoinStore::load_from_backend: {e}");
            let noop: Arc<dyn PersistenceBackend> = Arc::new(NoopBackend);
            RamStore::empty(noop, store_key, encode_outpoint, encode_coin)
        });
        Self { store }
    }
}

impl<P: SpStorageProfile> SpCoinStore<P> {
    /// Wrap any `Store<Key = OutPoint, Value = SpCoinEntry>` impl.
    pub fn from_store(store: P::CoinStore) -> Self {
        Self { store }
    }

    /// Get an owned copy of a coin entry by outpoint.
    pub fn get(&self, outpoint: &OutPoint) -> Option<SpCoinEntry> {
        self.store.get(outpoint).ok().flatten()
    }

    /// Insert a new coin entry from an outpoint and owned output.
    pub fn insert(&mut self, outpoint: OutPoint, output: OwnedOutput) {
        let entry = SpCoinEntry::new(outpoint, output);
        if let Err(e) = self.store.insert(outpoint, entry) {
            log::error!("SpCoinStore::insert: {e}");
        }
    }

    pub fn insert_batch(&mut self, outputs: BTreeMap<OutPoint, OwnedOutput>) {
        for (outpoint, output) in outputs {
            self.insert(outpoint, output);
        }
    }

    pub fn remove(&mut self, outpoint: &OutPoint) -> Option<SpCoinEntry> {
        self.store.remove(outpoint).unwrap_or_default()
    }

    pub fn mark_spent(&mut self, outpoint: &OutPoint, spending_txid: [u8; 32]) {
        if let Err(e) = self.store.modify(outpoint, |entry| {
            entry.output.spend_status = OutputSpendStatus::Spent(spending_txid);
        }) {
            log::error!("SpCoinStore::mark_spent: {e}");
        }
    }

    pub fn mark_mined(&mut self, outpoint: &OutPoint, block_hash: [u8; 32]) {
        if let Err(e) = self.store.modify(outpoint, |entry| {
            entry.output.spend_status = OutputSpendStatus::Mined(block_hash);
        }) {
            log::error!("SpCoinStore::mark_mined: {e}");
        }
    }

    /// Returns a snapshot of every coin as a fresh `BTreeMap`.
    pub fn coins(&self) -> BTreeMap<OutPoint, SpCoinEntry> {
        self.store
            .iter()
            .ok()
            .map(|it| it.collect())
            .unwrap_or_default()
    }

    /// Returns a CoinState with only spendable (unspent) coins.
    pub fn spendable_coins(&self) -> CoinState {
        let mut state = CoinState::default();
        if let Ok(iter) = self.store.iter() {
            for (outpoint, entry) in iter {
                if entry.is_spendable() {
                    state.confirmed_coins += 1;
                    state.confirmed_balance += entry.amount_sat();
                    state.coins.insert(outpoint, entry);
                }
            }
        }
        state
    }

    pub fn all_outpoints(&self) -> HashSet<OutPoint> {
        self.store
            .keys()
            .ok()
            .map(|it| it.collect())
            .unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.store.len().unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn balance(&self) -> Amount {
        let sats: u64 = self
            .store
            .values()
            .ok()
            .map(|it| {
                it.filter(|entry| entry.is_spendable())
                    .map(|entry| entry.amount_sat())
                    .sum()
            })
            .unwrap_or(0);
        Amount::from_sat(sats)
    }

    pub fn persist(&mut self) {
        if let Err(e) = self.store.flush() {
            log::error!("SpCoinStore::persist() flush: {e}");
        }
    }
}

// SpCoinSource (CoinSource for TxBuilder)

use std::sync::Mutex;

use bwk_tx::coin::{CoinSpendInfo, CoinStatus};
use bwk_tx::tx_builder::CoinSource;
use bwk_tx::Coin;

use bitcoin::bip32::{Fingerprint, Xpriv};
use bitcoin::secp256k1::{All, Secp256k1};

/// Taproot keyspend satisfaction weight in WU (1 Schnorr signature = 66 WU)
const TR_KEYSPEND_SATISFACTION_WEIGHT: u64 = 66;

/// Implements [`CoinSource`] for the SP coin store, providing spendable coins
/// to [`TxBuilder`](bwk_tx::TxBuilder). Generic over any `SpCoinStore<S>`.
pub struct SpCoinSource<P: SpStorageProfile = SpRamProfile<DefaultBackend>>(
    Arc<Mutex<SpCoinStore<P>>>,
);

impl<P: SpStorageProfile> SpCoinSource<P> {
    pub fn new(store: Arc<Mutex<SpCoinStore<P>>>) -> Self {
        Self(store)
    }
}

impl<P: SpStorageProfile + Send + Sync + 'static> CoinSource for SpCoinSource<P> {
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
pub struct MergedCoinSource<P: SpStorageProfile = SpRamProfile<DefaultBackend>> {
    sp_source: SpCoinSource<P>,
    bip32_sources: Vec<Box<dyn CoinSource>>,
}

impl<P: SpStorageProfile> MergedCoinSource<P> {
    pub fn new(sp_source: SpCoinSource<P>, bip32_sources: Vec<Box<dyn CoinSource>>) -> Self {
        Self {
            sp_source,
            bip32_sources,
        }
    }
}

impl<P: SpStorageProfile + Send + Sync + 'static> CoinSource for MergedCoinSource<P> {
    fn spendable_coins(&self) -> Vec<Coin> {
        let mut coins = self.sp_source.spendable_coins();
        for source in &self.bip32_sources {
            coins.extend(source.spendable_coins());
        }
        coins
    }
}

// KeyedBip32Source (enriches BIP32 coins with secret keys for SP partial secret)

/// A [`CoinSource`] wrapper that populates the ephemeral `secret_key` field on
/// BIP32 coins so that [`SpPartialSecretProvider`] can compute the partial
/// secret when sending to SP addresses with mixed inputs.
pub struct KeyedBip32Source {
    inner: Box<dyn CoinSource>,
    xprivs: BTreeMap<Fingerprint, Xpriv>,
    secp: Secp256k1<All>,
}

impl KeyedBip32Source {
    pub fn new(inner: Box<dyn CoinSource>, xprivs: BTreeMap<Fingerprint, Xpriv>) -> Self {
        Self {
            inner,
            xprivs,
            secp: Secp256k1::new(),
        }
    }

    /// Derive the secret key for a BIP32 coin using its PSBT input derivation info.
    fn populate_secret_key(&self, coin: &mut Coin) {
        if !coin.is_bip32() {
            return;
        }

        let psbt_input = match coin.to_psbt_input() {
            Ok(inp) => inp,
            Err(_) => return,
        };

        // Try segwit bip32_derivation first, then taproot tap_key_origins
        let derived_key = if !psbt_input.bip32_derivation.is_empty() {
            psbt_input.bip32_derivation.values().find_map(|(fg, path)| {
                let xpriv = self.xprivs.get(fg)?;
                xpriv
                    .derive_priv(&self.secp, path)
                    .ok()
                    .map(|k| k.private_key)
            })
        } else if !psbt_input.tap_key_origins.is_empty() {
            psbt_input
                .tap_key_origins
                .values()
                .find_map(|(_, (fg, path))| {
                    let xpriv = self.xprivs.get(fg)?;
                    xpriv
                        .derive_priv(&self.secp, path)
                        .ok()
                        .map(|k| k.private_key)
                })
        } else {
            None
        };

        if let (Some(sk), CoinSpendInfo::Bip32 { secret_key, .. }) =
            (derived_key, &mut coin.spend_info)
        {
            *secret_key = Some(sk);
        }
    }
}

impl CoinSource for KeyedBip32Source {
    fn spendable_coins(&self) -> Vec<Coin> {
        let mut coins = self.inner.spendable_coins();
        for coin in &mut coins {
            self.populate_secret_key(coin);
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
    use bwk::persist::JsonBackend;
    use std::fs;

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
    fn test_coin_store_persistence() {
        use std::env;

        let temp_dir = env::temp_dir().join("bwk-sp-coin-store-test");
        let _ = fs::remove_dir_all(&temp_dir);
        let _ = fs::create_dir_all(&temp_dir);

        // Scoped so `store` drops at the closing brace, releasing
        // the DirLock before the reopener tries to acquire it.
        {
            let backend = JsonBackend::open(temp_dir.clone()).unwrap();
            let coins_path = backend.path_for(STORE_KEY);
            let mut store = SpCoinStore::with_backend(Arc::new(backend), STORE_KEY);
            store.insert(test_outpoint(), test_owned_output(10000));
            store.insert(test_outpoint_2(), test_owned_output(20000));
            store.persist();

            assert!(coins_path.exists());
        }

        // Load from dir
        let backend = Arc::new(JsonBackend::open(temp_dir.clone()).unwrap());
        let loaded = SpCoinStore::load_from_backend(backend, STORE_KEY);

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

        // Compute the would-be on-disk path via the backend, then drop
        // the backend before exercising the no-persist path.
        let backend = JsonBackend::open(temp_dir.clone()).unwrap();
        let coins_path = backend.path_for(STORE_KEY);
        drop(backend);

        // Create store with persist disabled (NoopBackend via SpCoinStore::new)
        let mut store = SpCoinStore::new();
        store.insert(test_outpoint(), test_owned_output(10000));
        store.persist();

        // File should not exist
        assert!(!coins_path.exists());

        // Clean up
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_coin_store_mark_spent_api() {
        let mut store = SpCoinStore::new();
        let outpoint = test_outpoint();
        store.insert(outpoint, test_owned_output(10000));

        // Modify through mark_spent (the closure-based `modify` that
        // replaces the old `get_mut`).
        store.mark_spent(&outpoint, [1u8; 32]);

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
    fn test_coin_store_coins_reference() {
        let mut store = SpCoinStore::new();
        store.insert(test_outpoint(), test_owned_output(10000));

        let coins = store.coins();
        assert_eq!(coins.len(), 1);
        assert!(coins.contains_key(&test_outpoint()));
    }
}
