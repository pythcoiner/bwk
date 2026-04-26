use std::{env, path::PathBuf};

use bwk_electrum::client::{Client, Error};
use electrsd::{
    bitcoind::{BitcoinD, P2P},
    ElectrsD,
};
use miniscript::bitcoin::{
    self, absolute, transaction, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness,
};

fn bootstrap_electrs() -> (String, u16, ElectrsD, BitcoinD) {
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

    let electrsd_conf = electrsd::Conf::default();
    let electrsd = ElectrsD::with_conf(electrs_path, &bitcoind, &electrsd_conf).unwrap();
    let (url, port) = electrsd.electrum_url.split_once(':').unwrap();
    let port = port.parse::<u16>().unwrap();
    (url.into(), port, electrsd, bitcoind)
}

/// A tx referencing a non-existent UTXO triggers `Response::Error` from electrs;
/// the new `broadcast_tx` must map that to `Error::Rejected` rather than the
/// opaque `Error::WrongResponse` returned by `broadcast`.
#[test]
fn broadcast_tx_rejection_is_typed() {
    let (url, port, _electrs, _bitcoind) = bootstrap_electrs();
    let mut client = Client::new(&url, port).expect("connect");

    // Random outpoint guaranteed not to exist on regtest.
    let bogus = Transaction {
        version: transaction::Version(2),
        lock_time: absolute::LockTime::Blocks(absolute::Height::ZERO),
        input: vec![TxIn {
            previous_output: OutPoint::new(
                bitcoin::Txid::from_raw_hash(bitcoin::hashes::Hash::all_zeros()),
                0,
            ),
            script_sig: ScriptBuf::new(),
            sequence: Sequence(0xFFFFFFFF),
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: bitcoin::Amount::from_sat(1_000),
            script_pubkey: ScriptBuf::new(),
        }],
    };

    match client.broadcast_tx(&bogus) {
        Err(Error::Rejected(msg)) => {
            assert!(!msg.is_empty(), "rejection message should be non-empty");
        }
        Err(other) => panic!("expected Rejected, got {other:?}"),
        Ok(txid) => panic!("expected rejection, server accepted tx {txid}"),
    }
}
