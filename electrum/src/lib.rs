#![allow(dead_code)]
pub mod client;
pub mod electrum;
pub mod raw_client;
pub mod validation;

pub use client::Client;
pub use validation::{PsbtValidationReport, ReusedOutput, SpentInput, ValidationError};
