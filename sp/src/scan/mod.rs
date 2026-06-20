#[cfg(feature = "scan-profile")]
pub mod profiling;
pub mod state;

use std::{
    collections::{HashMap, HashSet},
    ops::RangeInclusive,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::Instant,
};

use bitcoin::{
    absolute::Height,
    bip158::BlockFilter,
    hashes::{sha256, Hash},
    secp256k1::PublicKey,
    Amount, BlockHash, OutPoint, Txid, XOnlyPublicKey,
};

use crate::{
    account::coin_store::SpCoinStore,
    blindbit,
    core::receiving::Label,
    profile::SpStorageProfile,
    receiver::{self, BlockData, FilterData, OutputSpendStatus, OwnedOutput, SpReceiver, UtxoData},
    scan::state::ScanState,
    thread_pool::ThreadPool,
    SpNotification,
};

/// Progress callbacks invoked during a scan: one per fetched block, and one per
/// height while sweeping spent filters.
pub type BlockDataObserver = Arc<dyn Fn(&BlockData) + Send + Sync>;
pub type HeightObserver = Arc<dyn Fn(Height) + Send + Sync>;

const CONCURRENT_FILTER_REQUESTS: usize = 64;
const BLOCK_CHANNEL_CAPACITY: usize = 64;

pub(crate) fn fetch_concurrency() -> usize {
    static CACHE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        std::env::var("BWK_SP_FETCH_CONCURRENCY")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(CONCURRENT_FILTER_REQUESTS)
    })
}

fn fetch_channel_cap() -> usize {
    static CACHE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        std::env::var("BWK_SP_FETCH_CHANNEL_CAP")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(BLOCK_CHANNEL_CAPACITY)
    })
}

/// Tweaks per batched native candidate-derivation call. Each chunk is one FFI
/// call; a block's tweaks split into chunks processed sequentially (the scan
/// parallelizes across blocks in the match window, not across a block's chunks).
/// Matches the native primitive's internal tweak-chunk size so each call is one
/// chunk.
const CANDIDATE_TWEAK_CHUNK: usize = 32;

/// Tweak-chunk size, overridable via `BWK_SP_CANDIDATE_CHUNK` for tuning the
/// batch granularity. Only changes how a block's tweaks split into batched
/// calls; the native primitive's internal chunk (`SP_BATCH_TWEAK_CHUNK`) is
/// compile-time. Defaults to the const above.
fn candidate_tweak_chunk() -> usize {
    static CACHE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        std::env::var("BWK_SP_CANDIDATE_CHUNK")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(CANDIDATE_TWEAK_CHUNK)
    })
}

pub(crate) fn script_to_secret_map(
    sp_receiver: &SpReceiver,
    tweak_data_vec: Vec<PublicKey>,
) -> Result<HashMap<[u8; 34], PublicKey>, receiver::error::Error> {
    let b_scan = &sp_receiver.get_scan_key();

    // ECDH per tweak, then SPK derivation. Only reached on a filter match;
    // sequential within the block, as the scan parallelizes across blocks.
    let shared_secrets: Vec<PublicKey> = tweak_data_vec
        .into_iter()
        .map(|tweak| crate::core::receiving::calculate_ecdh_shared_secret(&tweak, b_scan))
        .collect();

    let items: Result<Vec<_>, receiver::error::Error> = shared_secrets
        .into_iter()
        .map(|secret| {
            let spks = sp_receiver.receiver.get_spks_from_shared_secret(&secret)?;
            Ok((secret, spks.into_values()))
        })
        .collect();

    let mut res = HashMap::new();
    for (secret, spks) in items? {
        for spk in spks {
            res.insert(spk, secret);
        }
    }
    Ok(res)
}

/// The candidate output spks for a batch of tweaks, derived in one native
/// call per tweak (vartime ECDH plus `k = 0` candidate derivation, no FFI
/// round-trips). The spend points are constant across tweaks, so they are
/// computed once and reused. This is the GCS-filter membership set; on a
/// match the caller recovers the shared secrets via [`script_to_secret_map`].
pub(crate) fn candidate_spks(
    sp_receiver: &SpReceiver,
    tweaks: &[PublicKey],
) -> Result<Vec<[u8; 34]>, receiver::error::Error> {
    let scan_key = sp_receiver.get_scan_key();
    let spend_points = sp_receiver.receiver.candidate_spend_points()?;

    #[cfg(feature = "scan-profile")]
    let __t = std::time::Instant::now();
    // One batched native call per chunk of tweaks. Sequential within the
    // block; the scan parallelizes across blocks in the match window.
    let spks: Result<Vec<Vec<Vec<[u8; 34]>>>, receiver::error::Error> = tweaks
        .chunks(candidate_tweak_chunk())
        .map(|chunk| {
            sp_receiver
                .receiver
                .candidate_output_spks_batch(chunk, &scan_key, &spend_points)
                .map_err(Into::into)
        })
        .collect();
    #[cfg(feature = "scan-profile")]
    profiling::add(&profiling::CANDIDATES_NS, __t.elapsed());

    Ok(spks?.into_iter().flatten().flatten().collect())
}

