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

/// Marker for "must be `Sync` when the windowed parallel scan is enabled".
///
/// On native builds with the `parallel` feature the read-only output matching is
/// run with `par_iter` over a window of blocks, which shares `&Self` across rayon
/// threads and therefore requires `Sync`. On wasm / no-parallel builds the scan
/// is sequential, so this imposes no bound. Keeping it as one marker lets the
/// `SpScanner` impl and the windowed loop carry a single, conditionally-empty
/// bound instead of duplicating their definitions per cfg.
#[cfg(all(not(target_arch = "wasm32"), feature = "parallel"))]
pub trait MaybeSync: Sync {}
#[cfg(all(not(target_arch = "wasm32"), feature = "parallel"))]
impl<T: Sync + ?Sized> MaybeSync for T {}

#[cfg(not(all(not(target_arch = "wasm32"), feature = "parallel")))]
pub trait MaybeSync {}
#[cfg(not(all(not(target_arch = "wasm32"), feature = "parallel")))]
impl<T: ?Sized> MaybeSync for T {}

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

    /// Read-only output matching for a single block.
    ///
    /// This is the expensive, embarrassingly-parallel part of the scan
    /// (candidate-spk derivation + GCS output-filter test + UTXO scan). It only
    /// reads `&self` (the client / backend) and the block's own data, so it can
    /// be run concurrently across a window of blocks by the two-phase receive
    /// pass.
    ///
    /// # Returns
    /// * Map of outpoints to owned outputs found in this block
    fn match_block_outputs(&self, blockdata: &BlockData) -> Result<HashMap<OutPoint, OwnedOutput>>;

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

    /// Get the receive-only block data iterator for a range of blocks.
    ///
    /// Used by the two-phase receive pass; the spent filter is fetched
    /// separately in the spend sweep.
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
    ) -> crate::BlockDataIterator {
        self.backend()
            .get_block_data_for_range(range, dust_limit, with_cutthrough)
    }

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

    /// Get input hashes for an explicit owned-outpoint set.
    ///
    /// The two-phase spend pass owns its set in memory (it is not on `self`),
    /// so this takes the set as a parameter instead of reading the scanner's.
    fn input_hashes_for(
        &self,
        blkhash: BlockHash,
        owned: &HashSet<OutPoint>,
    ) -> Result<HashMap<[u8; 8], OutPoint>>;

    /// Match a block's spent filter against an explicit owned set.
    ///
    /// Order-free spend detection used by the two-phase pass: builds the input
    /// hashes for `owned`, tests the GCS spent filter, and on a hit fetches the
    /// spent index and returns the owned outpoints spent in this block.
    fn match_inputs_for(
        &self,
        blkheight: Height,
        spent_filter: FilterData,
        owned: &HashSet<OutPoint>,
    ) -> Result<HashSet<OutPoint>> {
        let mut res = HashSet::new();
        let blkhash = spent_filter.block_hash;
        let input_hashes_map = self.input_hashes_for(blkhash, owned)?;

        let blkfilter = BlockFilter::new(&spent_filter.data);
        let matched_inputs = self.check_block_inputs(
            blkfilter,
            blkhash,
            input_hashes_map.keys().cloned().collect(),
        )?;

        if matched_inputs {
            let spent = self.backend().spent_index(blkheight)?.data;
            for spent in spent {
                let hex: &[u8] = spent.as_ref();
                if let Some(outpoint) = input_hashes_map.get(hex) {
                    res.insert(*outpoint);
                }
            }
        }
        Ok(res)
    }

    /// Advance the contiguous receive frontier to a fully received block.
    ///
    /// Used by the two-phase receive pass as its contiguous tip fills in, so the
    /// frontier stays contiguous and monotonic despite order-free commits.
    fn record_frontier(&mut self, height: Height, block_hash: BlockHash) -> Result<()> {
        self.updater().record_scan_frontier(height, block_hash)
    }

    /// Advance the spend frontier to a fully swept height.
    ///
    /// Used by the two-phase spend sweep; the default delegates to the updater.
    fn record_spend_frontier(&mut self, height: Height) -> Result<()> {
        self.updater().record_spend_frontier(height)
    }

    /// Return the highest spend-swept height, if any.
    ///
    /// Used to resume the spend sweep; the default delegates to the updater.
    fn spend_frontier(&mut self) -> Result<Option<u32>> {
        self.updater().spend_frontier()
    }

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

