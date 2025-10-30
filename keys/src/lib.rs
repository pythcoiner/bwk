pub mod derivator;
pub mod keys;

pub use derivator::KeyDerivator;
pub use keys::{OXpriv, OXpub};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Fail to create Xpriv from seed.")]
    XPrivFromSeed,
    #[error("Mnemonics words are invalid.")]
    InvalidMnemonicWords,
}