/// Fetch tweaks + new-utxo filter for every height in `range`, fanning out over
/// a bounded worker pool and streaming `BlockData` into `sender` in completion
/// order. Non-blocking: queues the heights, spawns the pool, and returns
/// immediately. The bounded channel applies backpressure, so fetching stays at
/// most `fetch_channel_cap()` blocks ahead of the consumer.
pub fn fetch_blocks<P: SpStorageProfile>(
    sender: mpsc::SyncSender<std::result::Result<BlockData, receiver::error::Error>>,
    backend: &BackendContext,
    scan: &ScanContext<P>,
) {
    // Height 0 has no block to fetch; clamp the start to 1.
    let start = scan.start.to_consensus_u32().max(1);
    let end = scan.end.to_consensus_u32();
    spawn_block_fetchers(
        backend.agent.clone(),
        backend.url.clone(),
        start..=end,
        backend.dust_limit,
        backend.with_cutthrough,
        sender,
        scan.block_data_observer.clone(),
        ThreadPool::new(fetch_concurrency()),
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_block_fetchers(
    agent: Arc<ureq::Agent>,
    url: String,
    range: RangeInclusive<u32>,
    dust_limit: Option<Amount>,
    with_cutthrough: bool,
    sender: mpsc::SyncSender<std::result::Result<BlockData, receiver::error::Error>>,
    block_data_observer: Option<BlockDataObserver>,
    pool: ThreadPool,
) {
    for height in range {
        let agent = agent.clone();
        let url = url.clone();
        let sender = sender.clone();
        let block_data_observer = block_data_observer.clone();
        pool.execute(move || {
            fetch_block_data_for_height(
                agent,
                url,
                height,
                dust_limit,
                with_cutthrough,
                sender,
                block_data_observer,
            );
        });
    }
}

fn fetch_block_data_for_height(
    agent: Arc<ureq::Agent>,
    url: String,
    height: u32,
    dust_limit: Option<Amount>,
    with_cutthrough: bool,
    sender: mpsc::SyncSender<std::result::Result<BlockData, receiver::error::Error>>,
    block_data_observer: Option<BlockDataObserver>,
) {
    let blkheight = match Height::from_consensus(height) {
        Ok(bh) => bh,
        Err(e) => {
            let _ = sender.send(Err(receiver::error::Error::from(e)));
            return;
        }
    };
    let tweaks = match with_cutthrough {
        true => blindbit::tweaks(&agent, &url, blkheight, dust_limit),
        false => blindbit::tweak_index(&agent, &url, blkheight, dust_limit),
    };
    let tweaks = match tweaks {
        Ok(t) => t,
        Err(e) => {
            let _ = sender.send(Err(receiver::error::Error::from(e)));
            return;
        }
    };
    let new_utxo_filter = match blindbit::filter_new_utxos(&agent, &url, blkheight) {
        Ok(f) => f,
        Err(e) => {
            let _ = sender.send(Err(receiver::error::Error::from(e)));
            return;
        }
    };
    let blkhash = new_utxo_filter.block_hash;
    let block_data = BlockData {
        blkheight,
        blkhash,
        tweaks,
        new_utxo_filter: new_utxo_filter.into(),
    };
    if let Some(observer) = &block_data_observer {
        observer(&block_data);
    }
    let _ = sender.send(Ok(block_data));
}

pub struct ScanStores<P: SpStorageProfile> {
    pub coin_store: Arc<Mutex<SpCoinStore<P>>>,
    pub tx_store: Arc<Mutex<crate::account::tx_store::SpTxStore<P>>>,
    pub scan_state: Arc<Mutex<ScanState>>,
    pub sender: mpsc::Sender<crate::Notification>,
}

/// Connection + fetch config for the blindbit backend. Grouped so the scan
/// pipeline takes one backend handle; later this can be made generic over the
/// transport.
struct BackendContext {
    agent: Arc<ureq::Agent>,
    url: String,
    dust_limit: Option<Amount>,
    with_cutthrough: bool,
}

/// Wallet-side handles and the height range for a scan.
struct ScanContext<'a, P: SpStorageProfile> {
    sp_receiver: &'a SpReceiver,
    stores: &'a ScanStores<P>,
    stop: &'a Arc<AtomicBool>,
    start: Height,
    end: Height,
    block_data_observer: Option<BlockDataObserver>,
    owned: HashSet<OutPoint>,
}

pub fn scan_blocks<P: SpStorageProfile>(
    agent: Arc<ureq::Agent>,
    blindbit_url: &str,
    sp_receiver: &SpReceiver,
    stores: &ScanStores<P>,
    stop: &Arc<AtomicBool>,
    start: Height,
    end: Height,
    dust_limit: Option<Amount>,
    with_cutthrough: bool,
) -> std::result::Result<(), receiver::error::Error> {
    scan_blocks_with_observer(
        agent,
        blindbit_url,
        sp_receiver,
        stores,
        stop,
        start,
        end,
        dust_limit,
        with_cutthrough,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn scan_blocks_with_observer<P: SpStorageProfile>(
    agent: Arc<ureq::Agent>,
    blindbit_url: &str,
    sp_receiver: &SpReceiver,
    stores: &ScanStores<P>,
    stop: &Arc<AtomicBool>,
    start: Height,
    end: Height,
    dust_limit: Option<Amount>,
    with_cutthrough: bool,
    block_data_observer: Option<BlockDataObserver>,
) -> std::result::Result<(), receiver::error::Error> {
    if start > end {
        return Err(receiver::error::Error::InvalidRange(
            start.to_consensus_u32(),
            end.to_consensus_u32(),
        ));
    }

    log::info!("start: {} end: {}", start, end);
    let start_time = Instant::now();
    let owned = stores.coin_store.lock().expect("poisoned").all_outpoints();
    let backend = BackendContext {
        agent,
        url: blindbit_url.to_string(),
        dust_limit,
        with_cutthrough,
    };
    let mut scan = ScanContext {
        sp_receiver,
        stores,
        stop,
        start,
        end,
        block_data_observer,
        owned,
    };
    process_scan(&backend, &mut scan)?;
    log::info!(
        "Blindbit scan completed in {} seconds",
        start_time.elapsed().as_secs()
    );
    Ok(())
}

fn should_interrupt(stop: &Arc<AtomicBool>) -> bool {
    stop.load(Ordering::Relaxed)
}

fn save_state<P: SpStorageProfile>(
    stores: &ScanStores<P>,
) -> std::result::Result<(), receiver::error::Error> {
    stores.coin_store.lock().expect("poisoned").persist();
    stores.tx_store.lock().expect("poisoned").persist();
    stores.scan_state.lock().expect("poisoned").persist();
    Ok(())
}

fn record_outputs<P: SpStorageProfile>(
    stores: &ScanStores<P>,
    outputs: HashMap<OutPoint, OwnedOutput>,
) -> std::result::Result<(), receiver::error::Error> {
    let mut store = stores.coin_store.lock().expect("poisoned");
    for (outpoint, output) in outputs {
        store.insert(outpoint, output);
        let _ = stores
            .sender
            .send(crate::Notification::Sp(SpNotification::NewOutput(outpoint)));
    }
    store.persist();
    Ok(())
}

fn record_inputs<P: SpStorageProfile>(
    stores: &ScanStores<P>,
    block_hash: BlockHash,
    inputs: HashSet<OutPoint>,
) -> std::result::Result<(), receiver::error::Error> {
    let mut store = stores.coin_store.lock().expect("poisoned");
    for outpoint in inputs {
        store.mark_mined(&outpoint, *block_hash.as_byte_array());
        let _ = stores
            .sender
            .send(crate::Notification::Sp(SpNotification::OutputSpent(
                outpoint,
            )));
    }
    store.persist();
    Ok(())
}

fn record_progress<P: SpStorageProfile>(
    stores: &ScanStores<P>,
    current: Height,
    end: Height,
) -> std::result::Result<(), receiver::error::Error> {
    let _ = stores
        .sender
        .send(crate::Notification::Sp(SpNotification::ScanProgress {
            current: current.to_consensus_u32(),
            end: end.to_consensus_u32(),
        }));
    Ok(())
}

fn record_scan_frontier<P: SpStorageProfile>(
    stores: &ScanStores<P>,
    height: Height,
    block_hash: BlockHash,
) -> std::result::Result<(), receiver::error::Error> {
    let mut state = stores.scan_state.lock().expect("poisoned");
    state.advance_frontier(height.to_consensus_u32(), *block_hash.as_byte_array());
    state.persist();
    Ok(())
}

fn record_spend_frontier<P: SpStorageProfile>(
    stores: &ScanStores<P>,
    height: Height,
) -> std::result::Result<(), receiver::error::Error> {
    let mut state = stores.scan_state.lock().expect("poisoned");
    state.advance_spend_frontier(height.to_consensus_u32());
    state.persist();
    Ok(())
}

fn spend_frontier<P: SpStorageProfile>(
    stores: &ScanStores<P>,
) -> std::result::Result<Option<u32>, receiver::error::Error> {
    Ok(stores
        .scan_state
        .lock()
        .expect("poisoned")
        .last_spend_height())
}

fn scan_utxos(
    backend: &BackendContext,
    sp_receiver: &SpReceiver,
    blkheight: Height,
    secrets_map: HashMap<[u8; 34], bitcoin::secp256k1::PublicKey>,
) -> std::result::Result<
    Vec<(Option<Label>, UtxoData, bitcoin::secp256k1::Scalar)>,
    receiver::error::Error,
> {
    let utxos = blindbit::utxos(&backend.agent, &backend.url, blkheight)?;
    let mut txmap: HashMap<Txid, Vec<UtxoData>> = HashMap::new();
    for utxo in utxos {
        txmap.entry(utxo.txid).or_default().push(utxo);
    }

    let mut result = Vec::new();
    for utxos in txmap.into_values() {
        let mut secret = None;
        for utxo in utxos.iter() {
            let spk = utxo.scriptpubkey.as_bytes();
            if let Some(s) = secrets_map.get(spk) {
                secret = Some(s);
                break;
            }
        }
        let secret = match secret {
            Some(secret) => secret,
            None => continue,
        };

        let output_keys: std::result::Result<Vec<XOnlyPublicKey>, receiver::error::Error> = utxos
            .iter()
            .filter_map(|x| {
                if x.scriptpubkey.is_p2tr() {
                    Some(
                        XOnlyPublicKey::from_slice(&x.scriptpubkey.as_bytes()[2..])
                            .map_err(receiver::error::Error::from),
                    )
                } else {
                    None
                }
            })
            .collect();

        let ours = sp_receiver
            .receiver
            .scan_transaction(secret, output_keys?)?;

        for utxo in utxos {
            if !utxo.scriptpubkey.is_p2tr() || utxo.spent {
                continue;
            }
            let xonly = XOnlyPublicKey::from_slice(&utxo.scriptpubkey.as_bytes()[2..])?;
            for (label, map) in ours.iter() {
                if let Some(scalar) = map.get(&xonly) {
                    result.push((label.clone(), utxo, *scalar));
                    break;
                }
            }
        }
    }
    Ok(result)
}

fn check_block_outputs(
    created_utxo_filter: BlockFilter,
    blkhash: BlockHash,
    candidate_spks: Vec<&[u8; 34]>,
) -> std::result::Result<bool, receiver::error::Error> {
    let output_keys: Vec<_> = candidate_spks
        .into_iter()
        .map(|spk| spk[2..].as_ref())
        .collect();
    if !output_keys.is_empty() {
        Ok(created_utxo_filter.match_any(&blkhash, &mut output_keys.into_iter())?)
    } else {
        Ok(false)
    }
}

fn match_block_outputs(
    backend: &BackendContext,
    sp_receiver: &SpReceiver,
    blockdata: &BlockData,
) -> std::result::Result<HashMap<OutPoint, OwnedOutput>, receiver::error::Error> {
    process_block_outputs(
        backend,
        sp_receiver,
        blockdata.blkheight,
        &blockdata.tweaks,
        blockdata.new_utxo_filter.clone(),
    )
}

fn process_block_outputs(
    backend: &BackendContext,
    sp_receiver: &SpReceiver,
    blkheight: Height,
    tweaks: &[[u8; 33]],
    new_utxo_filter: FilterData,
) -> std::result::Result<HashMap<OutPoint, OwnedOutput>, receiver::error::Error> {
    let mut res = HashMap::new();
    if tweaks.is_empty() {
        return Ok(res);
    }

    let tweaks: Vec<bitcoin::secp256k1::PublicKey> = tweaks
        .iter()
        .map(|t| bitcoin::secp256k1::PublicKey::from_slice(t))
        .collect::<std::result::Result<_, _>>()?;
    let candidate_spks = candidate_spks(sp_receiver, &tweaks)?;
    let candidate_spks: Vec<&[u8; 34]> = candidate_spks.iter().collect();

    #[cfg(feature = "scan-profile")]
    let __t = std::time::Instant::now();
    let blkfilter = BlockFilter::new(&new_utxo_filter.data);
    let blkhash = new_utxo_filter.block_hash;
    let matched_outputs = check_block_outputs(blkfilter, blkhash, candidate_spks)?;
    #[cfg(feature = "scan-profile")]
    profiling::add(&profiling::OUTPUT_FILTER_NS, __t.elapsed());

    if !matched_outputs {
        return Ok(res);
    }

    log::info!("matched outputs on: {}", blkheight);
    let secrets_map = script_to_secret_map(sp_receiver, tweaks)?;
    #[cfg(feature = "scan-profile")]
    let __t = std::time::Instant::now();
    let found = scan_utxos(backend, sp_receiver, blkheight, secrets_map)?;
    #[cfg(feature = "scan-profile")]
    profiling::add(&profiling::SCAN_UTXOS_NS, __t.elapsed());

    for (label, utxo, tweak) in found {
        let outpoint = OutPoint {
            txid: utxo.txid,
            vout: utxo.vout,
        };
        let out = OwnedOutput {
            blockheight: blkheight,
            tweak: tweak.to_be_bytes(),
            amount: utxo.value,
            script: utxo.scriptpubkey,
            label,
            spend_status: OutputSpendStatus::Unspent,
        };
        res.insert(outpoint, out);
    }
    Ok(res)
}

fn input_hashes_for(
    blkhash: BlockHash,
    owned: &HashSet<OutPoint>,
) -> std::result::Result<HashMap<[u8; 8], OutPoint>, receiver::error::Error> {
    let mut map = HashMap::new();
    for outpoint in owned {
        let mut arr = [0u8; 68];
        arr[..32].copy_from_slice(outpoint.txid.to_raw_hash().as_byte_array());
        arr[32..36].copy_from_slice(&outpoint.vout.to_le_bytes());
        arr[36..].copy_from_slice(&blkhash.to_byte_array());
        let hash = sha256::Hash::hash(&arr);
        let mut res = [0u8; 8];
        res.copy_from_slice(&hash[..8]);
        map.insert(res, *outpoint);
    }
    Ok(map)
}

fn check_block_inputs(
    spent_filter: BlockFilter,
    blkhash: BlockHash,
    input_hashes: Vec<[u8; 8]>,
) -> std::result::Result<bool, receiver::error::Error> {
    if !input_hashes.is_empty() {
        Ok(spent_filter.match_any(&blkhash, &mut input_hashes.into_iter())?)
    } else {
        Ok(false)
    }
}

fn match_inputs_for(
    backend: &BackendContext,
    blkheight: Height,
    spent_filter: FilterData,
    owned: &HashSet<OutPoint>,
) -> std::result::Result<HashSet<OutPoint>, receiver::error::Error> {
    let mut res = HashSet::new();
    let blkhash = spent_filter.block_hash;
    let input_hashes_map = input_hashes_for(blkhash, owned)?;

    let blkfilter = BlockFilter::new(&spent_filter.data);
    let matched_inputs = check_block_inputs(
        blkfilter,
        blkhash,
        input_hashes_map.keys().cloned().collect(),
    )?;
    if matched_inputs {
        let spent = blindbit::spent_index(&backend.agent, &backend.url, blkheight)?.data;
        for spent in spent {
            let hex: &[u8] = spent.as_ref();
            if let Some(outpoint) = input_hashes_map.get(hex) {
                res.insert(*outpoint);
            }
        }
    }
    Ok(res)
}

#[cfg(all(not(target_arch = "wasm32"), feature = "parallel"))]
const MATCH_WINDOW_MAX: usize = 64;

#[cfg(all(not(target_arch = "wasm32"), feature = "parallel"))]
fn match_window_cap() -> usize {
    static CACHE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
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
    })
}

#[allow(clippy::too_many_arguments)]
fn commit_block<P: SpStorageProfile>(
    scan: &mut ScanContext<P>,
    start_u32: u32,
    end_u32: u32,
    blockdata: BlockData,
    outs: HashMap<OutPoint, OwnedOutput>,
    done: &mut [bool],
    hashes: &mut [Option<BlockHash>],
    recv_tip: &mut Option<u32>,
    last_progress: &mut u32,
    notified_any: &mut bool,
    last_checkpoint: &mut std::time::Instant,
) -> std::result::Result<(), receiver::error::Error> {
    let blkheight = blockdata.blkheight;
    let blkhash = blockdata.blkhash;
    let idx = (blkheight.to_consensus_u32() - start_u32) as usize;
    done[idx] = true;
    hashes[idx] = Some(blkhash);
    if !outs.is_empty() {
        for outpoint in outs.keys() {
            scan.owned.insert(*outpoint);
        }
        record_outputs(scan.stores, outs)?;
    }

    let mut next = recv_tip.map(|h| h + 1).unwrap_or(start_u32);
    while next <= end_u32 && done[(next - start_u32) as usize] {
        *recv_tip = Some(next);
        next += 1;
    }

    if let Some(tip) = *recv_tip {
        if !*notified_any || tip.saturating_sub(*last_progress) >= 100 {
            record_progress(scan.stores, Height::from_consensus(tip)?, scan.end)?;
            *last_progress = tip;
            *notified_any = true;
        }
    }

    if last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
        if let Some(tip) = *recv_tip {
            let i = (tip - start_u32) as usize;
            let hash = hashes[i].ok_or(receiver::error::Error::MissingBlockHash(tip))?;
            record_scan_frontier(scan.stores, Height::from_consensus(tip)?, hash)?;
            save_state(scan.stores)?;
        }
        *last_checkpoint = std::time::Instant::now();
    }
    let _ = scan.start;
    Ok(())
}

