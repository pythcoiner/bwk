use std::collections::{HashMap, HashSet};

#[cfg(not(all(not(target_arch = "wasm32"), feature = "parallel")))]
use crate::error::Error;
use crate::error::Result;
use bitcoin::{
    absolute::Height, bip158::BlockFilter, Amount, BlockHash, OutPoint, Txid, XOnlyPublicKey,
};
use silentpayments::receiving::Label;

#[cfg(all(not(target_arch = "wasm32"), feature = "parallel"))]
use rayon::prelude::*;

use crate::{BlockData, ChainBackend, FilterData, OwnedOutput, SpClient, Updater, UtxoData};

/// Trait for scanning silent payment blocks
///
/// This trait abstracts the core scanning functionality, allowing consumers
/// to implement it with their own constraints and requirements.
pub trait SpScanner {
    /// Scan a range of blocks for silent payment outputs and inputs
    ///
    /// # Arguments
    /// * `start` - Starting block height (inclusive)
    /// * `end` - Ending block height (inclusive)
    /// * `dust_limit` - Minimum amount to consider (dust outputs are ignored)
    /// * `with_cutthrough` - Whether to use cutthrough optimization
    fn scan_blocks(
        &mut self,
        start: Height,
        end: Height,
        dust_limit: Option<Amount>,
        with_cutthrough: bool,
    ) -> Result<()>;

    /// Process a single block's data
    ///
    /// # Arguments
    /// * `blockdata` - Block data containing tweaks and filters
    ///
    /// # Returns
    /// * `(found_outputs, found_inputs)` - Tuple of found outputs and spent inputs
    fn process_block(
        &mut self,
        blockdata: BlockData,
    ) -> Result<(HashMap<OutPoint, OwnedOutput>, HashSet<OutPoint>)>;

    /// Process block outputs to find owned silent payment outputs
    ///
    /// # Arguments
    /// * `blkheight` - Block height
    /// * `tweaks` - List of tweak public keys
    /// * `new_utxo_filter` - Filter data for new UTXOs
    ///
    /// # Returns
    /// * Map of outpoints to owned outputs
    fn process_block_outputs(
        &self,
        blkheight: Height,
        tweaks: &[bitcoin::secp256k1::PublicKey],
        new_utxo_filter: FilterData,
    ) -> Result<HashMap<OutPoint, OwnedOutput>>;

    /// Process block inputs to find spent outputs
    ///
    /// # Arguments
    /// * `blkheight` - Block height
    /// * `spent_filter` - Filter data for spent outputs
    ///
    /// # Returns
    /// * Set of spent outpoints
    fn process_block_inputs(
        &self,
        blkheight: Height,
        spent_filter: FilterData,
    ) -> Result<HashSet<OutPoint>>;

    /// Get the block data iterator for a range of blocks
    ///
    /// # Arguments
    /// * `range` - Range of block heights
    /// * `dust_limit` - Minimum amount to consider
    /// * `with_cutthrough` - Whether to use cutthrough optimization
    ///
    /// # Returns
    /// * Iterator of block data results
    fn get_block_data_iterator(
        &self,
        range: std::ops::RangeInclusive<u32>,
        dust_limit: Option<Amount>,
        with_cutthrough: bool,
    ) -> crate::BlockDataIterator;

    /// Check if scanning should be interrupted
    ///
    /// # Returns
    /// * `true` if scanning should stop, `false` otherwise
    fn should_interrupt(&self) -> bool;

    /// Save current state to persistent storage
    fn save_state(&mut self) -> Result<()>;

    /// Record found outputs for a block
    ///
    /// # Arguments
    /// * `height` - Block height
    /// * `block_hash` - Block hash
    /// * `outputs` - Found outputs
    fn record_outputs(
        &mut self,
        height: Height,
        block_hash: BlockHash,
        outputs: HashMap<OutPoint, OwnedOutput>,
    ) -> Result<()>;

