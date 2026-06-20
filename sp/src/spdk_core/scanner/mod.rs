use std::collections::{HashMap, HashSet};

use crate::silentpayments::receiving::Label;
use crate::spdk_core::error::Error;
use crate::spdk_core::error::Result;
use bitcoin::{
    absolute::Height, bip158::BlockFilter, Amount, BlockHash, OutPoint, Txid, XOnlyPublicKey,
};

use crate::spdk_core::{
    BlockData, ChainBackend, FilterData, OwnedOutput, SpClient, Updater, UtxoData,
};

/// Marker for "must be `Sync` when the windowed parallel scan is enabled".
///
/// On native builds with the `parallel` feature the read-only output matching is
/// run across a pool of scoped threads over a window of blocks, which shares
/// `&Self` across threads and therefore requires `Sync`. On wasm / no-parallel builds the scan
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
        tweaks: &[[u8; 33]],
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
    ) -> crate::spdk_core::BlockDataIterator {
        self.backend()
            .get_block_data_for_range(range, dust_limit, with_cutthrough)
    }

    /// Check if scanning should be interrupted
    ///
    /// # Returns
    /// * `true` if scanning should stop, `false` otherwise
    fn should_interrupt(&self) -> bool;

    /// Save current state to persistent storage
    fn save_state(&self) -> Result<()>;

    /// Record found outputs for a block
    ///
    /// # Arguments
    /// * `height` - Block height
    /// * `block_hash` - Block hash
    /// * `outputs` - Found outputs
    fn record_outputs(
        &self,
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
        &self,
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
    fn record_progress(&self, start: Height, current: Height, end: Height) -> Result<()>;

    /// Get the silent payment client
    fn client(&self) -> &SpClient;

    /// Get the chain backend
    fn backend(&self) -> &dyn ChainBackend;

    /// Get the updater
    fn updater(&self) -> &dyn Updater;

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

        // Transaction scanning (only reached when a block matches the filter).
        // Sequential within the block; the scan parallelizes across blocks in the
        // match window.
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
    fn record_frontier(&self, height: Height, block_hash: BlockHash) -> Result<()> {
        self.updater().record_scan_frontier(height, block_hash)
    }

    /// Advance the spend frontier to a fully swept height.
    ///
    /// Used by the two-phase spend sweep; the default delegates to the updater.
    fn record_spend_frontier(&self, height: Height) -> Result<()> {
        self.updater().record_spend_frontier(height)
    }

    /// Return the highest spend-swept height, if any.
    ///
    /// Used to resume the spend sweep; the default delegates to the updater.
    fn spend_frontier(&self) -> Result<Option<u32>> {
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
/// across a pool of scoped threads over the W blocks in a window, so that even
/// sparse early-mainnet blocks (few tweaks each) keep every core busy —
/// parallelizing within a block alone leaves cores idle on such blocks. W is sized to
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

/// Commit one matched block in the order-free receive pass: mark it done, record
/// any found outputs, advance the contiguous receive frontier over newly-filled
/// gaps, notify progress (first advance, then every 100 blocks), and persist the
/// frontier about once a minute. All scanner calls go through `&self`, so this
/// runs on the committing thread while the compute workers keep matching.
#[allow(clippy::too_many_arguments)]
fn commit_block<S>(
    scanner: &S,
    start: Height,
    end: Height,
    start_u32: u32,
    end_u32: u32,
    blockdata: BlockData,
    outs: HashMap<OutPoint, OwnedOutput>,
    owned: &mut HashSet<OutPoint>,
    done: &mut [bool],
    hashes: &mut [Option<BlockHash>],
    recv_tip: &mut Option<u32>,
    last_progress: &mut u32,
    notified_any: &mut bool,
    last_checkpoint: &mut std::time::Instant,
) -> Result<()>
where
    S: SpScanner + ?Sized,
{
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

    // Advance the contiguous receive frontier over any newly-filled gap.
    let mut next = recv_tip.map(|h| h + 1).unwrap_or(start_u32);
    while next <= end_u32 && done[(next - start_u32) as usize] {
        *recv_tip = Some(next);
        next += 1;
    }

    // Notify on the first advance, then every 100 blocks the frontier advances.
    if let Some(tip) = *recv_tip {
        if !*notified_any || tip - *last_progress >= 100 {
            scanner.record_progress(start, Height::from_consensus(tip)?, end)?;
            *last_progress = tip;
            *notified_any = true;
        }
    }

    // Persist the receive frontier about once a minute.
    if last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
        if let Some(tip) = *recv_tip {
            let i = (tip - start_u32) as usize;
            let hash = hashes[i].ok_or(crate::spdk_core::error::Error::MissingBlockHash(tip))?;
            scanner.record_frontier(Height::from_consensus(tip)?, hash)?;
            scanner.save_state()?;
        }
        *last_checkpoint = std::time::Instant::now();
    }
    Ok(())
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
    scanner: &S,
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
    let iter = scanner.get_block_data_iterator(start_u32..=end_u32, dust_limit, with_cutthrough);
    let mut done = vec![false; len];
    let mut hashes: Vec<Option<BlockHash>> = vec![None; len];
    let mut recv_tip: Option<u32> = None;
    // Notify on the first frontier advance (immediate feedback that the scan is
    // live), then every 100 blocks as the receive frontier advances.
    let mut last_progress = start_u32.saturating_sub(1);
    let mut notified_any = false;
    let mut last_checkpoint = Instant::now();

    // Pipeline: the fetch backend feeds `iter`; the compute workers pull-and-match
    // blocks continuously (no per-window barrier); this thread commits results as
    // they arrive. The receive frontier is order-free, so out-of-order arrival is
    // fine. Commits go through `&self` (interior mutability), so the workers can
    // keep matching while this thread commits.
    #[cfg(all(not(target_arch = "wasm32"), feature = "parallel"))]
    {
        let n_workers = match_window_cap();
        let iter = std::sync::Mutex::new(iter);
        let (tx, rx) = std::sync::mpsc::sync_channel::<
            Result<(BlockData, HashMap<OutPoint, OwnedOutput>)>,
        >(n_workers * 2);
        let interrupted = std::thread::scope(|s| -> Result<bool> {
            for _ in 0..n_workers {
                let iter = &iter;
                let tx = tx.clone();
                s.spawn(move || loop {
                    if scanner.should_interrupt() {
                        break;
                    }
                    // Hold the lock only to pull the next fetched block; matching
                    // (the expensive part) runs outside the lock.
                    let next = { iter.lock().expect("poisoned").next() };
                    match next {
                        Some(Ok(bd)) => {
                            let r = scanner.match_block_outputs(&bd).map(|outs| (bd, outs));
                            let stop = r.is_err();
                            if tx.send(r).is_err() || stop {
                                break;
                            }
                        }
                        // Fail fast on a fetch error (e.g. the oracle is still
                        // syncing and cannot serve the range).
                        Some(Err(e)) => {
                            let _ = tx.send(Err(e));
                            break;
                        }
                        None => break,
                    }
                });
            }
            drop(tx);
            for msg in rx {
                let (blockdata, outs) = msg?;
                commit_block(
                    scanner,
                    start,
                    end,
                    start_u32,
                    end_u32,
                    blockdata,
                    outs,
                    &mut owned,
                    &mut done,
                    &mut hashes,
                    &mut recv_tip,
                    &mut last_progress,
                    &mut notified_any,
                    &mut last_checkpoint,
                )?;
                if scanner.should_interrupt() {
                    scanner.save_state()?;
                    return Ok(true);
                }
            }
            Ok(false)
        })?;
        if interrupted {
            return Ok(owned);
        }
    }

    #[cfg(not(all(not(target_arch = "wasm32"), feature = "parallel")))]
    {
        let mut iter = iter;
        loop {
            if scanner.should_interrupt() {
                scanner.save_state()?;
                return Ok(owned);
            }
            match iter.next() {
                Some(Ok(blockdata)) => {
                    let outs = scanner.match_block_outputs(&blockdata)?;
                    commit_block(
                        scanner,
                        start,
                        end,
                        start_u32,
                        end_u32,
                        blockdata,
                        outs,
                        &mut owned,
                        &mut done,
                        &mut hashes,
                        &mut recv_tip,
                        &mut last_progress,
                        &mut notified_any,
                        &mut last_checkpoint,
                    )?;
                }
                Some(Err(e)) => return Err(e),
                None => break,
            }
        }
    }

    // The receive frontier must reach `end`; a short stream means a missing block.
    if recv_tip != Some(end_u32) {
        return Err(crate::spdk_core::error::Error::MissingBlockHash(end_u32));
    }
    let end_hash =
        hashes[len - 1].ok_or(crate::spdk_core::error::Error::MissingBlockHash(end_u32))?;
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
    // Notify progress every 1000 blocks in the spend sweep.
    let mut last_progress = spend_start.saturating_sub(1);
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
        if h - last_progress >= 1000 {
            scanner.record_progress(start, height, end)?;
            last_progress = h;
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