const CHECKPOINT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Consume `BlockData` from `receiver` (produced by [`fetch_blocks`]), derive the
/// owned outputs of each block, and commit them in height order. Returns `true`
/// if the scan was interrupted via `stop` (state already persisted); on normal
/// completion it records the receive-scan frontier and returns `false`.
fn process_blocks<P: SpStorageProfile>(
    backend: &BackendContext,
    scan: &mut ScanContext<P>,
    receiver: mpsc::Receiver<std::result::Result<BlockData, receiver::error::Error>>,
) -> std::result::Result<bool, receiver::error::Error> {
    let start_u32 = scan.start.to_consensus_u32();
    let end_u32 = scan.end.to_consensus_u32();
    let len = (end_u32 - start_u32 + 1) as usize;

    let mut done = vec![false; len];
    let mut hashes: Vec<Option<BlockHash>> = vec![None; len];
    let mut recv_tip: Option<u32> = None;
    let mut last_progress = start_u32.saturating_sub(1);
    let mut notified_any = false;
    let mut last_checkpoint = Instant::now();

    #[cfg(all(not(target_arch = "wasm32"), feature = "parallel"))]
    {
        // Copy the `&'a` handles out so the scoped workers don't borrow `scan`,
        // leaving the main loop free to hold `&mut scan` for `scan.owned`.
        let sp_receiver = scan.sp_receiver;
        let stop = scan.stop;
        let n_workers = match_window_cap();
        let receiver = std::sync::Mutex::new(receiver);
        let (tx, rx) = std::sync::mpsc::sync_channel::<
            std::result::Result<
                (BlockData, HashMap<OutPoint, OwnedOutput>),
                receiver::error::Error,
            >,
        >(n_workers * 2);
        let interrupted =
            std::thread::scope(|s| -> std::result::Result<bool, receiver::error::Error> {
                for _ in 0..n_workers {
                    let receiver = &receiver;
                    let tx = tx.clone();
                    s.spawn(move || loop {
                        if should_interrupt(stop) {
                            break;
                        }
                        let next = { receiver.lock().expect("poisoned").recv() };
                        match next {
                            Ok(Ok(bd)) => {
                                let r = match_block_outputs(backend, sp_receiver, &bd)
                                    .map(|outs| (bd, outs));
                                let stop = r.is_err();
                                if tx.send(r).is_err() || stop {
                                    break;
                                }
                            }
                            Ok(Err(e)) => {
                                let _ = tx.send(Err(e));
                                break;
                            }
                            Err(_) => break,
                        }
                    });
                }
                drop(tx);
                for msg in rx {
                    let (blockdata, outs) = msg?;
                    commit_block(
                        scan,
                        start_u32,
                        end_u32,
                        blockdata,
                        outs,
                        &mut done,
                        &mut hashes,
                        &mut recv_tip,
                        &mut last_progress,
                        &mut notified_any,
                        &mut last_checkpoint,
                    )?;
                    if should_interrupt(stop) {
                        save_state(scan.stores)?;
                        return Ok(true);
                    }
                }
                Ok(false)
            })?;
        if interrupted {
            return Ok(true);
        }
    }

    #[cfg(not(all(not(target_arch = "wasm32"), feature = "parallel")))]
    {
        loop {
            if should_interrupt(scan.stop) {
                save_state(scan.stores)?;
                return Ok(true);
            }
            match receiver.recv() {
                Ok(Ok(blockdata)) => {
                    let outs = match_block_outputs(backend, scan.sp_receiver, &blockdata)?;
                    commit_block(
                        scan,
                        start_u32,
                        end_u32,
                        blockdata,
                        outs,
                        &mut done,
                        &mut hashes,
                        &mut recv_tip,
                        &mut last_progress,
                        &mut notified_any,
                        &mut last_checkpoint,
                    )?;
                }
                Ok(Err(e)) => return Err(e),
                Err(_) => break,
            }
        }
    }

    if recv_tip != Some(end_u32) {
        return Err(receiver::error::Error::MissingBlockHash(end_u32));
    }
    let end_hash = hashes[len - 1].ok_or(receiver::error::Error::MissingBlockHash(end_u32))?;
    record_scan_frontier(scan.stores, scan.end, end_hash)?;
    record_progress(scan.stores, scan.end, scan.end)?;
    save_state(scan.stores)?;
    Ok(false)
}

