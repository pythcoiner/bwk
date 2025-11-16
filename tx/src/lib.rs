pub mod coin;
pub mod coin_selection;
pub mod descr_fingerprint;
pub mod error;
pub mod recipient;
pub mod transaction;
pub mod tx_builder;

pub use coin::{Coin, CoinStatus};
pub use descr_fingerprint::DescrFingerprint;
pub use error::Error;
pub use recipient::Recipient;
pub use transaction::{Amount, Drain, FeeResult, Fees, TransactionResult, TxTemplate, Warning};

const DUST_AMOUNT: u64 = 5_000;
