//! QR generation, scanning, and signing-flow message transport.
//!
//! The crate keeps QR primitives internal. Callers use [`Encoder`] and
//! [`Decoder`] for plain text QR codes or, with the `protocol` feature, the
//! signing-flow messages in [`protocol`].

mod config;
mod error;
mod image;

#[cfg(feature = "protocol")]
pub mod bbqr;
#[cfg(feature = "scan")]
mod decoder;
#[cfg(feature = "gen")]
mod encoder;
#[cfg(feature = "gen")]
mod gen;
#[cfg(feature = "scan")]
mod scan;

#[cfg(feature = "protocol")]
pub use bwk_qr_protocol as protocol;

pub use config::Config;
pub use error::Error;
pub use image::Image;

#[cfg(feature = "scan")]
pub use decoder::{Decoded, Decoder, Progress};
#[cfg(feature = "gen")]
pub use encoder::Encoder;