/// Sweep `[spend_frontier+1, end]` for inputs spending any still-owned output:
/// remove those outputs, record the spends, and advance the spend frontier
/// (checkpointing periodically), persisting it at `end` on completion. Returns
/// early with state persisted if interrupted via `stop` or once nothing remains
/// owned.
fn process_spends<P: SpStorageProfile>(
    backend: &BackendContext,
    scan: &mut ScanContext<P>,
) -> std::result::Result<(), receiver::error::Error> {
    let start_u32 = scan.start.to_consensus_u32();
    let end_u32 = scan.end.to_consensus_u32();
    let spend_start = spend_frontier(scan.stores)?
        .map(|h| h + 1)
        .unwrap_or(start_u32);
    let mut last_checkpoint = Instant::now();
    let mut last_progress = spend_start.saturating_sub(1);
    for h in spend_start..=end_u32 {
        if should_interrupt(scan.stop) {
            save_state(scan.stores)?;
            return Ok(());
        }
        if scan.owned.is_empty() {
            break;
        }
        let height = Height::from_consensus(h)?;
        let spent_filter = blindbit::spent_filter(&backend.agent, &backend.url, height, None)?;
        let blkhash = spent_filter.block_hash;
        let ins = match_inputs_for(backend, height, spent_filter, &scan.owned)?;
        if !ins.is_empty() {
            for outpoint in &ins {
                scan.owned.remove(outpoint);
            }
            record_inputs(scan.stores, blkhash, ins)?;
        }
        if h - last_progress >= 1000 {
            record_progress(scan.stores, height, scan.end)?;
            last_progress = h;
        }
        if last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
            record_spend_frontier(scan.stores, height)?;
            save_state(scan.stores)?;
            last_checkpoint = Instant::now();
        }
    }
    record_spend_frontier(scan.stores, scan.end)?;
    save_state(scan.stores)?;
    Ok(())
}

