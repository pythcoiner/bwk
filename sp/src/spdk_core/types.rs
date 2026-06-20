use bitcoin::{absolute::Height, Amount, BlockHash, ScriptBuf, Txid};

pub struct BlockData {
    pub blkheight: Height,
    pub blkhash: BlockHash,
    // Raw 33-byte compressed tweak points, NOT parsed to `PublicKey` here: point
    // validation is crypto and is deferred to the bounded compute threads (see
    // `process_block_outputs`), so the many fetch workers stay pure I/O and never
    // oversubscribe the cores.
    pub tweaks: Vec<[u8; 33]>,
    pub new_utxo_filter: FilterData,
}

#[derive(Clone)]
pub struct UtxoData {
    pub txid: Txid,
    pub vout: u32,
    pub value: Amount,
    pub scriptpubkey: ScriptBuf,
    pub spent: bool,
}

pub struct SpentIndexData {
    pub data: Vec<Vec<u8>>,
}

#[derive(Clone)]
pub struct FilterData {
    pub block_hash: BlockHash,
    pub data: Vec<u8>,
}
