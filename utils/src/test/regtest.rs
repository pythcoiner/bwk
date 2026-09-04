//! Regtest harness: a bitcoind + electrs pair and the block helpers that
//! drive them.
//!
//! The node binaries are looked up under `tests/bin` of the crate running the
//! test, so each consumer ships its own pair.

use std::{
    env,
    path::PathBuf,
    thread::sleep,
    time::{Duration, Instant},
};

use electrsd::{
    bitcoind::{
        bitcoincore_rpc::{jsonrpc::serde_json::Value, RpcApi},
        BitcoinD, P2P,
    },
    electrum_client::ElectrumApi,
    ElectrsD,
};

/// How long `bootstrap_electrs` waits for electrs to index the pre-mined
/// blocks before handing the pair back.
const SYNC_TIMEOUT: Duration = Duration::from_secs(30);

/// Spin up an electrs process against `bitcoind` on a freshly assigned port.
fn spawn_electrs(bitcoind: &BitcoinD) -> (String, u16, ElectrsD) {
    let mut cwd: PathBuf = env::current_dir().expect("current_dir");
    cwd.push("tests");
    let mut electrs_path = cwd.clone();
    electrs_path.push("bin");
    electrs_path.push("electrs_0_9_11");

    let mut electrsd_conf = electrsd::Conf::default();
    electrsd_conf.args = vec!["--log-filters", "DEBUG"];
    electrsd_conf.buffered_logs = true;

    let electrsd = ElectrsD::with_conf(electrs_path, bitcoind, &electrsd_conf).unwrap();
    let (url, port) = electrsd.electrum_url.split_once(':').unwrap();
    let port = port.parse::<u16>().unwrap();

    (url.into(), port, electrsd)
}

/// Spin up bitcoind + electrs and pre-mine 101 blocks so coins to fresh
/// addresses can be confirmed immediately.
pub fn bootstrap_electrs() -> (String, u16, ElectrsD, BitcoinD) {
    bootstrap_electrs_with_args(&[])
}

/// [`bootstrap_electrs`] with extra `bitcoind` command line arguments, for a
/// test that needs the node configured differently (`-txindex` and such).
pub fn bootstrap_electrs_with_args(bitcoind_args: &[&str]) -> (String, u16, ElectrsD, BitcoinD) {
    let mut cwd: PathBuf = env::current_dir().expect("current_dir");
    cwd.push("tests");

    let mut bitcoind_path = cwd.clone();
    bitcoind_path.push("bin");
    bitcoind_path.push("bitcoind_25_2");

    let mut conf = electrsd::bitcoind::Conf::default();
    conf.p2p = P2P::Yes;
    conf.args.extend_from_slice(bitcoind_args);
    let bitcoind = BitcoinD::with_conf(bitcoind_path, &conf).unwrap();

    let (url, port, electrsd) = spawn_electrs(&bitcoind);

    let node_address = bitcoind.client.call::<Value>("getnewaddress", &[]).unwrap();
    bitcoind
        .client
        .call::<Value>("generatetoaddress", &[101.into(), node_address])
        .unwrap();

    wait_electrs_synced(&bitcoind, &electrsd);

    (url, port, electrsd, bitcoind)
}

/// Wait until electrs has indexed up to bitcoind's tip, so a test does not
/// start querying it while it is still ingesting the pre-mined blocks.
fn wait_electrs_synced(bitcoind: &BitcoinD, electrsd: &ElectrsD) {
    let target = get_block_height(bitcoind);
    let synced = wait_until(SYNC_TIMEOUT, || {
        electrsd
            .client
            .block_headers_subscribe()
            .is_ok_and(|header| header.height as u32 >= target)
    });
    assert!(synced, "electrs did not reach height {target}");
}

/// Kill `electrsd` and spin up a fresh electrs process against the same
/// `bitcoind`, simulating a server restart (the new process gets its own
/// port; callers must repoint their client at the returned url/port).
pub fn restart_electrs(mut electrsd: ElectrsD, bitcoind: &BitcoinD) -> (String, u16, ElectrsD) {
    electrsd.kill().expect("kill electrs");
    spawn_electrs(bitcoind)
}

pub fn generate(bitcoind: &BitcoinD, blocks: u32) {
    let node_address = bitcoind.client.call::<Value>("getnewaddress", &[]).unwrap();
    bitcoind
        .client
        .call::<Value>("generatetoaddress", &[blocks.into(), node_address])
        .unwrap();
}

pub fn get_block_height(bitcoind: &BitcoinD) -> u32 {
    bitcoind.client.call("getblockcount", &[]).unwrap()
}

pub fn get_block_hash_str(bitcoind: &BitcoinD, height: u32) -> String {
    bitcoind
        .client
        .call("getblockhash", &[height.into()])
        .unwrap()
}

pub fn invalidate_block(bitcoind: &BitcoinD, hash: String) {
    bitcoind
        .client
        .call::<Value>("invalidateblock", &[hash.into()])
        .unwrap();
}

/// Poll `cond` for up to `timeout`. Returns true if it ever held.
pub fn wait_until<F: FnMut() -> bool>(timeout: Duration, mut cond: F) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return true;
        }
        sleep(Duration::from_millis(100));
    }
    cond()
}

/// Logger for the regtest tests. Unlike [`super::setup_logger`] it keeps the
/// `RUST_LOG` default, so bitcoind and electrs do not flood the output.
pub fn init_logger() {
    let _ = env_logger::builder().is_test(true).try_init();
}