fn process_scan<P: SpStorageProfile>(
    backend: &BackendContext,
    scan: &mut ScanContext<P>,
) -> std::result::Result<(), receiver::error::Error> {
    let start_u32 = scan.start.to_consensus_u32();

    if spend_frontier(scan.stores)?.is_none() {
        if let Some(floor) = start_u32.checked_sub(1) {
            record_spend_frontier(scan.stores, Height::from_consensus(floor)?)?;
            save_state(scan.stores)?;
        }
    }

    let (sender, receiver) = mpsc::sync_channel(fetch_channel_cap());
    fetch_blocks(sender, backend, scan);

    if process_blocks(backend, scan, receiver)? {
        // Interrupted mid-scan; process_blocks already persisted state.
        return Ok(());
    }

    process_spends(backend, scan)?;
    Ok(())
}

// Account scan-orchestration methods.
//
// These stay methods on [`Account`] (public API unchanged); only their
// source location lives here, next to the free scanning functions they
// drive. Gated on `mnemonic` alongside the `account` module that defines
// [`Account`].
#[cfg(feature = "mnemonic")]
use {
    crate::{
        account::{AccountError, ScanMode},
        Notification,
    },
    std::{thread, time::Duration},
};

#[cfg(feature = "mnemonic")]
impl<P: SpStorageProfile> crate::account::Account<P> {
    /// Start a scan with the specified mode.
    ///
    /// # Arguments
    /// * `mode` - The scanning mode (OneShot or Continuous)
    ///
    /// # Modes
    /// - `OneShot`: Synchronous scan from last position to current chain tip, then returns.
    ///   If already at tip, returns immediately without scanning.
    /// - `Continuous`: Spawns a background thread that scans to tip, then watches for new blocks.
    ///   Returns immediately after spawning. Use `stop_scan()` to stop.
    ///
    /// # Errors
    /// - `AccountError::ScannerAlreadyRunning` if continuous scan is already active
    /// - `AccountError::Scan` if scan fails
    pub fn start_scan(&mut self, mode: ScanMode) -> Result<(), AccountError> {
        match mode {
            ScanMode::OneShot => self.scan_oneshot(),
            ScanMode::Continuous => self.start_continuous_scan(),
        }
    }

