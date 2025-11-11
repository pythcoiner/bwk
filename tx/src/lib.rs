pub mod coin;
pub mod descr_fingerprint;
pub mod error;
pub mod recipient;
pub mod transaction;

pub use coin::{Coin, CoinStatus};
pub use descr_fingerprint::DescrFingerprint;
pub use error::Error;
pub use recipient::Recipient;
pub use transaction::{
    Amount, Drain, FeeResult, Fees, TransactionResult, TransactionTemplate, Warning,
};

/// The factor that non-witness serialization data is multiplied by during weight calculation.
const WITNESS_SCALE_FACTOR: u64 = 4;

const DUST_AMOUNT: u64 = 5_000;
