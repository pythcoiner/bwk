#![allow(clippy::module_inception)]

pub mod account;
pub mod backend;
pub mod client;
pub mod constants;
pub mod error;
pub mod scan_profile;
pub mod scanner;
pub mod types;
pub mod updater;

// Re-export core functionality
pub use backend::{BlockDataIterator, ChainBackend};
pub use client::*;
pub use constants::*;
pub use error::Error;
pub use scanner::SpScanner;
pub use types::*;
pub use updater::Updater;
// Re-export commonly used external types
pub use crate::silentpayments;
pub use bdk_coin_select::FeeRate;
#[cfg(feature = "mnemonic")]
pub use bip39;
pub use bitcoin;
