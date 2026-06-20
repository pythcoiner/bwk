//! A rust implementation of BIP352: Silent Payments. This library
//! can be used to add silent payment support to wallets.
//!
//! This library is split up in two parts: sending and receiving.
#![allow(non_snake_case)]

pub use secp256k1;

pub mod error;

pub mod receiving;
pub mod sending;
pub mod utils;
