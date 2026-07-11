//! A rust implementation of BIP352: Silent Payments. This library
//! can be used to add silent payment support to wallets.
//!
//! This library is split up in two parts: sending and receiving.
//!
//! Source: adapted from SPDK's vendored `silentpayments` implementation,
//! originally imported from cygnet3/rust-silentpayments. See `sp/NOTICE`.
#![allow(non_snake_case)]

pub use secp256k1;

pub mod error;

pub mod receiving;
pub mod sending;
pub mod utils;
