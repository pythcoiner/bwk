//! Storage profile for `bwk::Account`.
//!
//! A [`StorageProfile`] names the four typed stores an
//! `Account<P>` needs, binding each to a concrete [`Store`] impl.
//! The choice of profile is made at launch time (typically by
//! matching on [`PersistenceKind`](bwk_persist::PersistenceKind))
//! and stays fixed for the lifetime of the account.
//!
//! Today's only shipped profile is [`RamProfile<B>`]: every store is a
//! RAM-cached, write-back [`RamStore<B, K, V>`] over some
//! [`PersistenceBackend`]. Future profiles (DB-through, online, LRU, …)
//! plug in without touching `Account<P>` or any caller that works
//! through the `Store` trait.
//!
//! The store traits land here first; per-store migrations in the
//! next few commits flip `TxStore`/`LabelStore`/address_store over
//! to `RamStore`-backed `Store` shapes, and the `RamStores` bundle
//! and `open_ram_stores` factory show up once all the stores
//! expose their encode/decode helpers.

use std::{marker::PhantomData, sync::Arc};

use bwk_persist::{PersistError, PersistenceBackend, RamStore, Store};
use bwk_sign::JsonSigner;
use miniscript::bitcoin::{bip32, ScriptBuf, Txid};

use crate::{config::Tip, label_store::LabelKey, tx_store::TxEntry};

/// Convenient alias for the runtime-dispatched backend variant.
pub type DefaultBackend = Arc<dyn PersistenceBackend>;

/// The four typed stores a `bwk::Account<P>` needs.
///
/// Implementations pick each associated type; the standard
/// [`RamProfile<B>`] binds them all to [`RamStore<B, K, V>`].
pub trait StorageProfile: 'static + Send + Sync {
    type TxStore: Store<Key = Txid, Value = TxEntry> + Send + Sync + 'static;
    type LabelStore: Store<Key = LabelKey, Value = String> + Send + Sync + 'static;
    type StatusesStore: Store<Key = ScriptBuf, Value = (Option<String>, u32, u32)>
        + Send
        + Sync
        + 'static;
    type AccountStore: Store<Key = String, Value = Vec<u8>> + Send + Sync + 'static;
    type SignerStore: Store<Key = bip32::Fingerprint, Value = JsonSigner> + Send + Sync + 'static;
}

/// The RAM-cached, write-back profile over any
/// [`PersistenceBackend`].
///
/// The only concrete `StorageProfile` shipped today. Parameterised
/// over the backend type so static-dispatch callers can monomorphise
/// on `SqliteBackend` / `JsonBackend` / `NoopBackend`, and
/// runtime-dispatch callers on `Arc<dyn PersistenceBackend>` via the
/// [`PersistenceBackend` blanket impl for
/// `Arc<T>`](bwk_persist#impl-PersistenceBackend-for-Arc%3CT%3E).
pub struct RamProfile<B: PersistenceBackend = DefaultBackend>(PhantomData<B>);

impl<B: PersistenceBackend> std::fmt::Debug for RamProfile<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RamProfile")
    }
}

impl<B: PersistenceBackend + Clone + 'static> StorageProfile for RamProfile<B> {
    type TxStore = RamStore<B, Txid, TxEntry>;
    type LabelStore = RamStore<B, LabelKey, String>;
    type StatusesStore = RamStore<B, ScriptBuf, (Option<String>, u32, u32)>;
    type AccountStore = RamStore<B, String, Vec<u8>>;
    type SignerStore = RamStore<B, bip32::Fingerprint, JsonSigner>;
}

// ----- statuses encode/decode helpers used by the StatusesStore slot -----
// Wired up by `open_ram_stores` in a follow-up commit.

#[allow(dead_code)]
pub(crate) fn encode_status_key(k: &ScriptBuf) -> String {
    serde_json::to_string(k).expect("ScriptBuf serialises as JSON")
}
#[allow(dead_code)]
pub(crate) fn decode_status_key(s: &str) -> Result<ScriptBuf, PersistError> {
    serde_json::from_str(s).map_err(|e| PersistError::Serde(format!("bad ScriptBuf pk: {e}")))
}
#[allow(dead_code)]
pub(crate) fn encode_status_value(v: &(Option<String>, u32, u32)) -> Result<Vec<u8>, PersistError> {
    serde_json::to_vec(v).map_err(|e| PersistError::Serde(format!("encode status: {e}")))
}
#[allow(dead_code)]
pub(crate) fn decode_status_value(
    bytes: &[u8],
) -> Result<(Option<String>, u32, u32), PersistError> {
    serde_json::from_slice(bytes).map_err(|e| PersistError::Serde(format!("decode status: {e}")))
}

// ----- account-store (Tip) encode/decode helpers -----

#[allow(clippy::ptr_arg)]
pub fn encode_account_key(k: &String) -> String {
    k.clone()
}
pub fn decode_account_key(s: &str) -> Result<String, PersistError> {
    Ok(s.to_string())
}
#[allow(clippy::ptr_arg)]
pub fn encode_account_value(v: &Vec<u8>) -> Result<Vec<u8>, PersistError> {
    Ok(v.clone())
}
pub fn decode_account_value(bytes: &[u8]) -> Result<Vec<u8>, PersistError> {
    Ok(bytes.to_vec())
}

/// Row keys used by [`Tip::persist`] / [`Tip::from_account_store`]
/// inside the `account` store.
pub const TIP_RECEIVE_ROW: &str = "receive_index";
pub const TIP_CHANGE_ROW: &str = "change_index";

impl Tip {
    /// Persist `receive` / `change` into `store` (typically a
    /// [`StorageProfile::AccountStore`]).
    pub fn persist(
        store: &mut impl Store<Key = String, Value = Vec<u8>>,
        receive: u32,
        change: u32,
    ) {
        // Bytes cross the PersistenceBackend boundary, so they must be
        // JSON-parseable for JsonBackend to accept them and keep writing
        // human-readable {store}.json files.
        let enc = |label: &str, v: u32| match serde_json::to_vec(&v) {
            Ok(b) => Some(b),
            Err(e) => {
                log::error!("Tip::persist encode {label}: {e}");
                None
            }
        };
        let Some(recv) = enc(TIP_RECEIVE_ROW, receive) else {
            return;
        };
        let Some(chg) = enc(TIP_CHANGE_ROW, change) else {
            return;
        };
        if let Err(e) = store.insert(TIP_RECEIVE_ROW.to_string(), recv) {
            log::error!("Tip::persist receive: {e}");
            return;
        }
        if let Err(e) = store.insert(TIP_CHANGE_ROW.to_string(), chg) {
            log::error!("Tip::persist change: {e}");
        }
    }

    /// Reconstruct [`Tip`] from the `account` store.
    pub fn from_account_store(store: &impl Store<Key = String, Value = Vec<u8>>) -> Tip {
        let read = |row: &str| -> u32 {
            match store.get(&row.to_string()) {
                Ok(Some(bytes)) => serde_json::from_slice::<u32>(&bytes).unwrap_or_default(),
                _ => 0,
            }
        };
        Tip {
            receive: read(TIP_RECEIVE_ROW),
            change: read(TIP_CHANGE_ROW),
        }
    }
}
