//! A rust implementation of BIP352: Silent Payments. This library
//! can be used to add silent payment support to wallets.
//!
//! This library is split up in two parts: sending and receiving.
#![allow(dead_code, non_snake_case)]

pub use secp256k1;

mod error;

pub mod receiving;
pub mod sending;
pub mod utils;

pub use bitcoin_hashes;

pub use crate::silentpayments::error::Error;
pub use utils::common::Network;
pub use utils::common::SilentPaymentAddress;

pub type Result<T> = std::result::Result<T, Error>;
