#![allow(clippy::module_inception)]
mod backend;
mod client;
pub mod error;

mod thread_pool;

pub use backend::{
    agent, block_height, forward_tx, get_block_data_for_range, info, spent_filter, spent_index,
    utxos, BlockDataObserver, HeightObserver,
};

pub use client::structs::InfoResponse;

// Re-export core types and traits (avoiding module name conflicts)
pub use crate::spdk_core::{
    BlockData,
    // Re-exported external types
    FeeRate,
    FilterData,
    OutputSpendStatus,
    OwnedOutput,
    Recipient,
    RecipientAddress,
    SilentPaymentUnsignedTransaction,
    SpClient,
    SpendKey,
    SpentIndexData,
    UtxoData,
    // Constants
    DATA_CARRIER_SIZE,
    DUST_THRESHOLD,
    NUMS,
    PSBT_SP_ADDRESS_KEY,
    PSBT_SP_PREFIX,
    PSBT_SP_SUBTYPE,
    PSBT_SP_TWEAK_KEY,
};
