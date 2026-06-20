//! Comparison benchmark: drives the **upstream cygnet3/spdk** async silent-payments
//! scan (`spdk-wallet`'s `SpScanner` + `backend-blindbit-v1`), the implementation
//! bwk-sp originally descended from, against the same blindbit oracle as
//! `sp_sync_bench`, so their `ms/block` can be compared head-to-head.
//!
//! Throwaway bench only (a `cygnet3/spdk` git dep pulling reqwest/tokio + an
//! edition-2024 crate; never shipped, MSRV-exempt), gated behind `--features bench-spdk`.
//!
//! Oracle metadata (network check, with-cutthrough, default end = tip) is taken via
//! bwk's own `bwk_sp::blindbit::info` so it matches `sp_sync_bench` exactly; only the
//! scan itself is spdk's. Keys are a fixed watch-only dummy, sync cost is independent
//! of whether the wallet owns any outputs.
//!
//! For an apples-to-apples run, point both benches at the same oracle/range and pass a
//! non-zero `--dust-limit` to both (spdk takes a mandatory `Amount`; sp_sync_bench treats
//! 0 as "no filter" via `None`). Use `sp_sync_bench --features instrumentation` on the bwk
//! side so it honours `--start/--end` (its default `bench` build always scans to tip).
//!
//! Example:
//!   cargo run -p bwk-sp --features bench-spdk --bin spdk_sync_bench -- \
//!     --url https://silentpayments.dev/blindbit/signet --network signet \
//!     --start 200000 --end 200010 --dust-limit 1000

use std::{
    collections::{HashMap, HashSet},
    sync::{atomic::AtomicBool, Arc, Mutex},
    time::Instant,
};

use anyhow::{anyhow, Result};
use backend_blindbit_v1::{BlindbitBackend, BlindbitClient};
use bwk_sp::bitcoin::{
    absolute::Height,
    secp256k1::{PublicKey, SecretKey},
    Amount, BlockHash, Network, OutPoint,
};
use spdk_core::updater::{DiscoveredOutput, Updater};
use spdk_wallet::{
    client::{SpClient, SpendKey},
    scanner::SpScanner,
};

/// Default dust limit in sats when `--dust-limit` is not given (matches sp_sync_bench).
const DEFAULT_DUST_LIMIT_SATS: u64 = 600;

/// Dummy spend public key (watch-only): the secp256k1 generator point G.
const DUMMY_SPEND_PUBKEY: &str =
    "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

/// Minimum birthday height per network, mirroring sp_sync_bench.
fn min_birthday_for(network: Network) -> u32 {
    match network {
        Network::Bitcoin => 709_632,
        _ => 1,
    }
}

fn parse_network(s: &str) -> std::result::Result<Network, String> {
    match s {
        "bitcoin" | "mainnet" => Ok(Network::Bitcoin),
        "signet" => Ok(Network::Signet),
        "testnet" => Ok(Network::Testnet),
        "regtest" => Ok(Network::Regtest),
        other => Err(format!("unknown network: {other}")),
    }
}

struct Args {
    url: String,
    network: Network,
    start: Option<u32>,
    end: Option<u32>,
    dust: u64,
}