    /// Record spent inputs for a block
    ///
    /// # Arguments
    /// * `height` - Block height
    /// * `block_hash` - Block hash
    /// * `inputs` - Spent inputs
    fn record_inputs(
        &mut self,
        height: Height,
        block_hash: BlockHash,
        inputs: HashSet<OutPoint>,
    ) -> Result<()>;

    /// Record scan progress
    ///
    /// # Arguments
    /// * `start` - Start height
    /// * `current` - Current height
    /// * `end` - End height
    fn record_progress(&mut self, start: Height, current: Height, end: Height) -> Result<()>;

    /// Get the silent payment client
    fn client(&self) -> &SpClient;

    /// Get the chain backend
    fn backend(&self) -> &dyn ChainBackend;

    /// Get the updater
    fn updater(&mut self) -> &mut dyn Updater;

    // Helper methods with default implementations

    /// Process multiple blocks from an iterator
    ///
    /// This is a default implementation that can be overridden if needed.
    /// Blocks are processed as soon as they form a contiguous sequence from start,
    /// allowing pipelining with parallel fetching. Out-of-order blocks are buffered
    /// until earlier blocks arrive. If any block fetch fails, the error is returned
    /// after processing all successfully fetched blocks.
    fn process_blocks<I>(&mut self, start: Height, end: Height, block_data_iter: I) -> Result<()>
    where
        I: Iterator<Item = Result<BlockData>>,
    {
        process_ordered_block_results(
            self,
            start,
            end,
            block_data_iter,
            |scanner, blockdata, save_to_storage| {
                let blkheight = blockdata.blkheight;
                let blkhash = blockdata.blkhash;

                let (found_outputs, found_inputs) = scanner.process_block(blockdata)?;

                let mut save_to_storage = save_to_storage;
                if !found_outputs.is_empty() {
                    save_to_storage = true;
                    scanner.record_outputs(blkheight, blkhash, found_outputs)?;
                }

                if !found_inputs.is_empty() {
                    save_to_storage = true;
                    scanner.record_inputs(blkheight, blkhash, found_inputs)?;
                }

                scanner.record_progress(start, blkheight, end)?;

                if save_to_storage {
                    scanner.save_state()?;
                }

                Ok(())
            },
        )
    }

    /// Helper method to process blocks sequentially
    ///
    /// # Arguments
    /// * `start` - Start height
    /// * `end` - End height
    /// * `block_data_iter` - Iterator of block data
    /// * `with_cutthrough` - Whether cutthrough is enabled (unused, kept for API compatibility)
    ///
    /// # Returns
    /// * Result indicating success or failure
    fn process_blocks_auto<I>(
        &mut self,
        start: Height,
        end: Height,
        block_data_iter: I,
        _with_cutthrough: bool,
    ) -> Result<()>
    where
        I: Iterator<Item = Result<BlockData>>,
    {
        // Always use sequential processing
        self.process_blocks(start, end, block_data_iter)
    }

