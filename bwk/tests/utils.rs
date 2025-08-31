use std::{env, path::PathBuf};

use bwk_electrum::client::Client;
use electrsd::{
    bitcoind::{
        bitcoincore_rpc::{jsonrpc::serde_json::Value, RpcApi},
        BitcoinD, P2P,
    },
    ElectrsD,
};
use miniscript::bitcoin::{Address, Amount, Transaction, Txid};

pub fn bootstrap_electrs() -> (
    String, /* url */
    u16,    /* port */
    ElectrsD,
    BitcoinD,
) {
    let mut cwd: PathBuf = env::current_dir().expect("Failed to get current directory");
    cwd.push("tests");

    let mut electrs_path = cwd.clone();
    electrs_path.push("bin");
    electrs_path.push("electrs_0_9_11");

    let mut bitcoind_path = cwd.clone();
    bitcoind_path.push("bin");
    bitcoind_path.push("bitcoind_25_2");

    let mut conf = electrsd::bitcoind::Conf::default();
    conf.p2p = P2P::Yes;
    let bitcoind = BitcoinD::with_conf(bitcoind_path, &conf).unwrap();

    let mut electrsd_conf = electrsd::Conf::default();
    electrsd_conf.args = vec!["--log-filters", "DEBUG"];
    electrsd_conf.buffered_logs = true;

    let electrsd = ElectrsD::with_conf(electrs_path, &bitcoind, &electrsd_conf).unwrap();
    let (url, port) = electrsd.electrum_url.split_once(':').unwrap();
    let port = port.parse::<u16>().unwrap();

    // mine 101 blocks
    let node_address = bitcoind.client.call::<Value>("getnewaddress", &[]).unwrap();
    bitcoind
        .client
        .call::<Value>("generatetoaddress", &[101.into(), node_address])
        .unwrap();

    (url.into(), port, electrsd, bitcoind)
}

pub fn tcp_client() -> (Client, ElectrsD, BitcoinD) {
    let (url, port, e, b) = bootstrap_electrs();
    let client = Client::new(&url, port).unwrap();

    (client, e, b)
}

pub fn send_to_address(bitcoind: &BitcoinD, addr: &Address, amount: Amount) -> Txid {
    let txid = bitcoind
        .client
        .send_to_address(addr, amount, None, None, None, None, None, None)
        .unwrap();
    log::debug!("send_to_address({}, {}) => {}", addr, amount, txid);
    txid
}

pub fn get_tx(bitcoind: &BitcoinD, txid: Txid) -> Transaction {
    bitcoind.client.get_raw_transaction(&txid, None).unwrap()
}

pub fn broadcast(bitcoind: &BitcoinD, transaction: Transaction) {
    let _txid = bitcoind.client.send_raw_transaction(&transaction).unwrap();
}

pub fn get_block_hash(bitcoind: &BitcoinD, height: u32) -> String {
    bitcoind
        .client
        .call("getblockhash", &[height.into()])
        .unwrap()
}

pub fn get_block_height(bitcoind: &BitcoinD) -> u32 {
    bitcoind.client.call("getblockcount", &[]).unwrap()
}

pub fn generate(bitcoind: &BitcoinD, blocks: u32) {
    let node_address = bitcoind.client.call::<Value>("getnewaddress", &[]).unwrap();
    bitcoind
        .client
        .call::<Value>("generatetoaddress", &[blocks.into(), node_address])
        .unwrap();
}

pub fn reorg_chain(bitcoind: &BitcoinD, blocks: u32) {
    let chain_height: u32 = get_block_height(bitcoind);
    let reorg_height = chain_height - blocks;
    let block_hash = get_block_hash(bitcoind, reorg_height);

    invalidate_block(bitcoind, block_hash);

    generate(bitcoind, blocks);
}

pub fn invalidate_block(bitcoind: &BitcoinD, block_hash: String) {
    bitcoind
        .client
        .call::<Value>("invalidateblock", &[block_hash.clone().into()])
        .unwrap();

    log::info!("Invalidated block with hash: {}", block_hash);
}

pub fn dump_logs(e: &mut ElectrsD) {
    while let Ok(msg) = e.logs.try_recv() {
        println!("{}", msg);
    }
}