/// Parse CLI args + env (same flags as sp_sync_bench). `Ok(None)` => `--help`.
fn parse_args() -> std::result::Result<Option<Args>, String> {
    let mut url = std::env::var("BWK_SP_BLINDBIT_URL").ok();
    let mut network_str = std::env::var("BWK_SP_NETWORK").ok();
    let mut start: Option<u32> = None;
    let mut end: Option<u32> = None;
    let mut dust: u64 = DEFAULT_DUST_LIMIT_SATS;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--url" => url = Some(args.next().ok_or("--url requires a value")?),
            "--network" => network_str = Some(args.next().ok_or("--network requires a value")?),
            "--start" => {
                let v = args.next().ok_or("--start requires a value")?;
                start = Some(v.parse().map_err(|_| format!("invalid --start: {v}"))?);
            }
            "--end" => {
                let v = args.next().ok_or("--end requires a value")?;
                end = Some(v.parse().map_err(|_| format!("invalid --end: {v}"))?);
            }
            "--dust-limit" => {
                let v = args.next().ok_or("--dust-limit requires a value")?;
                dust = v
                    .parse()
                    .map_err(|_| format!("invalid --dust-limit: {v}"))?;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    let url = url.ok_or("--url (or env BWK_SP_BLINDBIT_URL) is required")?;
    let network = match network_str {
        Some(s) => parse_network(&s)?,
        None => Network::Bitcoin,
    };
    let url = if url.contains("://") {
        url
    } else {
        format!("http://{url}")
    };

    Ok(Some(Args {
        url,
        network,
        start,
        end,
        dust,
    }))
}

fn usage() -> String {
    "spdk_sync_bench - benchmark the upstream cygnet3/spdk async SP scan (comparison baseline)\n\
     \n\
     USAGE:\n\
     \x20 spdk_sync_bench --url <URL> [--network <NET>] [--start <H>] [--end <H>] [--dust-limit <SATS>]\n\
     \n\
     OPTIONS:\n\
     \x20 --url <URL>          blindbit oracle (or env BWK_SP_BLINDBIT_URL)\n\
     \x20 --network <NET>      bitcoin|signet|testnet|regtest (default bitcoin; or env BWK_SP_NETWORK)\n\
     \x20 --start <H>          first height (default: network birthday)\n\
     \x20 --end <H>            last height (default: oracle tip)\n\
     \x20 --dust-limit <SATS>  dust limit, mandatory Amount in spdk (default 600)\n\
     \x20 -h, --help           print this help\n"
        .to_string()
}

/// Format a duration as a compact `HhMMmSSs` / `MmSSs` / `SSs` (mirrors sp_sync_bench).
fn fmt_dur(secs: f64) -> String {
    let total = secs.round() as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}h{m:02}m{s:02}s")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Updater that counts committed blocks/outputs/inputs (the scanner calls it once per
/// block) and prints a throttled progress line (~1/s) so a long scan isn't silent.
/// Shared with `main` via `Arc<Mutex>` to read the totals back after the scan.
#[derive(Clone)]
struct ProgressUpdater {
    state: Arc<Mutex<Progress>>,
}

struct Progress {
    total: u64,
    blocks: u64,
    outputs: usize,
    inputs: usize,
    started: Option<Instant>,
    last_print: Instant,
}

impl ProgressUpdater {
    fn new(total: u64) -> Self {
        Self {
            state: Arc::new(Mutex::new(Progress {
                total,
                blocks: 0,
                outputs: 0,
                inputs: 0,
                started: None,
                last_print: Instant::now(),
            })),
        }
    }
}

impl Updater for ProgressUpdater {
    fn record_block_scan_result(
        &mut self,
        _blkheight: Height,
        _blkhash: BlockHash,
        discovered_inputs: HashSet<OutPoint>,
        discovered_outputs: HashMap<OutPoint, DiscoveredOutput>,
    ) -> Result<()> {
        let now = Instant::now();
        let mut p = self.state.lock().expect("poisoned");
        if p.started.is_none() {
            p.started = Some(now);
            p.last_print = now;
        }
        p.blocks += 1;
        p.inputs += discovered_inputs.len();
        p.outputs += discovered_outputs.len();
        // Time-throttle catches large ranges (the backend's 200-deep fetch window slides,
        // so commits spread over wall-clock); the block-step catches small ranges (<=200
        // fetch concurrently and commit in one burst, where <1s elapses between prints).
        let step = (p.total / 50).max(20);
        if now.duration_since(p.last_print).as_secs_f64() >= 1.0 || p.blocks % step == 0 {
            p.last_print = now;
            let secs = p
                .started
                .map(|t| now.duration_since(t).as_secs_f64())
                .unwrap_or(0.0)
                .max(1e-9);
            let rate = p.blocks as f64 / secs;
            let pct = p.blocks as f64 / p.total.max(1) as f64 * 100.0;
            let eta = if rate > 0.0 {
                fmt_dur(p.total.saturating_sub(p.blocks) as f64 / rate)
            } else {
                "?".to_string()
            };
            println!(
                "  [{pct:5.1}%] {}/{}  {rate:.0} blk/s  ETA {eta}",
                p.blocks, p.total
            );
        }
        Ok(())
    }
}

fn main() -> Result<()> {
    let args = match parse_args() {
        Ok(Some(args)) => args,
        Ok(None) => {
            print!("{}", usage());
            return Ok(());
        }
        Err(e) => {
            eprint!("error: {e}\n\n{}", usage());
            std::process::exit(1);
        }
    };

    // Oracle metadata via bwk's blindbit client (identical to sp_sync_bench): network
    // check, with-cutthrough capability, and the tip for the default --end.
    let agent = bwk_sp::blindbit::agent();
    let info = bwk_sp::blindbit::info(&agent, &args.url)?;
    if info.network != args.network {
        return Err(anyhow!(
            "oracle network {} does not match --network {}",
            info.network,
            args.network
        ));
    }
    let with_cutthrough = info.tweaks_cut_through_with_dust_filter;
    let start = args.start.unwrap_or_else(|| min_birthday_for(args.network));
    let end = args.end.unwrap_or_else(|| info.height.to_consensus_u32());
    if end < start {
        return Err(anyhow!("end height {end} is before start height {start}"));
    }
    let block_count = u64::from(end - start + 1);

    // Watch-only dummy keys (scan cost is key-independent).
    let scan_sk = SecretKey::from_slice(&[1u8; 32]).expect("valid scan key");
    let spend_pk: PublicKey = DUMMY_SPEND_PUBKEY.parse().expect("valid spend pubkey");
    let client = SpClient::new(scan_sk, SpendKey::Public(spend_pk), args.network)?;

    let backend = BlindbitBackend::new(BlindbitClient::new(&args.url)?);
    let updater = ProgressUpdater::new(block_count);
    let keep_scanning = AtomicBool::new(true);
    let mut scanner = SpScanner::new(
        client,
        Box::new(updater.clone()),
        Box::new(backend),
        HashSet::new(),
        &keep_scanning,
    );

    let dust = Amount::from_sat(args.dust);
    println!("plan:");
    println!("  network:      {}", args.network);
    println!("  url:          {}", args.url);
    println!("  start height: {start}");
    println!("  end height:   {end}");
    println!("  blocks:       {block_count}");
    println!("  dust limit:   {} sat", args.dust);
    println!("  cutthrough:   {with_cutthrough}");
    println!("scanning (upstream cygnet3/spdk async)...");

    let range = Height::from_consensus(start)?..=Height::from_consensus(end)?;
    let rt = tokio::runtime::Runtime::new()?;
    let t = Instant::now();
    rt.block_on(scanner.scan_blocks(range, false, dust, with_cutthrough))?;
    let secs = t.elapsed().as_secs_f64();

    let (blocks_per_sec, ms_per_block) = if secs > 0.0 && block_count > 0 {
        (
            block_count as f64 / secs,
            (secs * 1000.0) / block_count as f64,
        )
    } else {
        (f64::INFINITY, 0.0)
    };
    let p = updater.state.lock().expect("poisoned");
    println!("summary:");
    println!("  blocks scanned:   {block_count}");
    println!("  elapsed:          {secs:.3} s");
    println!("  blocks/sec:       {blocks_per_sec:.2}");
    println!("  ms/block:         {ms_per_block:.3}");
    println!(
        "  discovered:       {} outputs, {} inputs (committed {} blocks)",
        p.outputs, p.inputs, p.blocks
    );

    Ok(())
}