    /// Scan UTXOs for a given block and secrets map
    ///
    /// This is a default implementation that can be overridden if needed
    fn scan_utxos(
        &self,
        blkheight: Height,
        secrets_map: HashMap<[u8; 34], bitcoin::secp256k1::PublicKey>,
    ) -> Result<Vec<(Option<Label>, UtxoData, bitcoin::secp256k1::Scalar)>> {
        let utxos = self.backend().utxos(blkheight)?;

        // group utxos by the txid
        let mut txmap: HashMap<Txid, Vec<UtxoData>> = HashMap::new();
        for utxo in utxos {
            txmap.entry(utxo.txid).or_default().push(utxo);
        }

        let client = self.client();

        // Parallel transaction scanning on native platforms with parallel feature
        #[cfg(all(not(target_arch = "wasm32"), feature = "parallel"))]
        let res: Vec<_> = txmap
            .into_par_iter()
            .filter_map(|(_, utxos)| {
                // check if we know the secret to any of the spks
                let secret = utxos.iter().find_map(|utxo| {
                    let spk = utxo.scriptpubkey.as_bytes();
                    secrets_map.get(spk)
                })?;

                let output_keys: Vec<XOnlyPublicKey> = utxos
                    .iter()
                    .filter_map(|x| {
                        if x.scriptpubkey.is_p2tr() {
                            XOnlyPublicKey::from_slice(&x.scriptpubkey.as_bytes()[2..]).ok()
                        } else {
                            None
                        }
                    })
                    .collect();

                // CPU-intensive cryptographic operation
                let ours = client
                    .sp_receiver
                    .scan_transaction(secret, output_keys)
                    .ok()?;

                // Match UTXOs against our keys
                let matched: Vec<_> = utxos
                    .into_iter()
                    .filter(|utxo| utxo.scriptpubkey.is_p2tr() && !utxo.spent)
                    .filter_map(|utxo| {
                        let xonly =
                            XOnlyPublicKey::from_slice(&utxo.scriptpubkey.as_bytes()[2..]).ok()?;
                        ours.iter().find_map(|(label, map)| {
                            map.get(&xonly)
                                .map(|scalar| (label.clone(), utxo.clone(), *scalar))
                        })
                    })
                    .collect();

                if matched.is_empty() {
                    None
                } else {
                    Some(matched)
                }
            })
            .flatten()
            .collect();

        // Sequential fallback (WASM or no parallel feature)
        #[cfg(not(all(not(target_arch = "wasm32"), feature = "parallel")))]
        let res: Vec<_> = {
            let mut result = Vec::new();
            for utxos in txmap.into_values() {
                // check if we know the secret to any of the spks
                let mut secret = None;
                for utxo in utxos.iter() {
                    let spk = utxo.scriptpubkey.as_bytes();
                    if let Some(s) = secrets_map.get(spk) {
                        secret = Some(s);
                        break;
                    }
                }

                // skip this tx if no secret is found
                let secret = match secret {
                    Some(secret) => secret,
                    None => continue,
                };

                let output_keys: Result<Vec<XOnlyPublicKey>> = utxos
                    .iter()
                    .filter_map(|x| {
                        if x.scriptpubkey.is_p2tr() {
                            Some(
                                XOnlyPublicKey::from_slice(&x.scriptpubkey.as_bytes()[2..])
                                    .map_err(Error::from),
                            )
                        } else {
                            None
                        }
                    })
                    .collect();

                let ours = client.sp_receiver.scan_transaction(secret, output_keys?)?;

                for utxo in utxos {
                    if !utxo.scriptpubkey.is_p2tr() || utxo.spent {
                        continue;
                    }

                    match XOnlyPublicKey::from_slice(&utxo.scriptpubkey.as_bytes()[2..]) {
                        Ok(xonly) => {
                            for (label, map) in ours.iter() {
                                if let Some(scalar) = map.get(&xonly) {
                                    result.push((label.clone(), utxo, *scalar));
                                    break;
                                }
                            }
                        }
                        Err(_) => todo!(),
                    }
                }
            }
            result
        };

        Ok(res)
    }

    /// Check if block contains relevant output transactions
    ///
    /// This is a default implementation that can be overridden if needed
    fn check_block_outputs(
        created_utxo_filter: BlockFilter,
        blkhash: BlockHash,
        candidate_spks: Vec<&[u8; 34]>,
    ) -> Result<bool> {
        // check output scripts
        let output_keys: Vec<_> = candidate_spks
            .into_iter()
            .map(|spk| spk[2..].as_ref())
            .collect();

        // note: match will always return true for an empty query!
        if !output_keys.is_empty() {
            Ok(created_utxo_filter.match_any(&blkhash, &mut output_keys.into_iter())?)
        } else {
            Ok(false)
        }
    }

