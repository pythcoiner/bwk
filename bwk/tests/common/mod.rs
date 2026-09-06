//! Shared regtest harness helpers for the `HeaderStore` integration tests.
#![allow(dead_code)]

use std::{
    thread::sleep,
    time::{Duration, Instant},
};

pub use bwk_utils::test::{electrsd, start_electrs, TestBitcoinD};

use electrsd::{
    bitcoind::{
        bitcoincore_rpc::{jsonrpc::serde_json::Value, RpcApi},
        BitcoinD,
    },
    ElectrsD,
};

pub fn bootstrap_electrs(txindex: bool) -> (String, u16, ElectrsD, TestBitcoinD) {
    let (url, port, electrsd, bitcoind) = bwk_utils::test::bootstrap_electrs(txindex);
    generate(&bitcoind, 101);
    (url, port, electrsd, bitcoind)
}

/// Kill `electrsd` and spin up a fresh electrs process against the same
/// `bitcoind`, simulating a server restart (the new process gets its own
/// port; callers must repoint their client at the returned url/port).
pub fn restart_electrs(mut electrsd: ElectrsD, bitcoind: &BitcoinD) -> (String, u16, ElectrsD) {
    electrsd.kill().expect("kill electrs");
    start_electrs(bitcoind)
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
