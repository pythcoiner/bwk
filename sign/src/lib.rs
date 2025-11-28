pub mod error;
pub mod hot_signer;
pub mod signer;
pub mod signing_manager;

pub use error::Error;
pub use hot_signer::{HotSigner, JsonSigner};
pub use signer::{Signer, SignerNotif};
pub use signing_manager::SigningManager;

// re-export
pub use bip39;
pub use bwk_descriptor;
pub use bwk_keys;
pub use bwk_utils;
pub use miniscript;
pub use serde;
pub use serde_json;