    /// Get input hashes for owned outpoints
    fn get_input_hashes(&self, blkhash: BlockHash) -> Result<HashMap<[u8; 8], OutPoint>>;

    /// Check if block contains relevant input transactions
    ///
    /// This is a default implementation that can be overridden if needed
    fn check_block_inputs(
        &self,
        spent_filter: BlockFilter,
        blkhash: BlockHash,
        input_hashes: Vec<[u8; 8]>,
    ) -> Result<bool> {
        // note: match will always return true for an empty query!
        if !input_hashes.is_empty() {
            Ok(spent_filter.match_any(&blkhash, &mut input_hashes.into_iter())?)
        } else {
            Ok(false)
        }
    }
}

/// Drain block results in height order and run a callback for each contiguous block.
pub fn process_ordered_block_results<S, I, F>(
    scanner: &mut S,
    start: Height,
    end: Height,
    block_data_iter: I,
    mut handle_block: F,
) -> Result<()>
where
    S: SpScanner + ?Sized,
    I: Iterator<Item = Result<BlockData>>,
    F: FnMut(&mut S, BlockData, bool) -> Result<()>,
{
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    let mut update_time = Instant::now();
    let mut pending: BTreeMap<u32, BlockData> = BTreeMap::new();
    let mut next_height = start.to_consensus_u32();
    let mut first_error: Option<crate::error::Error> = None;

    for blockdata_result in block_data_iter {
        match blockdata_result {
            Ok(blockdata) => {
                pending.insert(blockdata.blkheight.to_consensus_u32(), blockdata);
            }
            Err(e) if first_error.is_none() => {
                first_error = Some(e);
            }
            Err(_) => {}
        }

        while let Some(blockdata) = pending.remove(&next_height) {
            let blkheight = blockdata.blkheight;

            if scanner.should_interrupt() {
                scanner.save_state()?;
                return Ok(());
            }

            let save_to_storage =
                blkheight == end || update_time.elapsed() > Duration::from_secs(30);
            handle_block(scanner, blockdata, save_to_storage)?;

            if save_to_storage {
                update_time = Instant::now();
            }

            next_height += 1;
        }
    }

    while let Some(blockdata) = pending.remove(&next_height) {
        let blkheight = blockdata.blkheight;

        if scanner.should_interrupt() {
            scanner.save_state()?;
            return Ok(());
        }

        let save_to_storage = blkheight == end || update_time.elapsed() > Duration::from_secs(30);
        handle_block(scanner, blockdata, save_to_storage)?;

        if save_to_storage {
            update_time = Instant::now();
        }

        next_height += 1;
    }

    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use bitcoin::hashes::Hash;
    use std::cell::{Cell, RefCell};

    struct MockScanner {
        saves: Cell<usize>,
    }

    impl MockScanner {
        fn new() -> Self {
            Self {
                saves: Cell::new(0),
            }
        }
    }

    impl SpScanner for MockScanner {
        fn scan_blocks(
            &mut self,
            _start: Height,
            _end: Height,
            _dust_limit: Option<Amount>,
            _with_cutthrough: bool,
        ) -> Result<()> {
            panic!("not used");
        }

        fn process_block(
            &mut self,
            _blockdata: BlockData,
        ) -> Result<(HashMap<OutPoint, OwnedOutput>, HashSet<OutPoint>)> {
            panic!("not used");
        }

        fn process_block_outputs(
            &self,
            _blkheight: Height,
            _tweaks: &[bitcoin::secp256k1::PublicKey],
            _new_utxo_filter: FilterData,
        ) -> Result<HashMap<OutPoint, OwnedOutput>> {
            panic!("not used");
        }

        fn process_block_inputs(
            &self,
            _blkheight: Height,
            _spent_filter: FilterData,
        ) -> Result<HashSet<OutPoint>> {
            panic!("not used");
        }

        fn get_block_data_iterator(
            &self,
            _range: std::ops::RangeInclusive<u32>,
            _dust_limit: Option<Amount>,
            _with_cutthrough: bool,
        ) -> crate::BlockDataIterator {
            panic!("not used");
        }

        fn should_interrupt(&self) -> bool {
            false
        }

        fn save_state(&mut self) -> Result<()> {
            self.saves.set(self.saves.get() + 1);
            Ok(())
        }

        fn record_outputs(
            &mut self,
            _height: Height,
            _block_hash: BlockHash,
            _outputs: HashMap<OutPoint, OwnedOutput>,
        ) -> Result<()> {
            panic!("not used");
        }

        fn record_inputs(
            &mut self,
            _height: Height,
            _block_hash: BlockHash,
            _inputs: HashSet<OutPoint>,
        ) -> Result<()> {
            panic!("not used");
        }

        fn record_progress(
            &mut self,
            _start: Height,
            _current: Height,
            _end: Height,
        ) -> Result<()> {
            Ok(())
        }

        fn client(&self) -> &SpClient {
            panic!("not used");
        }

        fn backend(&self) -> &dyn ChainBackend {
            panic!("not used");
        }

        fn updater(&mut self) -> &mut dyn Updater {
            panic!("not used");
        }

        fn get_input_hashes(&self, _blkhash: BlockHash) -> Result<HashMap<[u8; 8], OutPoint>> {
            panic!("not used");
        }
    }

    fn block(height: u32) -> BlockData {
        BlockData {
            blkheight: Height::from_consensus(height).expect("valid height"),
            blkhash: BlockHash::from_byte_array([height as u8; 32]),
            tweaks: Vec::new(),
            new_utxo_filter: FilterData {
                block_hash: BlockHash::from_byte_array([height as u8; 32]),
                data: Vec::new(),
            },
            spent_filter: FilterData {
                block_hash: BlockHash::from_byte_array([height as u8; 32]),
                data: Vec::new(),
            },
        }
    }

    #[test]
    fn drains_blocks_in_order_and_returns_first_error() {
        let mut scanner = MockScanner::new();
        let seen = RefCell::new(Vec::new());
        let blocks = vec![
            Ok(block(2)),
            Ok(block(1)),
            Err(Error::Sighash("boom".to_string())),
            Ok(block(3)),
        ];

        let err = process_ordered_block_results(
            &mut scanner,
            Height::from_consensus(1).expect("valid height"),
            Height::from_consensus(3).expect("valid height"),
            blocks.into_iter(),
            |_, blockdata, save_to_storage| {
                seen.borrow_mut()
                    .push((blockdata.blkheight.to_consensus_u32(), save_to_storage));
                Ok(())
            },
        )
        .expect_err("expected first fetch error");

        assert_eq!(seen.into_inner(), vec![(1, false), (2, false), (3, true)]);
        assert_eq!(err.to_string(), "sighash: boom");
        assert_eq!(scanner.saves.get(), 0);
    }
}

