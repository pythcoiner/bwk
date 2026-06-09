// bwk-sp: Silent Payment Account Wrapper
//
// A modular Rust library for building Bitcoin wallets with Silent Payments.
// Experimental, not production-ready.

// Account is concrete on BlindbitBackend (and seeded from a mnemonic), so it
// only exists with the blindbit transport.
#[cfg(feature = "blindbit")]
mod account;
#[cfg(feature = "blindbit")]
pub mod blindbit;
mod coin_store;
mod config;
pub mod profile;
pub mod recipient;
mod scan_state;
pub mod silentpayments;
pub mod spdk_core;
mod tx_store;
mod unified;

pub use profile::{SpRamProfile, SpStorageProfile};

// Internal types
#[cfg(feature = "blindbit")]
pub use account::{
    backend_block_height, backend_info, sp_coin_entry_to_coin, Account, AccountError, Payment,
    PaymentType, ScanMode,
};
#[cfg(feature = "blindbit")]
pub use blindbit::InfoResponse;
pub use bwk;
pub use bwk::label_store::LabelKey;
pub use bwk::{Notification, SpNotification};
pub use coin_store::{CoinState, SpCoinEntry, SpCoinStore};
pub use config::{Config, ConfigError, SubAccountConfig, CONFIG_FILENAME};
pub use scan_state::ScanState;
pub use tx_store::{SpTxEntry, SpTxStore, TxDirection};
pub use unified::{CoinOrigin, SpendableSummary, UnifiedCoin};

// Re-export external types for convenience
pub use bitcoin;
pub use bwk_sign;
pub use bwk_tx;
pub use bwk_tx::{
    Fees, FinalizationContext, PsbtOutputInfo, RecipientProvider, SpPartialSecretProvider,
};
pub use recipient::{SpRecipient, SpRecipientAddress, SpRecipientError, TxBuilderSpExt};
