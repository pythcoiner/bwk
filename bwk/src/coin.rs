use miniscript::bitcoin::{self, Sequence, TxOut};
use serde::{Deserialize, Serialize};

use crate::account::AddrAccount;

#[derive(Debug, Clone, Serialize, Deserialize)]
/// A struct that represents a coin in a Bitcoin transaction.
///
/// This struct contains information about the transaction output,
/// the outpoint, the sequence number, and the derivation path of the coin.
pub struct Coin {
    pub txout: TxOut,
    pub outpoint: bitcoin::OutPoint,
    pub sequence: Sequence,
    pub coin_path: (AddrAccount, u32),
}
