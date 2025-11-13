use std::{str::FromStr, sync::Once};

pub use corepc_node::{self, Client, Node};
pub use miniscript;

use miniscript::bitcoin::{
    self, hashes::serde_macros::serde_details::SerdeHash, key::rand::random, Address, Amount,
    BlockHash, OutPoint, ScriptBuf, Transaction, TxIn, TxOut, Txid,
};
use rand::Rng;

static INIT: Once = Once::new();

pub fn setup_logger() {
    INIT.call_once(|| {
        env_logger::builder()
            // Ensures output is only printed in test mode
            .is_test(true)
            .filter_level(log::LevelFilter::Debug)
            .init();
    });
}

// generate a dummy txid
pub fn txid() -> bitcoin::Txid {
    let raw: [u8; 32] = random();
    bitcoin::Txid::from_slice_delegated(&raw).expect("Invalid Txid")
}

/// Generates a random Bitcoin transaction input.
#[allow(deprecated)]
pub fn random_input() -> TxIn {
    let mut rng = rand::thread_rng();
    let txid = txid();
    let vout = rng.gen_range(0..10);
    TxIn {
        previous_output: OutPoint::new(txid, vout),
        script_sig: ScriptBuf::new(),
        sequence: bitcoin::Sequence(0xFFFFFFFF),
        witness: bitcoin::Witness::new(),
    }
}

/// Generates a random Bitcoin transaction output.
#[allow(deprecated)]
pub fn random_output() -> TxOut {
    let mut rng = rand::thread_rng();
    let value = rng.gen_range(1..100_000);
    let script_pubkey = ScriptBuf::new();
    TxOut {
        value: Amount::from_sat(value),
        script_pubkey,
    }
}

/// Generates a funding transaction paying to a given spk with additional
/// random inputs and outputs.
#[allow(deprecated)]
pub fn funding_tx(spk: ScriptBuf, amount: f64) -> bitcoin::Transaction {
    let num_inputs = rand::thread_rng().gen_range(1..10);
    let num_outputs = rand::thread_rng().gen_range(1..5);

    let mut input = vec![];
    let mut output = vec![];

    for _ in 0..num_inputs {
        input.push(random_input());
    }

    for _ in 0..num_outputs {
        output.push(random_output());
    }

    output.push(TxOut {
        value: Amount::from_btc(amount).unwrap(),
        script_pubkey: spk,
    });

    bitcoin::Transaction {
        version: bitcoin::transaction::Version(2),
        lock_time: bitcoin::absolute::LockTime::Blocks(bitcoin::absolute::Height::ZERO),
        input,
        output,
    }
}

/// Generates a spending transaction, spending a given outpoint with additional
/// random inputs and outputs.
#[allow(deprecated)]
pub fn spending_tx(outpoint: bitcoin::OutPoint) -> bitcoin::Transaction {
    let num_inputs = rand::thread_rng().gen_range(1..=10);
    let num_outputs = rand::thread_rng().gen_range(0..=5);

    let mut input = vec![TxIn {
        previous_output: outpoint,
        script_sig: ScriptBuf::new(),
        sequence: bitcoin::Sequence(0xFFFFFFFF),
        witness: bitcoin::Witness::new(),
    }];

    for _ in 0..(num_inputs - 1) {
        input.push(random_input());
    }

    let mut output = Vec::with_capacity(num_outputs);
    for _ in 0..num_outputs {
        output.push(random_output());
    }

    bitcoin::Transaction {
        version: bitcoin::transaction::Version(2),
        lock_time: bitcoin::absolute::LockTime::Blocks(bitcoin::absolute::Height::ZERO),
        input,
        output,
    }
}

pub fn generate_blocks(bitcoind: &mut Client, blocks: usize) {
    let addr = bitcoind.new_address().unwrap();
    bitcoind.generate_to_address(blocks, &addr).unwrap();
}

pub fn send(bitcoind: &mut Client, addr: Address, btc: f64) -> Option<Txid> {
    bitcoind
        .send_to_address(&addr, Amount::from_btc(btc).unwrap())
        .unwrap()
        .txid()
        .ok()
}

pub fn bitcoind_with_txindex() -> corepc_node::Node {
    let mut conf = corepc_node::Conf::default();
    conf.args.push("-txindex");
    let mut node = corepc_node::Node::from_downloaded_with_conf(&conf).unwrap();
    generate_blocks(&mut node.client, 110);
    node
}

pub fn get_tx(bitcoind: &mut Client, txid: Txid) -> Option<Transaction> {
    bitcoind
        .get_raw_transaction(txid)
        .unwrap()
        .transaction()
        .ok()
}

pub fn get_height(bitcoind: &mut Client) -> u64 {
    bitcoind.get_block_count().unwrap().0
}

pub fn get_tx_height(bitcoind: &mut Client, txid: Txid) -> Option<u64> {
    let hash = bitcoind
        .get_raw_transaction_verbose(txid)
        .unwrap()
        .block_hash?;
    let hash = BlockHash::from_str(&hash).unwrap();
    bitcoind.get_block(hash).unwrap().bip34_block_height().ok()
}

#[test]
fn gen_txid() {
    setup_logger();
    let tx = txid();
    log::debug!("{tx:?}");
    let tx = txid();
    log::debug!("{tx:?}");
}