    /// Internal: Execute one-shot scan to current chain tip.
    fn scan_oneshot(&mut self) -> Result<(), AccountError> {
        // Clear any stale cancel signal from a previous run before we hand
        // the flag down to the scanner. Without this, a caller that flipped
        // the flag via `cancel_flag()` for a prior scan would cause the next
        // OneShot to bail at the first block (spdk-core's `process_blocks`
        // returns Ok early when `should_interrupt()` is true).
        self.scanner_stop.store(false, Ordering::Relaxed);

        let start_height = self.scan_state.lock().expect("poisoned").next_scan_start();
        let end_height = self.block_height()?;

        if start_height > end_height {
            return Ok(()); // Already at tip, nothing to scan
        }

        let start = Height::from_consensus(start_height)
            .map_err(|e| AccountError::Scan(format!("invalid start height: {e}")))?;
        let end = Height::from_consensus(end_height)
            .map_err(|e| AccountError::Scan(format!("invalid end height: {e}")))?;

        let dust_limit = self.config.dust_limit.map(Amount::from_sat);

        let with_cutthrough = blindbit::info(&self.agent, &self.config.blindbit_url)
            .map(|info| info.tweaks_cut_through_with_dust_filter)
            .unwrap_or(false);

        let stores = ScanStores {
            coin_store: self.coin_store.clone(),
            tx_store: self.tx_store.clone(),
            scan_state: self.scan_state.clone(),
            sender: self.sender.clone(),
        };

        let _ = self
            .sender
            .send(Notification::Sp(SpNotification::ScanStarted {
                start: start_height,
                end: end_height,
            }));

        scan_blocks(
            self.agent.clone(),
            &self.config.blindbit_url,
            &self.sp_receiver,
            &stores,
            &self.scanner_stop,
            start,
            end,
            dust_limit,
            with_cutthrough,
        )
        .map_err(|e| AccountError::Scan(e.to_string()))?;

        let _ = self
            .sender
            .send(Notification::Sp(SpNotification::ScanCompleted));

        Ok(())
    }