/// Number of contiguous in-order blocks matched in parallel per window.
///
/// The expensive output matching ([`SpScanner::match_block_outputs`]) is run
/// with an outer `par_iter` over the W blocks in a window, so that even sparse
/// early-mainnet blocks (few tweaks each) keep every core busy — the inner
/// per-tweak `par_chunks` alone leaves cores idle on such blocks. W is sized to
/// give the pool well over 2x the core count of independent tasks while keeping
/// memory bounded to W blocks held at once.
///
/// The window is sized to the core count at runtime (capped by this max); a fixed
/// large window over-buffers and does a long serial commit burst, which regresses
/// on low-core devices (e.g. mobile) while barely helping — per-core sizing fills
/// many-core machines without that overhead.
#[cfg(all(not(target_arch = "wasm32"), feature = "parallel"))]
const MATCH_WINDOW_MAX: usize = 64;

/// Match-window size, overridable via `BWK_SP_MATCH_WINDOW` for sweeping. When
/// set it overrides both the per-core sizing and the `MATCH_WINDOW_MAX` cap (so a
/// sweep can exceed 64); unset keeps the per-core default capped at the max.
#[cfg(all(not(target_arch = "wasm32"), feature = "parallel"))]
fn match_window_cap() -> usize {
    if let Some(n) = std::env::var("BWK_SP_MATCH_WINDOW")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0)
    {
        return n;
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8)
        .min(MATCH_WINDOW_MAX)
}

/// Match outputs for every block in `window` (read-only), preserving order.
///
/// Native+parallel: the blocks are matched concurrently with `par_iter`
/// (outer parallelism that saturates cores even for sparse blocks). The first
/// error in height order is returned. WASM/no-parallel: sequential fallback.
fn match_window_outputs<S>(
    scanner: &S,
    window: &[BlockData],
) -> Result<Vec<HashMap<OutPoint, OwnedOutput>>>
where
    S: SpScanner + MaybeSync + ?Sized,
{
    #[cfg(all(not(target_arch = "wasm32"), feature = "parallel"))]
    {
        window
            .par_iter()
            .map(|blockdata| scanner.match_block_outputs(blockdata))
            .collect()
    }
    #[cfg(not(all(not(target_arch = "wasm32"), feature = "parallel")))]
    {
        window
            .iter()
            .map(|blockdata| scanner.match_block_outputs(blockdata))
            .collect()
    }
}