/// Async version of SpScanner for non-blocking I/O operations
///
/// This trait provides async methods for scanning silent payment blocks,
/// allowing for concurrent operations and better integration with async ecosystems.
/// Particularly useful for WASM targets and UI applications.
#[cfg(feature = "async")]
#[async_trait::async_trait]
pub trait AsyncSpScanner: Send + Sync {
    /// Scan a range of blocks for silent payment outputs and inputs
    ///
    /// # Arguments
    /// * `start` - Starting block height (inclusive)
    /// * `end` - Ending block height (inclusive)
    /// * `dust_limit` - Minimum amount to consider (dust outputs are ignored)
    /// * `with_cutthrough` - Whether to use cutthrough optimization
    async fn scan_blocks(
        &mut self,
        start: Height,
        end: Height,
        dust_limit: Amount,
        with_cutthrough: bool,
    ) -> Result<()>;

    /// Process a single block's data
    ///
    /// # Arguments
    /// * `blockdata` - Block data containing tweaks and filters
    ///
    /// # Returns
    /// * `(found_outputs, found_inputs)` - Tuple of found outputs and spent inputs
    async fn process_block(
        &mut self,
        blockdata: BlockData,
    ) -> Result<(HashMap<OutPoint, OwnedOutput>, HashSet<OutPoint>)>;