    /// Internal: Start continuous scan in background thread.
    fn start_continuous_scan(&mut self) -> Result<(), AccountError> {
        if self.scanner_handle.is_some() {
            return Err(AccountError::ScannerAlreadyRunning);
        }

        self.scanner_stop.store(false, Ordering::Relaxed);
        let _ = self
            .sender
            .send(Notification::Sp(SpNotification::StartingScan));

        let sp_receiver = self.sp_receiver.clone();
        let agent = self.agent.clone();
        let blindbit_url = self.config.blindbit_url.clone();
        let dust_limit = self.config.dust_limit.map(Amount::from_sat);
        let coin_store = self.coin_store.clone();
        let tx_store = self.tx_store.clone();
        let scan_state = self.scan_state.clone();
        let sender = self.sender.clone();
        let stop = self.scanner_stop.clone();

        let handle = thread::spawn(move || {
            let mut last_notified_tip: Option<u32> = None;
            let mut waiting = false;

            let with_cutthrough = blindbit::info(&agent, &blindbit_url)
                .map(|info| info.tweaks_cut_through_with_dust_filter)
                .unwrap_or(false);

            let stores = ScanStores {
                coin_store: coin_store.clone(),
                tx_store: tx_store.clone(),
                scan_state: scan_state.clone(),
                sender: sender.clone(),
            };

            while !stop.load(Ordering::Relaxed) {
                let chain_height = match blindbit::block_height(&agent, &blindbit_url) {
                    Ok(h) => h.to_consensus_u32(),
                    Err(e) => {
                        log::warn!("scanner: failed to get block height: {e}");
                        let _ = sender.send(Notification::Sp(SpNotification::FailStartScanning {
                            message: e.to_string(),
                        }));
                        break;
                    }
                };

                let start_height = scan_state.lock().expect("poisoned").next_scan_start();

                if start_height > chain_height {
                    if !waiting {
                        let _ = sender.send(Notification::Sp(SpNotification::WaitingForBlocks {
                            tip_height: chain_height,
                        }));
                        waiting = true;
                    }
                    thread::sleep(Duration::from_secs(2));
                    continue;
                }

                waiting = false;

                // New blocks detected - notify if we were previously waiting
                if let Some(prev_tip) = last_notified_tip {
                    if chain_height > prev_tip {
                        let _ = sender.send(Notification::Sp(SpNotification::NewBlocksDetected {
                            from_height: prev_tip,
                            to_height: chain_height,
                        }));
                    }
                }

                let start = match Height::from_consensus(start_height) {
                    Ok(h) => h,
                    Err(_) => continue,
                };
                let end = match Height::from_consensus(chain_height) {
                    Ok(h) => h,
                    Err(_) => continue,
                };

                let _ = sender.send(Notification::Sp(SpNotification::ScanStarted {
                    start: start.to_consensus_u32(),
                    end: end.to_consensus_u32(),
                }));

                match scan_blocks(
                    agent.clone(),
                    &blindbit_url,
                    &sp_receiver,
                    &stores,
                    &stop,
                    start,
                    end,
                    dust_limit,
                    with_cutthrough,
                ) {
                    Ok(()) => {
                        let _ = sender.send(Notification::Sp(SpNotification::ScanCompleted));
                        last_notified_tip = Some(chain_height);
                    }
                    Err(e) => {
                        let _ = sender.send(Notification::Sp(SpNotification::FailScan {
                            message: e.to_string(),
                        }));
                        break;
                    }
                }

                // Brief pause before checking for new blocks
                thread::sleep(Duration::from_millis(500));
            }

            let _ = sender.send(Notification::Sp(SpNotification::ScanStopped));
        });

        self.scanner_handle = Some(handle);
        Ok(())
    }

