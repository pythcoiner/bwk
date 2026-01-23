// bwk-sp: Silent Payment Account Wrapper
//
// A modular Rust library for building Bitcoin wallets with Silent Payments.
// Experimental, not production-ready.

mod account;
mod coin_store;
mod config;
mod label_store;
mod scan_state;
mod tx_store;

// Internal types
pub use account::{Account, AccountError, Notification, Payment, PaymentType};
pub use coin_store::{CoinState, CoinStoreError, SpCoinEntry, SpCoinStore};
pub use config::{Config, ConfigError};
pub use label_store::{LabelKey, LabelStoreError, SpLabelStore};
pub use scan_state::{ScanState, ScanStateError};
pub use tx_store::{SpTxEntry, SpTxStore, TxDirection, TxStoreError};

// Re-export external types for convenience
pub use bitcoin;
pub use silentpayments::SilentPaymentAddress;
pub use spdk_core::{OutputSpendStatus, OwnedOutput};
