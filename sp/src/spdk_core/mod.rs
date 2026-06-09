#![allow(clippy::module_inception)]

pub mod client;
pub mod constants;
pub mod error;
pub mod scan_profile;
pub mod types;

// Re-export core functionality
pub use client::*;
pub use constants::*;
pub use error::Error;
pub use types::*;
// Re-export commonly used external types
pub use crate::silentpayments;
pub use bdk_coin_select::FeeRate;
#[cfg(feature = "mnemonic")]
pub use bip39;
pub use bitcoin;