    /// Stop the continuous scan.
    ///
    /// No-op if not running in continuous mode.
    pub fn stop_scan(&mut self) {
        let _ = self
            .sender
            .send(Notification::Sp(SpNotification::StoppingScan));
        self.scanner_stop.store(true, Ordering::Relaxed);
        self.scanner_handle = None;
    }

    /// Check if a continuous scan is currently running.
    pub fn is_scanning(&self) -> bool {
        self.scanner_handle
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }

    /// Returns a clone of the scanner cancellation flag.
    ///
    /// Setting this `AtomicBool` to `true` causes any in-flight OneShot or
    /// Continuous scan to bail at the next per-block checkpoint inside
    /// spdk-core's `process_blocks` (which calls `should_interrupt()` before
    /// every block). The scan call returns `Ok(())` after persisting state
    /// , i.e. cancellation is graceful, not an error.
    ///
    /// `scan_oneshot` resets this flag to `false` at the start of each run,
    /// so leaving the flag in `true` between runs is harmless.
    ///
    /// Intended for consumers that hold an `Account` behind a `Mutex` and
    /// need to interrupt a scan without first re-acquiring the mutex (which
    /// the in-flight scan call still holds via `&mut self`).
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.scanner_stop.clone()
    }

    /// Scan a range of blocks for silent payment outputs.
    pub fn scan_blocks(
        &mut self,
        start: Option<u32>,
        end: Option<u32>,
    ) -> Result<(), AccountError> {
        // If both are None, use the new one-shot scan
        if start.is_none() && end.is_none() {
            return self.start_scan(ScanMode::OneShot);
        }

        // Custom range scan (legacy behavior)
        let start_height =
            start.unwrap_or_else(|| self.scan_state.lock().expect("poisoned").next_scan_start());
        let end_height = match end {
            Some(h) => h,
            None => self.block_height()?,
        };

        if start_height > end_height {
            return Ok(());
        }

        let start = Height::from_consensus(start_height)
            .map_err(|e| AccountError::Scan(format!("invalid start height: {e}")))?;
        let end = Height::from_consensus(end_height)
            .map_err(|e| AccountError::Scan(format!("invalid end height: {e}")))?;

        let dust_limit = self.config.dust_limit.map(Amount::from_sat);

        let with_cutthrough = blindbit::info(&self.agent, &self.config.blindbit_url)
            .map(|info| info.tweaks_cut_through_with_dust_filter)
            .unwrap_or(false);

        let stores = ScanStores {
            coin_store: self.coin_store.clone(),
            tx_store: self.tx_store.clone(),
            scan_state: self.scan_state.clone(),
            sender: self.sender.clone(),
        };

        scan_blocks(
            self.agent.clone(),
            &self.config.blindbit_url,
            &self.sp_receiver,
            &stores,
            &self.scanner_stop,
            start,
            end,
            dust_limit,
            with_cutthrough,
        )
        .map_err(|e| AccountError::Scan(e.to_string()))?;

        let _ = self
            .sender
            .send(Notification::Sp(SpNotification::ScanCompleted));
        Ok(())
    }

    /// Start a background scanner thread.
    pub fn start_scanner(&mut self) -> Result<(), AccountError> {
        self.start_scan(ScanMode::Continuous)
    }

    /// Stop the background scanner thread.
    pub fn stop_scanner(&mut self) {
        self.stop_scan()
    }

    /// Check if the scanner is currently running.
    pub fn scanner_running(&self) -> bool {
        self.is_scanning()
    }

    /// Returns the last scanned block height.
    pub fn last_scanned_height(&self) -> Option<u32> {
        self.scan_state
            .lock()
            .expect("poisoned")
            .last_scanned_height()
    }
}
