pub mod utils;
use std::{collections::BTreeMap, sync::Once, thread::sleep, time::Duration};

use crate::utils::bootstrap_electrs;
use bwk::{config::Config, descriptor::ScriptType, Account};
use electrsd::bitcoind::bitcoincore_rpc::RpcApi;
use miniscript::bitcoin::bip32::ChildNumber;
use utils::{dump_logs, generate, get_block_hash, get_block_height, reorg_chain, send_to_address};
use {
    bip39::Mnemonic,
    miniscript::bitcoin::{self, Amount, Network},
};

static INIT: Once = Once::new();

pub fn setup_logger() {
    INIT.call_once(|| {
        env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Debug)
            .filter_module("bitcoind", log::LevelFilter::Info)
            .filter_module("bitcoincore_rpc", log::LevelFilter::Info)
            .filter_module("bwk::account", log::LevelFilter::Debug)
            .filter_module("bwk-electrum::electrum", log::LevelFilter::Debug)
            .filter_module("bwk-electrum::raw_client", log::LevelFilter::Debug)
            .init();
    });
}

pub fn wait_until_timeout<F>(condition: F, timeout: u64)
where
    F: Fn() -> bool,
{
    let delay = Duration::from_millis(100);
    let start_time = std::time::Instant::now();

    while start_time.elapsed().as_secs() < timeout {
        if condition() {
            return;
        }
        sleep(delay);
    }
    panic!("Timeout elapsed while waiting for condition.");
}

#[test]
fn test_reorg() {
    setup_logger();
    let (_, _, _electrsd, bitcoind) = bootstrap_electrs();
    generate(&bitcoind, 100);

    reorg_chain(&bitcoind, 5);
}

#[test]
fn simple_wallet() {
    setup_logger();
    let (url, port, _electrsd, bitcoind) = bootstrap_electrs();
    generate(&bitcoind, 100);

    const TIMEOUT: u64 = 5;
    const BLOCKS: u32 = 1;

    let look_ahead = 20;

    let mnemonic = Mnemonic::generate(12).unwrap();
    let mut config = Config::new(
        mnemonic.to_string(),
        "account".to_string(),
        bitcoin::Network::Regtest,
        ScriptType::Segwit(ChildNumber::from_hardened_idx(0).unwrap()),
        ".bwk",
        false,
    );
    config.network = Network::Regtest;
    config.look_ahead = look_ahead;
    config.set_electrum_url(url);
    config.set_electrum_port(port.to_string());
    config.set_mnemonic(mnemonic.to_string());
    let mut account = Account::new(config);
    account.start_electrum();
    sleep(Duration::from_millis(300));

    let recv_addr = account.new_recv_addr();
    let change_addr = account.new_change_addr();

    send_to_address(&bitcoind, &recv_addr, Amount::from_btc(0.1).unwrap());
    generate(&bitcoind, BLOCKS);
    wait_until_timeout(
        || {
            let coins = account.coins();
            coins.len() == 1
        },
        TIMEOUT,
    );

    // Test change address
    send_to_address(&bitcoind, &change_addr, Amount::from_btc(0.1).unwrap());
    generate(&bitcoind, BLOCKS);
    wait_until_timeout(
        || {
            let coins = account.coins();
            coins.len() == 2
        },
        TIMEOUT,
    );

    // receive at look_ahead bound
    let recv_addr = account.recv_at(look_ahead);
    send_to_address(&bitcoind, &recv_addr, Amount::from_btc(0.1).unwrap());
    generate(&bitcoind, BLOCKS);
    wait_until_timeout(
        || {
            let coins = account.coins();
            coins.len() == 3
        },
        TIMEOUT,
    );

    // change at look_ahead bound
    let change_addr = account.change_at(look_ahead);
    send_to_address(&bitcoind, &change_addr, Amount::from_btc(0.1).unwrap());
    generate(&bitcoind, BLOCKS);
    wait_until_timeout(
        || {
            let coins = account.coins();
            coins.len() == 4
        },
        TIMEOUT,
    );

    let undiscovered_tip = account.recv_watch_tip() + 1;

    // receive beyond the look_ahead bound
    let recv_addr = account.recv_at(undiscovered_tip);
    send_to_address(&bitcoind, &recv_addr, Amount::from_btc(0.1).unwrap());
    generate(&bitcoind, BLOCKS);
    let coins = account.coins();
    // the coin is not detected for receiving address
    assert_eq!(coins.len(), 4);

    // change beyond the look_ahead bound
    let change_addr = account.change_at(undiscovered_tip);
    send_to_address(&bitcoind, &change_addr, Amount::from_btc(0.1).unwrap());
    generate(&bitcoind, BLOCKS);
    let coins = account.coins();
    // the coin is not detected for change address
    assert_eq!(coins.len(), 4);

    // move the watch tip forward
    account.new_recv_addr();
    account.new_recv_addr();
    wait_until_timeout(
        || {
            let coins = account.coins();
            coins.len() == 5
        },
        TIMEOUT,
    );

    account.new_change_addr();
    account.new_change_addr();
    wait_until_timeout(
        || {
            let coins = account.coins();
            coins.len() == 6
        },
        TIMEOUT,
    );
}

