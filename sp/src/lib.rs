// bwk-sp: Silent Payment Account Wrapper
//
// A modular Rust library for building Bitcoin wallets with Silent Payments.
// Experimental, not production-ready.

mod account;
mod coin_store;
mod config;
mod label_store;
pub mod profile;
pub mod recipient;
mod scan_state;
mod tx_store;

pub use profile::{SpRamProfile, SpStorageProfile};

// Internal types
pub use account::{
    backend_block_height, backend_info, Account, AccountError, Payment, PaymentType, ScanMode,
};
pub use backend_blindbit_native_non_async::InfoResponse;
pub use bwk;
pub use bwk::{Notification, SpNotification};
pub use coin_store::{CoinState, SpCoinEntry, SpCoinStore};
pub use config::{Config, ConfigError, SubAccountConfig};
pub use label_store::{LabelKey, LabelStoreError, SpLabelStore};
pub use scan_state::{ScanState, ScanStateError};
pub use tx_store::{SpTxEntry, SpTxStore, TxDirection};

// Re-export external types for convenience
pub use bitcoin;
pub use bwk_sign;
pub use bwk_tx;
pub use bwk_tx::{
    Fees, FinalizationContext, PsbtOutputInfo, RecipientProvider, SpPartialSecretProvider,
};
pub use recipient::{SpRecipient, SpRecipientAddress, TxBuilderSpExt};
pub use silentpayments;
pub use spdk_core;