    /// Process block outputs to find owned silent payment outputs
    ///
    /// # Arguments
    /// * `blkheight` - Block height
    /// * `tweaks` - List of tweak public keys
    /// * `new_utxo_filter` - Filter data for new UTXOs
    ///
    /// # Returns
    /// * Map of outpoints to owned outputs
    async fn process_block_outputs(
        &self,
        blkheight: Height,
        tweaks: Vec<bitcoin::secp256k1::PublicKey>,
        new_utxo_filter: FilterData,
    ) -> Result<HashMap<OutPoint, OwnedOutput>>;

    /// Process block inputs to find spent outputs
    ///
    /// # Arguments
    /// * `blkheight` - Block height
    /// * `spent_filter` - Filter data for spent outputs
    ///
    /// # Returns
    /// * Set of spent outpoints
    async fn process_block_inputs(
        &self,
        blkheight: Height,
        spent_filter: FilterData,
    ) -> Result<HashSet<OutPoint>>;

    /// Get the block data stream for a range of blocks
    ///
    /// # Arguments
    /// * `range` - Range of block heights
    /// * `dust_limit` - Minimum amount to consider
    /// * `with_cutthrough` - Whether to use cutthrough optimization
    ///
    /// # Returns
    /// * Stream of block data results
    fn get_block_data_stream(
        &self,
        range: std::ops::RangeInclusive<u32>,
        dust_limit: Amount,
        with_cutthrough: bool,
    ) -> crate::backend::BlockDataStream;

    /// Check if scanning should be interrupted
    ///
    /// # Returns
    /// * `true` if scanning should stop, `false` otherwise
    fn should_interrupt(&self) -> bool;

    /// Save current state to persistent storage
    async fn save_state(&mut self) -> Result<()>;

    /// Record found outputs for a block
    ///
    /// # Arguments
    /// * `height` - Block height
    /// * `block_hash` - Block hash
    /// * `outputs` - Found outputs
    async fn record_outputs(
        &mut self,
        height: Height,
        block_hash: BlockHash,
        outputs: HashMap<OutPoint, OwnedOutput>,
    ) -> Result<()>;

    /// Record spent inputs for a block
    ///
    /// # Arguments
    /// * `height` - Block height
    /// * `block_hash` - Block hash
    /// * `inputs` - Spent inputs
    async fn record_inputs(
        &mut self,
        height: Height,
        block_hash: BlockHash,
        inputs: HashSet<OutPoint>,
    ) -> Result<()>;

    /// Record scan progress
    ///
    /// # Arguments
    /// * `start` - Start height
    /// * `current` - Current height
    /// * `end` - End height
    async fn record_progress(&mut self, start: Height, current: Height, end: Height) -> Result<()>;

    /// Get the silent payment client
    fn client(&self) -> &SpClient;

    /// Get the async chain backend
    fn backend(&self) -> &dyn crate::backend::AsyncChainBackend;

    /// Get the async updater
    fn updater(&mut self) -> &mut dyn crate::updater::AsyncUpdater;

    // Helper methods with default implementations

