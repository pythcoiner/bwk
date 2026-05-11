// Vendored verbatim from upstream ledger_bitcoin_client; keep clippy
// stance aligned with the upstream crate rather than the bwk workspace.
#![allow(clippy::uninlined_format_args, clippy::while_let_loop)]

#[cfg(feature = "paranoid_client")]
mod bip327;
mod command;
mod interpreter;
mod merkle;
mod protocol;

pub mod apdu;
pub mod client;
pub mod error;
pub mod psbt;
pub mod wallet;

pub use client::{BitcoinClient, Transport};
pub use protocol::{MusigPartialSignature, MusigPubNonce, SignPsbtYieldedObject};
pub use psbt::{PartialSignature, PartialSignatureError};
pub use wallet::{WalletPolicy, WalletPubKey};