#[test]
fn simple_reorg() {
    setup_logger();
    let (url, port, mut electrsd, bitcoind) = bootstrap_electrs();
    generate(&bitcoind, 110);

    const TIMEOUT: u64 = 5;

    let look_ahead = 20;

    let mnemonic = Mnemonic::generate(12).unwrap();
    let mut config = Config::new(
        mnemonic.to_string(),
        "account".to_string(),
        bitcoin::Network::Regtest,
        ScriptType::Segwit(ChildNumber::from_hardened_idx(0).unwrap()),
        ".bwk",
        false,
    );
    config.look_ahead = look_ahead;
    config.set_electrum_url(url);
    config.set_electrum_port(port.to_string());
    config.set_mnemonic(mnemonic.to_string());
    let mut account = Account::new(config);
    account.start_electrum();
    sleep(Duration::from_millis(300));

    let recv_addr = account.new_recv_addr();
    let change_addr = account.new_change_addr();

    sleep(Duration::from_secs(1));

    // send to recv address
    let recv_txid = send_to_address(&bitcoind, &recv_addr, Amount::from_btc(0.1).unwrap());
    let recv_tx = bitcoind
        .client
        .get_raw_transaction(&recv_txid, None)
        .unwrap();

    generate(&bitcoind, 1);

    sleep(Duration::from_secs(1));
    dump_logs(&mut electrsd);

    // send to change address
    let change_txid = send_to_address(&bitcoind, &change_addr, Amount::from_btc(0.1).unwrap());
    let change_tx = bitcoind
        .client
        .get_raw_transaction(&change_txid, None)
        .unwrap();
    generate(&bitcoind, 1);

    wait_until_timeout(
        || {
            let coins = account.coins();
            coins.len() == 2
        },
        TIMEOUT,
    );

    let coins = account.coins();
    let coins_height: BTreeMap<_, _> = coins.into_iter().map(|(c, e)| (c, e.height())).collect();

    // all coins are confirmed
    assert!(coins_height.iter().all(|(_, e)| e.is_some()));

    let height_before_reorg = get_block_height(&bitcoind);
    let h_before_reorg = get_block_hash(&bitcoind, height_before_reorg);

    sleep(Duration::from_secs(2));

    electrsd.clear_logs();
    log::warn!(" ------------------------------- reorg now ------------------------");
    reorg_chain(&bitcoind, 7);
    generate(&bitcoind, 2);
    dump_logs(&mut electrsd);
    sleep(Duration::from_secs(2));
    dump_logs(&mut electrsd);

    // FIXME:
    // NOTE: here we likely hitting an `electrs` bug:
    // - we can see in the electrs logs that 2 status (None) updates are assumed sent
    //   from electrs end
    // - only 1 status update is received on our raw client TCP stream end

    // FIXME: shouldn't bitcoind rebroadcast txs itself after reorg?
    log::warn!(" ------------------------------- rebroadcast recv ------------------------");
    bitcoind.client.send_raw_transaction(&recv_tx).unwrap();
    generate(&bitcoind, 1);
    sleep(Duration::from_secs(2));
    dump_logs(&mut electrsd);

    // FIXME: shouldn't bitcoind rebroadcast txs itself after reorg?
    log::warn!(" ------------------------------- rebroadcast change ------------------------");
    bitcoind.client.send_raw_transaction(&change_tx).unwrap();
    generate(&bitcoind, 1);
    sleep(Duration::from_secs(2));
    dump_logs(&mut electrsd);

    let new_h = get_block_hash(&bitcoind, height_before_reorg);
    assert_ne!(h_before_reorg, new_h);

    let coins = account.coins();
    // there is still 2 coins
    assert_eq!(coins.len(), 2);
}

fn test_conf_unconf() {
    // TODO: verify that coins status (confirmed/unconfirmed) are good
}