    /// Process multiple blocks from a stream
    ///
    /// This is a default implementation that can be overridden if needed.
    /// Blocks are collected first, then processed in height order. This ensures
    /// progress is reported correctly even when blocks arrive out of order from
    /// parallel fetching. If any block fetch fails, the error is returned after
    /// processing all successfully fetched blocks.
    async fn process_blocks(
        &mut self,
        start: Height,
        end: Height,
        mut block_data_stream: crate::backend::BlockDataStream,
    ) -> Result<()> {
        use futures::StreamExt;
        use std::collections::BTreeMap;
        use std::time::{Duration, Instant};

        let mut update_time = Instant::now();
        let mut blocks: BTreeMap<u32, BlockData> = BTreeMap::new();
        let mut first_error: Option<crate::error::Error> = None;

        // Collect all results, storing first error
        while let Some(blockdata_result) = block_data_stream.next().await {
            match blockdata_result {
                Ok(blockdata) => {
                    blocks.insert(blockdata.blkheight.to_consensus_u32(), blockdata);
                }
                Err(e) if first_error.is_none() => {
                    first_error = Some(e);
                }
                Err(_) => {} // Ignore subsequent errors
            }
        }

        // Process collected blocks in height order
        for (_, blockdata) in blocks {
            let blkheight = blockdata.blkheight;
            let blkhash = blockdata.blkhash;

            // stop scanning and return if interrupted
            if self.should_interrupt() {
                self.save_state().await?;
                return Ok(());
            }

            let mut save_to_storage = false;

            // always save on last block or after 30 seconds since last save
            if blkheight == end || update_time.elapsed() > Duration::from_secs(30) {
                save_to_storage = true;
            }

            let (found_outputs, found_inputs) = self.process_block(blockdata).await?;

            if !found_outputs.is_empty() {
                save_to_storage = true;
                self.record_outputs(blkheight, blkhash, found_outputs)
                    .await?;
            }

            if !found_inputs.is_empty() {
                save_to_storage = true;
                self.record_inputs(blkheight, blkhash, found_inputs).await?;
            }

            // tell the updater we scanned this block
            self.record_progress(start, blkheight, end).await?;

            if save_to_storage {
                self.save_state().await?;
                update_time = Instant::now();
            }
        }

        // Return error after processing all available blocks
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Scan UTXOs for a given block and secrets map
    ///
    /// This is a default implementation that can be overridden if needed
    async fn scan_utxos(
        &self,
        blkheight: Height,
        secrets_map: HashMap<[u8; 34], bitcoin::secp256k1::PublicKey>,
    ) -> Result<Vec<(Option<Label>, UtxoData, bitcoin::secp256k1::Scalar)>> {
        let utxos = self.backend().utxos(blkheight).await?;

        // Group utxos by the txid
        let mut txmap: HashMap<Txid, Vec<UtxoData>> = HashMap::new();
        for utxo in utxos {
            txmap.entry(utxo.txid).or_default().push(utxo);
        }

        let client = self.client();

        // Parallel transaction scanning on native platforms with parallel feature
        // This uses Rayon for CPU parallelism. Rayon uses its own thread pool internally,
        // so while this blocks the current async task, it doesn't block the entire runtime
        // on multi-threaded executors. The CPU work benefits significantly from parallelism.
        #[cfg(all(not(target_arch = "wasm32"), feature = "parallel"))]
        let res = {
            use rayon::prelude::*;
            use std::sync::Arc;

            // Clone data needed for parallel processing
            let secrets_map = Arc::new(secrets_map);
            let client = Arc::new(client.clone());

            // Run CPU-intensive Rayon work
            // Rayon uses its own thread pool, so this parallelizes across CPU cores
            txmap
                .into_par_iter()
                .filter_map(|(_, utxos)| {
                    // check if we know the secret to any of the spks
                    let secret = utxos.iter().find_map(|utxo| {
                        let spk = utxo.scriptpubkey.as_bytes();
                        secrets_map.get(spk)
                    })?;

                    let output_keys: Vec<XOnlyPublicKey> = utxos
                        .iter()
                        .filter_map(|x| {
                            if x.scriptpubkey.is_p2tr() {
                                XOnlyPublicKey::from_slice(&x.scriptpubkey.as_bytes()[2..]).ok()
                            } else {
                                None
                            }
                        })
                        .collect();

                    // CPU-intensive cryptographic operation
                    let ours = client
                        .sp_receiver
                        .scan_transaction(secret, output_keys)
                        .ok()?;

                    // Match UTXOs against our keys
                    let matched: Vec<_> = utxos
                        .into_iter()
                        .filter(|utxo| utxo.scriptpubkey.is_p2tr() && !utxo.spent)
                        .filter_map(|utxo| {
                            let xonly =
                                XOnlyPublicKey::from_slice(&utxo.scriptpubkey.as_bytes()[2..])
                                    .ok()?;
                            ours.iter().find_map(|(label, map)| {
                                map.get(&xonly)
                                    .map(|scalar| (label.clone(), utxo.clone(), *scalar))
                            })
                        })
                        .collect();

                    if matched.is_empty() {
                        None
                    } else {
                        Some(matched)
                    }
                })
                .flatten()
                .collect()
        };

        // Sequential fallback (WASM or no parallel feature)
        #[cfg(not(all(not(target_arch = "wasm32"), feature = "parallel")))]
        let res: Vec<_> = {
            let mut result = Vec::new();
            for utxos in txmap.into_values() {
                // check if we know the secret to any of the spks
                let mut secret = None;
                for utxo in utxos.iter() {
                    let spk = utxo.scriptpubkey.as_bytes();
                    if let Some(s) = secrets_map.get(spk) {
                        secret = Some(s);
                        break;
                    }
                }

                // skip this tx if no secret is found
                let secret = match secret {
                    Some(secret) => secret,
                    None => continue,
                };

                let output_keys: Result<Vec<XOnlyPublicKey>> = utxos
                    .iter()
                    .filter_map(|x| {
                        if x.scriptpubkey.is_p2tr() {
                            Some(
                                XOnlyPublicKey::from_slice(&x.scriptpubkey.as_bytes()[2..])
                                    .map_err(crate::error::Error::from),
                            )
                        } else {
                            None
                        }
                    })
                    .collect();

                let ours = client.sp_receiver.scan_transaction(secret, output_keys?)?;

                for utxo in utxos {
                    if !utxo.scriptpubkey.is_p2tr() || utxo.spent {
                        continue;
                    }

                    match XOnlyPublicKey::from_slice(&utxo.scriptpubkey.as_bytes()[2..]) {
                        Ok(xonly) => {
                            for (label, map) in ours.iter() {
                                if let Some(scalar) = map.get(&xonly) {
                                    result.push((label.clone(), utxo.clone(), *scalar));
                                    break;
                                }
                            }
                        }
                        Err(_) => todo!(),
                    }
                }
            }
            result
        };

        Ok(res)
    }

    /// Check if block contains relevant output transactions
    ///
    /// This is a default implementation that can be overridden if needed
    fn check_block_outputs(
        created_utxo_filter: BlockFilter,
        blkhash: BlockHash,
        candidate_spks: Vec<&[u8; 34]>,
    ) -> Result<bool> {
        // check output scripts
        let output_keys: Vec<_> = candidate_spks
            .into_iter()
            .map(|spk| spk[2..].as_ref())
            .collect();

        // note: match will always return true for an empty query!
        if !output_keys.is_empty() {
            Ok(created_utxo_filter.match_any(&blkhash, &mut output_keys.into_iter())?)
        } else {
            Ok(false)
        }
    }

    /// Get input hashes for owned outpoints
    async fn get_input_hashes(&self, blkhash: BlockHash) -> Result<HashMap<[u8; 8], OutPoint>>;

    /// Check if block contains relevant input transactions
    ///
    /// This is a default implementation that can be overridden if needed
    fn check_block_inputs(
        &self,
        spent_filter: BlockFilter,
        blkhash: BlockHash,
        input_hashes: Vec<[u8; 8]>,
    ) -> Result<bool> {
        // note: match will always return true for an empty query!
        if !input_hashes.is_empty() {
            Ok(spent_filter.match_any(&blkhash, &mut input_hashes.into_iter())?)
        } else {
            Ok(false)
        }
    }
}