/// How often the receive frontier and spend frontier are persisted.
const CHECKPOINT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Two-phase, order-free scan over `[start, end]` with two persisted frontiers.
///
/// RECEIVE frontier: highest height whose receive (output) scan is contiguously
/// done. Drives the caller's `next_scan_start`. SPEND frontier: highest height
/// whose spend (input) sweep is done; trails the receive frontier.
///
/// Phase 1 runs a single order-free receive pass over `[start, end]`: blocks are
/// matched in arrival order (a slow block does not stall others), discovered
/// coins are recorded and added to `owned`, and a completion bitmap advances the
/// contiguous receive frontier, checkpointed about once a minute. Phase 2 then
/// sweeps spends over the un-swept tail up to `end` (the receive frontier).
///
/// `owned` is the starting owned outpoint set (restored coins plus any found
/// here). Returns the final in-memory owned set so the caller can refresh its
/// owned-outpoint view.
pub fn process_two_phase<S>(
    scanner: &mut S,
    start: Height,
    end: Height,
    dust_limit: Option<Amount>,
    with_cutthrough: bool,
    mut owned: HashSet<OutPoint>,
) -> Result<HashSet<OutPoint>>
where
    S: SpScanner + MaybeSync + ?Sized,
{
    use std::time::Instant;

    let start_u32 = start.to_consensus_u32();
    let end_u32 = end.to_consensus_u32();
    let len = (end_u32 - start_u32 + 1) as usize;

    // Seed the spend frontier to start-1 before receiving, so a crash mid-receive
    // resumes the spend sweep from this floor rather than from a later receive
    // frontier. Nothing below `start` was received without also being spend-swept,
    // so start-1 is the correct floor on the first run.
    if scanner.spend_frontier()?.is_none() {
        if let Some(floor) = start_u32.checked_sub(1) {
            scanner.record_spend_frontier(Height::from_consensus(floor)?)?;
            scanner.save_state()?;
        }
    }

    // Phase 1 RECEIVE: order-free over [start, end]. `done`/`hashes` record which
    // heights have been received and their block hash; `recv_tip` is the highest
    // contiguously-done height (the receive frontier), advancing only as the gap
    // below it fills, so a slow block never stalls others.
    let mut iter =
        scanner.get_block_data_iterator(start_u32..=end_u32, dust_limit, with_cutthrough);
    let mut done = vec![false; len];
    let mut hashes: Vec<Option<BlockHash>> = vec![None; len];
    let mut recv_tip: Option<u32> = None;
    let mut first_error: Option<crate::error::Error> = None;
    let mut last_checkpoint = Instant::now();

    #[cfg(all(not(target_arch = "wasm32"), feature = "parallel"))]
    let window_cap = match_window_cap();
    #[cfg(not(all(not(target_arch = "wasm32"), feature = "parallel")))]
    let window_cap = 1usize;

    'receive: loop {
        // Pull up to `window_cap` blocks in arrival order (no height gating).
        let mut window: Vec<BlockData> = Vec::with_capacity(window_cap);
        while window.len() < window_cap {
            match iter.next() {
                Some(Ok(blockdata)) => window.push(blockdata),
                Some(Err(e)) => {
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
                None => break,
            }
        }

        if window.is_empty() {
            break 'receive;
        }

        if scanner.should_interrupt() {
            scanner.save_state()?;
            return Ok(owned);
        }

        let matched = match_window_outputs(scanner, &window)?;

        for (blockdata, outs) in window.into_iter().zip(matched) {
            let blkheight = blockdata.blkheight;
            let blkhash = blockdata.blkhash;
            let idx = (blkheight.to_consensus_u32() - start_u32) as usize;
            done[idx] = true;
            hashes[idx] = Some(blkhash);
            if !outs.is_empty() {
                for outpoint in outs.keys() {
                    owned.insert(*outpoint);
                }
                scanner.record_outputs(blkheight, blkhash, outs)?;
            }
        }

        // Advance the contiguous receive frontier over any newly-filled gap.
        let mut next = recv_tip.map(|h| h + 1).unwrap_or(start_u32);
        while next <= end_u32 && done[(next - start_u32) as usize] {
            recv_tip = Some(next);
            next += 1;
        }

        // Checkpoint the receive frontier about once a minute.
        if last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
            if let Some(tip) = recv_tip {
                let idx = (tip - start_u32) as usize;
                let hash = hashes[idx].ok_or(crate::error::Error::MissingBlockHash(tip))?;
                scanner.record_frontier(Height::from_consensus(tip)?, hash)?;
                scanner.record_progress(start, Height::from_consensus(tip)?, end)?;
                scanner.save_state()?;
            }
            last_checkpoint = Instant::now();
        }
    }

    if let Some(e) = first_error {
        return Err(e);
    }

    // The receive frontier must reach `end`; a short stream means a missing block.
    if recv_tip != Some(end_u32) {
        return Err(crate::error::Error::MissingBlockHash(end_u32));
    }
    let end_hash = hashes[len - 1].ok_or(crate::error::Error::MissingBlockHash(end_u32))?;
    scanner.record_frontier(end, end_hash)?;
    scanner.record_progress(start, end, end)?;
    scanner.save_state()?;

    // Phase 2 SPEND: sweep the un-swept tail. INVARIANT: spend only sweeps heights
    // <= the receive frontier (now `end`), so the owned set is complete for every
    // swept height. A coin received at N can be spent only at M >= N, and N is
    // above the prior receive frontier (>= spend frontier), so M is always in this
    // un-swept tail; this is what prevents missing such a spend.
    // spend_start can trail `start`: the spend frontier persists separately and may
    // lag the receive frontier after a crash, so the sweep covers the un-swept tail
    // [spend_frontier+1 .. end] even where that dips below this call's start.
    let spend_start = scanner
        .spend_frontier()?
        .map(|h| h + 1)
        .unwrap_or(start_u32);
    let mut last_checkpoint = Instant::now();
    for h in spend_start..=end_u32 {
        if scanner.should_interrupt() {
            scanner.save_state()?;
            return Ok(owned);
        }
        if owned.is_empty() {
            break;
        }
        let height = Height::from_consensus(h)?;
        let spent_filter = scanner.backend().spent_filter(height)?;
        let blkhash = spent_filter.block_hash;
        let ins = scanner.match_inputs_for(height, spent_filter, &owned)?;
        if !ins.is_empty() {
            for outpoint in &ins {
                owned.remove(outpoint);
            }
            scanner.record_inputs(height, blkhash, ins)?;
        }
        if last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
            scanner.record_spend_frontier(height)?;
            scanner.save_state()?;
            last_checkpoint = Instant::now();
        }
    }
    // Persist the spend frontier once more at the end (last fully swept height).
    scanner.record_spend_frontier(end)?;
    scanner.save_state()?;

    Ok(owned)
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
