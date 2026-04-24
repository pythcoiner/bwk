//! Persistence backend abstraction for bwk.
//!
//! Stores (tx, label, coin, scan state, etc.) in `bwk` and `bwk-sp` do not
//! write files themselves — they hand their serialized bytes to a
//! [`PersistenceBackend`]. Concrete backends land in subsequent
//! commits; this module defines the trait surface plus the shared
//! error type, stamped [`DB_VERSION`], and set of logical store names
//! the ecosystem uses.
//!
//! # The DB is always a pure key-value store
//!
//! On-disk layouts are treated as opaque key-value stores. Each store
//! has rows keyed by a primary-key string (`key`) with bytes as the
//! value holding serde-encoded data. There are **no typed columns, no
//! relational structure, no schema migrations** — ever.
//!
//! The reason this is safe: bwk holds the whole account state in RAM at
//! runtime. The DB is only a persistence medium, never a query layer;
//! lookups happen in-memory, so the DB gains nothing from typed columns
//! or indexes. Format evolution is handled entirely in Rust code:
//!
//! - **Reads** support multiple parse paths (backward-compatible
//!   deserialization of older byte formats).
//! - **Writes** always emit the latest format.
//!
//! No `ALTER TABLE`, no versioned SQL migrations. If a write-format
//! change would break older binaries, bump [`DB_VERSION`]; any DB whose
//! recorded version is greater than the running binary's [`DB_VERSION`]
//! is refused at open time with a [`PersistError::DbVersionTooNew`].
//!
//! Backends MUST:
//! - Record the current [`DB_VERSION`] the first time they initialise
//!   a fresh account directory / sqlite file.
//! - On subsequent opens, read the recorded version and refuse to
//!   proceed if it's greater than [`DB_VERSION`] in the running binary.

pub mod backend;
pub mod storage;

pub use backend::{JsonBackend, NoopBackend, PersistenceBackend};
pub use storage::Store;

/// Monotonic integer stamped into every persistence medium by the
/// running binary.
///
/// Bump this only when introducing a write-format that older binaries
/// cannot parse. A DB whose recorded version is greater than the
/// running binary's `DB_VERSION` is refused at open time.
pub const DB_VERSION: u32 = 1;

/// Logical store name used for account-scoped singleton fields.
///
/// Every persistence medium has an `account` store that holds the
/// DB version row plus the flat scalar fields of whatever singleton
/// struct the account type owns (`Tip` for bwk accounts,
/// `ScanState` for bwk-sp accounts).
pub const ACCOUNT_STORE_KEY: &str = "account";

/// Logical store name for the bwk (Electrum) transaction store.
pub const TRANSACTIONS_STORE_KEY: &str = "transactions";

/// Logical store name for user-facing labels (used by both bwk and bwk-sp).
pub const LABELS_STORE_KEY: &str = "labels";

/// Logical store name for the bwk (Electrum) per-address subscription-status map.
pub const STATUSES_STORE_KEY: &str = "statuses";

/// Logical store name for the bwk-sp (silent-payment) coin store.
pub const COINS_STORE_KEY: &str = "coins";

/// Logical store name for the bwk-sp (silent-payment) transaction store.
pub const TXS_STORE_KEY: &str = "txs";

/// Logical store name for hot-signer material (BIP32 mnemonics +
/// per-signer descriptor sets), keyed by signer fingerprint.
///
/// Under [`PersistenceKind::Sqlite`] the `Account` constructor opens
/// this store against [`NoopBackend`] so secrets never reach the
/// SQLite DB; under JSON the store is a sibling of `transactions.json`,
/// `labels.json`, etc.
pub const SIGNERS_STORE_KEY: &str = "signers";

/// The complete set of logical store names the bwk ecosystem uses.
///
/// [`PersistenceBackend::validate_store_name`] consults this list to
/// reject typos and arbitrary caller-supplied strings (which would
/// otherwise become a SQL-injection surface for a SQLite backend and
/// a path-traversal surface for a JSON backend).
pub const KNOWN_STORES: &[&str] = &[
    ACCOUNT_STORE_KEY,
    TRANSACTIONS_STORE_KEY,
    LABELS_STORE_KEY,
    STATUSES_STORE_KEY,
    COINS_STORE_KEY,
    TXS_STORE_KEY,
    SIGNERS_STORE_KEY,
];

/// Row key in the `account` store holding the stamped [`DB_VERSION`].
pub const VERSION_ROW_KEY: &str = "version";

/// Errors returned by [`PersistenceBackend`] implementations.
#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    #[error("io error: {0}")]
    Io(String),
    #[error("serialization error: {0}")]
    Serde(String),
    #[error("sqlite error: {0}")]
    Sqlite(String),
    #[error("sqlite support disabled: rebuild with feature `sqlite`")]
    SqliteDisabled,
    /// Caller passed a `store` name that isn't in [`KNOWN_STORES`].
    #[error("unknown store {found:?} (expected one of {:?})", KNOWN_STORES)]
    UnknownStore { found: String },
    /// The persistence medium was written by a newer binary (recorded
    /// DB version is greater than [`DB_VERSION`]). We refuse to open
    /// for fear of truncating data whose shape we don't understand.
    #[error("persistence file is at version {found} but this binary only supports up to {max_supported}")]
    DbVersionTooNew { found: u32, max_supported: u32 },
}
