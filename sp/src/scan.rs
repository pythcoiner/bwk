use std::{
    collections::{HashMap, HashSet},
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
    Amount, BlockHash, OutPoint, Txid, XOnlyPublicKey,
};

use crate::{
    blindbit::{self, BlockDataObserver},
    coin_store::SpCoinStore,
    profile::SpStorageProfile,
    scan_state::ScanState,
    silentpayments::receiving::Label,
    spdk_core::{self, BlockData, FilterData, OutputSpendStatus, OwnedOutput, SpClient, UtxoData},
    SpNotification,
};

pub struct ScanStores<P: SpStorageProfile> {
    pub coin_store: Arc<Mutex<SpCoinStore<P>>>,
    pub tx_store: Arc<Mutex<crate::SpTxStore<P>>>,
    pub scan_state: Arc<Mutex<ScanState>>,
    pub sender: mpsc::Sender<crate::Notification>,
}

pub fn scan_blocks<P: SpStorageProfile>(
    agent: Arc<ureq::Agent>,
    blindbit_url: &str,
    sp_client: &SpClient,
    stores: &ScanStores<P>,
    stop: &Arc<AtomicBool>,
    start: Height,
    end: Height,
    dust_limit: Option<Amount>,
    with_cutthrough: bool,
) -> spdk_core::error::Result<()> {
    scan_blocks_with_observer(
        agent,
        blindbit_url,
        sp_client,
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
    sp_client: &SpClient,
    stores: &ScanStores<P>,
    stop: &Arc<AtomicBool>,
    start: Height,
    end: Height,
    dust_limit: Option<Amount>,
    with_cutthrough: bool,
    block_data_observer: Option<BlockDataObserver>,
) -> spdk_core::error::Result<()> {
    if start > end {
        return Err(spdk_core::error::Error::InvalidRange(
            start.to_consensus_u32(),
            end.to_consensus_u32(),
        ));
    }

    log::info!("start: {} end: {}", start, end);
    let start_time = Instant::now();
    let owned = stores.coin_store.lock().expect("poisoned").all_outpoints();
    process_two_phase(
        agent,
        blindbit_url,
        sp_client,
        stores,
        stop,
        start,
        end,
        dust_limit,
        with_cutthrough,
        block_data_observer,
        owned,
    )?;
    log::info!(
        "Blindbit scan completed in {} seconds",
        start_time.elapsed().as_secs()
    );
    Ok(())
}

fn should_interrupt(stop: &Arc<AtomicBool>) -> bool {
    stop.load(Ordering::Relaxed)
}

fn save_state<P: SpStorageProfile>(stores: &ScanStores<P>) -> spdk_core::error::Result<()> {
    stores.coin_store.lock().expect("poisoned").persist();
    stores.tx_store.lock().expect("poisoned").persist();
    stores.scan_state.lock().expect("poisoned").persist();
    Ok(())
}

fn record_outputs<P: SpStorageProfile>(
    stores: &ScanStores<P>,
    outputs: HashMap<OutPoint, OwnedOutput>,
) -> spdk_core::error::Result<()> {
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
) -> spdk_core::error::Result<()> {
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
) -> spdk_core::error::Result<()> {
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
) -> spdk_core::error::Result<()> {
    let mut state = stores.scan_state.lock().expect("poisoned");
    state.advance_frontier(height.to_consensus_u32(), *block_hash.as_byte_array());
    state.persist();
    Ok(())
}

fn record_spend_frontier<P: SpStorageProfile>(
    stores: &ScanStores<P>,
    height: Height,
) -> spdk_core::error::Result<()> {
    let mut state = stores.scan_state.lock().expect("poisoned");
    state.advance_spend_frontier(height.to_consensus_u32());
    state.persist();
    Ok(())
}

fn spend_frontier<P: SpStorageProfile>(
    stores: &ScanStores<P>,
) -> spdk_core::error::Result<Option<u32>> {
    Ok(stores
        .scan_state
        .lock()
        .expect("poisoned")
        .last_spend_height())
}

fn scan_utxos(
    agent: &ureq::Agent,
    blindbit_url: &str,
    sp_client: &SpClient,
    blkheight: Height,
    secrets_map: HashMap<[u8; 34], bitcoin::secp256k1::PublicKey>,
) -> spdk_core::error::Result<Vec<(Option<Label>, UtxoData, bitcoin::secp256k1::Scalar)>> {
    let utxos = blindbit::utxos(agent, blindbit_url, blkheight)?;
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

        let output_keys: spdk_core::error::Result<Vec<XOnlyPublicKey>> = utxos
            .iter()
            .filter_map(|x| {
                if x.scriptpubkey.is_p2tr() {
                    Some(
                        XOnlyPublicKey::from_slice(&x.scriptpubkey.as_bytes()[2..])
                            .map_err(spdk_core::Error::from),
                    )
                } else {
                    None
                }
            })
            .collect();

        let ours = sp_client
            .sp_receiver
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
) -> spdk_core::error::Result<bool> {
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
    agent: &ureq::Agent,
    blindbit_url: &str,
    sp_client: &SpClient,
    blockdata: &BlockData,
) -> spdk_core::error::Result<HashMap<OutPoint, OwnedOutput>> {
    process_block_outputs(
        agent,
        blindbit_url,
        sp_client,
        blockdata.blkheight,
        &blockdata.tweaks,
        blockdata.new_utxo_filter.clone(),
    )
}

fn process_block_outputs(
    agent: &ureq::Agent,
    blindbit_url: &str,
    sp_client: &SpClient,
    blkheight: Height,
    tweaks: &[[u8; 33]],
    new_utxo_filter: FilterData,
) -> spdk_core::error::Result<HashMap<OutPoint, OwnedOutput>> {
    let mut res = HashMap::new();
    if tweaks.is_empty() {
        return Ok(res);
    }

    let tweaks: Vec<bitcoin::secp256k1::PublicKey> = tweaks
        .iter()
        .map(|t| bitcoin::secp256k1::PublicKey::from_slice(t))
        .collect::<std::result::Result<_, _>>()?;
    let candidate_spks = sp_client.get_candidate_spks(&tweaks)?;
    let candidate_spks: Vec<&[u8; 34]> = candidate_spks.iter().collect();

    #[cfg(feature = "scan-profile")]
    let __t = std::time::Instant::now();
    let blkfilter = BlockFilter::new(&new_utxo_filter.data);
    let blkhash = new_utxo_filter.block_hash;
    let matched_outputs = check_block_outputs(blkfilter, blkhash, candidate_spks)?;
    #[cfg(feature = "scan-profile")]
    crate::spdk_core::scan_profile::add(
        &crate::spdk_core::scan_profile::OUTPUT_FILTER_NS,
        __t.elapsed(),
    );

    if !matched_outputs {
        return Ok(res);
    }

    log::info!("matched outputs on: {}", blkheight);
    let secrets_map = sp_client.get_script_to_secret_map(tweaks)?;
    #[cfg(feature = "scan-profile")]
    let __t = std::time::Instant::now();
    let found = scan_utxos(agent, blindbit_url, sp_client, blkheight, secrets_map)?;
    #[cfg(feature = "scan-profile")]
    crate::spdk_core::scan_profile::add(
        &crate::spdk_core::scan_profile::SCAN_UTXOS_NS,
        __t.elapsed(),
    );

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
) -> spdk_core::error::Result<HashMap<[u8; 8], OutPoint>> {
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
) -> spdk_core::error::Result<bool> {
    if !input_hashes.is_empty() {
        Ok(spent_filter.match_any(&blkhash, &mut input_hashes.into_iter())?)
    } else {
        Ok(false)
    }
}

fn match_inputs_for(
    agent: &ureq::Agent,
    blindbit_url: &str,
    blkheight: Height,
    spent_filter: FilterData,
    owned: &HashSet<OutPoint>,
) -> spdk_core::error::Result<HashSet<OutPoint>> {
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
        let spent = blindbit::spent_index(agent, blindbit_url, blkheight)?.data;
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

#[allow(clippy::too_many_arguments)]
fn commit_block<P: SpStorageProfile>(
    stores: &ScanStores<P>,
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
) -> spdk_core::error::Result<()> {
    let blkheight = blockdata.blkheight;
    let blkhash = blockdata.blkhash;
    let idx = (blkheight.to_consensus_u32() - start_u32) as usize;
    done[idx] = true;
    hashes[idx] = Some(blkhash);
    if !outs.is_empty() {
        for outpoint in outs.keys() {
            owned.insert(*outpoint);
        }
        record_outputs(stores, outs)?;
    }

    let mut next = recv_tip.map(|h| h + 1).unwrap_or(start_u32);
    while next <= end_u32 && done[(next - start_u32) as usize] {
        *recv_tip = Some(next);
        next += 1;
    }

    if let Some(tip) = *recv_tip {
        if !*notified_any || tip - *last_progress >= 100 {
            record_progress(stores, Height::from_consensus(tip)?, end)?;
            *last_progress = tip;
            *notified_any = true;
        }
    }

    if last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
        if let Some(tip) = *recv_tip {
            let i = (tip - start_u32) as usize;
            let hash = hashes[i].ok_or(spdk_core::error::Error::MissingBlockHash(tip))?;
            record_scan_frontier(stores, Height::from_consensus(tip)?, hash)?;
            save_state(stores)?;
        }
        *last_checkpoint = std::time::Instant::now();
    }
    let _ = start;
    Ok(())
}

const CHECKPOINT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

#[allow(clippy::too_many_arguments)]
fn process_two_phase<P: SpStorageProfile>(
    agent: Arc<ureq::Agent>,
    blindbit_url: &str,
    sp_client: &SpClient,
    stores: &ScanStores<P>,
    stop: &Arc<AtomicBool>,
    start: Height,
    end: Height,
    dust_limit: Option<Amount>,
    with_cutthrough: bool,
    block_data_observer: Option<BlockDataObserver>,
    mut owned: HashSet<OutPoint>,
) -> spdk_core::error::Result<HashSet<OutPoint>> {
    let start_u32 = start.to_consensus_u32();
    let end_u32 = end.to_consensus_u32();
    let len = (end_u32 - start_u32 + 1) as usize;

    if spend_frontier(stores)?.is_none() {
        if let Some(floor) = start_u32.checked_sub(1) {
            record_spend_frontier(stores, Height::from_consensus(floor)?)?;
            save_state(stores)?;
        }
    }

    let iter = blindbit::get_block_data_for_range(
        agent.clone(),
        blindbit_url.to_string(),
        start_u32..=end_u32,
        dust_limit,
        with_cutthrough,
        block_data_observer,
    );
    let mut done = vec![false; len];
    let mut hashes: Vec<Option<BlockHash>> = vec![None; len];
    let mut recv_tip: Option<u32> = None;
    let mut last_progress = start_u32.saturating_sub(1);
    let mut notified_any = false;
    let mut last_checkpoint = Instant::now();

    #[cfg(all(not(target_arch = "wasm32"), feature = "parallel"))]
    {
        let n_workers = match_window_cap();
        let iter = std::sync::Mutex::new(iter);
        let (tx, rx) = std::sync::mpsc::sync_channel::<
            spdk_core::error::Result<(BlockData, HashMap<OutPoint, OwnedOutput>)>,
        >(n_workers * 2);
        let interrupted = std::thread::scope(|s| -> spdk_core::error::Result<bool> {
            for _ in 0..n_workers {
                let iter = &iter;
                let tx = tx.clone();
                let agent = agent.clone();
                s.spawn(move || loop {
                    if should_interrupt(stop) {
                        break;
                    }
                    let next = { iter.lock().expect("poisoned").next() };
                    match next {
                        Some(Ok(bd)) => {
                            let r = match_block_outputs(&agent, blindbit_url, sp_client, &bd)
                                .map(|outs| (bd, outs));
                            let stop = r.is_err();
                            if tx.send(r).is_err() || stop {
                                break;
                            }
                        }
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
                    stores,
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
                if should_interrupt(stop) {
                    save_state(stores)?;
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
            if should_interrupt(stop) {
                save_state(stores)?;
                return Ok(owned);
            }
            match iter.next() {
                Some(Ok(blockdata)) => {
                    let outs = match_block_outputs(&agent, blindbit_url, sp_client, &blockdata)?;
                    commit_block(
                        stores,
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

    if recv_tip != Some(end_u32) {
        return Err(spdk_core::error::Error::MissingBlockHash(end_u32));
    }
    let end_hash = hashes[len - 1].ok_or(spdk_core::error::Error::MissingBlockHash(end_u32))?;
    record_scan_frontier(stores, end, end_hash)?;
    record_progress(stores, end, end)?;
    save_state(stores)?;

    let spend_start = spend_frontier(stores)?.map(|h| h + 1).unwrap_or(start_u32);
    let mut last_checkpoint = Instant::now();
    let mut last_progress = spend_start.saturating_sub(1);
    for h in spend_start..=end_u32 {
        if should_interrupt(stop) {
            save_state(stores)?;
            return Ok(owned);
        }
        if owned.is_empty() {
            break;
        }
        let height = Height::from_consensus(h)?;
        let spent_filter = blindbit::spent_filter(&agent, blindbit_url, height, None)?;
        let blkhash = spent_filter.block_hash;
        let ins = match_inputs_for(&agent, blindbit_url, height, spent_filter, &owned)?;
        if !ins.is_empty() {
            for outpoint in &ins {
                owned.remove(outpoint);
            }
            record_inputs(stores, blkhash, ins)?;
        }
        if h - last_progress >= 1000 {
            record_progress(stores, height, end)?;
            last_progress = h;
        }
        if last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
            record_spend_frontier(stores, height)?;
            save_state(stores)?;
            last_checkpoint = Instant::now();
        }
    }
    record_spend_frontier(stores, end)?;
    save_state(stores)?;
    Ok(owned)
}
