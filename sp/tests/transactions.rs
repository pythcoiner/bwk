//! Transaction building and signing tests for bwk-sp.
//!
//! Covers 14 spending scenarios:
//! - SP-only inputs (to SP, taproot, segwit, and mixed outputs)
//! - Mixed SP + BIP32 inputs (to standard outputs)

mod common;

use bitcoin::Network;
use bwk_sp::{SpRecipientAddress, TxBuilderSpExt};
use bwk_tx::transaction::Amount;
use bwk_tx::Recipient;

use common::{test_mnemonic, test_mnemonic_2, TestEnv};

/// Create a drain recipient for a standard address (Amount::Max, no change).
fn drain_to(addr: bitcoin::Address) -> Recipient {
    Recipient {
        address: addr.as_unchecked().clone(),
        amount: Amount::Max(None),
        label: None,
        origin: None,
        descriptor: None,
    }
}

/// Empty account cannot build a transaction.
#[test]
fn test_insufficient_funds() {
    let env = TestEnv::new();
    let account = env.sp_account("test-insufficient");

    assert_eq!(account.balance(), 0);
    assert!(account.coins().is_empty());

    let sp_address = account.sp_address();
    let mut builder = account.tx_builder().feerate(1000);
    builder.send_to_sp(sp_address, 100_000);

    let coins = builder.select_coins(100_000, 1000);
    assert!(coins.is_empty());
    assert!(builder.generate().is_err());
}

/// Drain uses all UTXOs.
#[test]
fn test_drain() {
    let mut env = TestEnv::new();
    let mut account = env.sp_account("test-drain");

    // Fund with 2 separate SP outputs
    env.fund_sp(&mut account, 0.1);
    env.fund_sp(&mut account, 0.1);
    assert_eq!(account.coins().len(), 2);

    let mut builder = account.tx_builder().feerate(1000);
    let mut drain_recip = SpRecipientAddress::from_sp(account.sp_address(), 0, Network::Regtest);
    drain_recip.amount = Amount::Max(None);
    builder.add_output(drain_recip);
    builder.drain_inputs();

    let psbt = builder.generate().unwrap();
    assert_eq!(psbt.unsigned_tx.input.len(), 2);
}

/// SP → SP (self): send to own address, verify change.
#[test]
fn test_sp_to_sp_self() {
    let mut env = TestEnv::new();
    let mut account = env.sp_account("test-self");
    env.fund_sp(&mut account, 0.5);

    let original_balance = account.balance();
    assert!(original_balance > 0);

    let mut builder = account.tx_builder().feerate(1000);
    builder.send_to_sp(account.sp_address(), 100_000);
    let coins = builder.select_coins(100_000, 1000);
    assert!(!coins.is_empty());
    for c in coins {
        builder.add_input(c);
    }
    let mut psbt = builder.generate().unwrap();
    let tx = account.sign_and_finalize(&mut psbt).unwrap();
    env.broadcast_and_mine(&tx);

    // Rescan to detect change outputs
    account.scan_blocks(Some(1), Some(env.height)).unwrap();
    let new_outputs: Vec<_> = account
        .coins()
        .into_iter()
        .filter(|(op, _)| op.txid == tx.compute_txid())
        .collect();
    assert!(!new_outputs.is_empty());
}

/// SP → SP (other wallet): verify recipient detects the output.
#[test]
fn test_sp_to_sp_other() {
    let mut env = TestEnv::new();
    let mut sender = env.sp_account_with_mnemonic("sender", test_mnemonic());
    let mut receiver = env.sp_account_with_mnemonic("receiver", test_mnemonic_2());

    assert_ne!(
        sender.sp_address().to_string(),
        receiver.sp_address().to_string()
    );

    env.fund_sp(&mut sender, 0.5);

    let send_amount = 100_000u64;
    let mut builder = sender.tx_builder().feerate(1000);
    builder.send_to_sp(receiver.sp_address(), send_amount);
    let coins = builder.select_coins(send_amount, 1000);
    assert!(!coins.is_empty());
    for c in coins {
        builder.add_input(c);
    }
    let mut psbt = builder.generate().unwrap();
    let tx = sender.sign_and_finalize(&mut psbt).unwrap();
    let txid = tx.compute_txid();
    env.broadcast_and_mine(&tx);

    // Receiver scans and finds the output
    receiver.scan_blocks(Some(1), Some(env.height)).unwrap();
    let receiver_coins = receiver.coins();
    assert_eq!(receiver_coins.len(), 1);
    let received_op = receiver_coins.keys().next().unwrap();
    assert_eq!(received_op.txid, txid);
    assert!(receiver.balance() >= send_amount);
}

/// SP → taproot address.
#[test]
fn test_sp_to_taproot() {
    let mut env = TestEnv::new();
    let mut account = env.sp_account("test");
    env.fund_sp(&mut account, 0.5);

    let tr_addr = env.taproot_addr(0);
    let mut builder = account.tx_builder().feerate(1000);
    builder.send_to(tr_addr, 100_000);
    let coins = builder.select_coins(100_000, 1000);
    for c in coins {
        builder.add_input(c);
    }
    let mut psbt = builder.generate().unwrap();
    let tx = account.sign_and_finalize(&mut psbt).unwrap();
    env.broadcast_and_mine(&tx);
}

/// SP → segwit (P2WPKH) address.
#[test]
fn test_sp_to_segwit() {
    let mut env = TestEnv::new();
    let mut account = env.sp_account("test");
    env.fund_sp(&mut account, 0.5);

    let sw_addr = env.segwit_addr(0);
    let mut builder = account.tx_builder().feerate(1000);
    builder.send_to(sw_addr, 100_000);
    let coins = builder.select_coins(100_000, 1000);
    for c in coins {
        builder.add_input(c);
    }
    let mut psbt = builder.generate().unwrap();
    let tx = account.sign_and_finalize(&mut psbt).unwrap();
    env.broadcast_and_mine(&tx);
}

/// SP → SP + taproot (mixed outputs).
#[test]
fn test_sp_to_mixed_sp_taproot() {
    let mut env = TestEnv::new();
    let mut account = env.sp_account("test");
    env.fund_sp(&mut account, 0.5);

    let mut builder = account.tx_builder().feerate(1000);
    builder.send_to_sp(account.sp_address(), 50_000);
    builder.send_to(env.taproot_addr(0), 50_000);
    let coins = builder.select_coins(100_000, 1000);
    for c in coins {
        builder.add_input(c);
    }
    let mut psbt = builder.generate().unwrap();
    let tx = account.sign_and_finalize(&mut psbt).unwrap();
    env.broadcast_and_mine(&tx);
}

/// SP → SP + segwit (mixed outputs).
#[test]
fn test_sp_to_mixed_sp_segwit() {
    let mut env = TestEnv::new();
    let mut account = env.sp_account("test");
    env.fund_sp(&mut account, 0.5);

    let mut builder = account.tx_builder().feerate(1000);
    builder.send_to_sp(account.sp_address(), 50_000);
    builder.send_to(env.segwit_addr(0), 50_000);
    let coins = builder.select_coins(100_000, 1000);
    for c in coins {
        builder.add_input(c);
    }
    let mut psbt = builder.generate().unwrap();
    let tx = account.sign_and_finalize(&mut psbt).unwrap();
    env.broadcast_and_mine(&tx);
}

/// 2 SP → SP + taproot (multiple SP inputs, mixed outputs).
#[test]
fn test_multi_sp_to_mixed_sp_taproot() {
    let mut env = TestEnv::new();
    let mut account = env.sp_account("test");
    env.fund_sp(&mut account, 0.1);
    env.fund_sp(&mut account, 0.1);
    assert_eq!(account.coins().len(), 2);

    let mut builder = account.tx_builder().feerate(1000);
    builder.send_to_sp(account.sp_address(), 50_000);
    builder.send_to(env.taproot_addr(0), 50_000);
    builder.drain_inputs();
    let mut psbt = builder.generate().unwrap();
    assert_eq!(psbt.unsigned_tx.input.len(), 2);

    let tx = account.sign_and_finalize(&mut psbt).unwrap();
    env.broadcast_and_mine(&tx);
}

/// 2 SP → taproot (drain to standard output).
#[test]
fn test_multi_sp_to_taproot() {
    let mut env = TestEnv::new();
    let mut account = env.sp_account("test");
    env.fund_sp(&mut account, 0.1);
    env.fund_sp(&mut account, 0.1);

    let mut builder = account.tx_builder().feerate(1000);
    builder.send_to(env.taproot_addr(0), 50_000);
    builder.drain_inputs();
    let mut psbt = builder.generate().unwrap();
    assert_eq!(psbt.unsigned_tx.input.len(), 2);

    let tx = account.sign_and_finalize(&mut psbt).unwrap();
    env.broadcast_and_mine(&tx);
}

/// SP + taproot → taproot (drain, no SP change).
#[test]
fn test_mixed_sp_taproot_to_taproot() {
    let mut env = TestEnv::new();
    let mut account = env.sp_account("test");
    env.fund_sp(&mut account, 0.1);
    env.add_taproot_sub_account(&mut account);
    let tr_coin = env.create_taproot_coin(0.1);

    let mut builder = account.tx_builder().feerate(1000);
    builder.add_output(drain_to(env.taproot_addr(1)));
    builder.drain_inputs();
    builder.add_input(tr_coin);
    let mut psbt = builder.generate().unwrap();
    let tx = account.sign_and_finalize(&mut psbt).unwrap();
    env.broadcast_and_mine(&tx);
}

/// SP + taproot → segwit (drain, no SP change).
#[test]
fn test_mixed_sp_taproot_to_segwit() {
    let mut env = TestEnv::new();
    let mut account = env.sp_account("test");
    env.fund_sp(&mut account, 0.1);
    env.add_taproot_sub_account(&mut account);
    let tr_coin = env.create_taproot_coin(0.1);

    let mut builder = account.tx_builder().feerate(1000);
    builder.add_output(drain_to(env.segwit_addr(0)));
    builder.drain_inputs();
    builder.add_input(tr_coin);
    let mut psbt = builder.generate().unwrap();
    let tx = account.sign_and_finalize(&mut psbt).unwrap();
    env.broadcast_and_mine(&tx);
}

/// SP + segwit → taproot (drain, no SP change).
#[test]
fn test_mixed_sp_segwit_to_taproot() {
    let mut env = TestEnv::new();
    let mut account = env.sp_account("test");
    env.fund_sp(&mut account, 0.1);
    env.add_segwit_sub_account(&mut account);
    let sw_coin = env.create_segwit_coin(0.1);

    let mut builder = account.tx_builder().feerate(1000);
    builder.add_output(drain_to(env.taproot_addr(0)));
    builder.drain_inputs();
    builder.add_input(sw_coin);
    let mut psbt = builder.generate().unwrap();
    let tx = account.sign_and_finalize(&mut psbt).unwrap();
    env.broadcast_and_mine(&tx);
}

/// SP + segwit → segwit (drain, no SP change).
#[test]
fn test_mixed_sp_segwit_to_segwit() {
    let mut env = TestEnv::new();
    let mut account = env.sp_account("test");
    env.fund_sp(&mut account, 0.1);
    env.add_segwit_sub_account(&mut account);
    let sw_coin = env.create_segwit_coin(0.1);

    let mut builder = account.tx_builder().feerate(1000);
    builder.add_output(drain_to(env.segwit_addr(1)));
    builder.drain_inputs();
    builder.add_input(sw_coin);
    let mut psbt = builder.generate().unwrap();
    let tx = account.sign_and_finalize(&mut psbt).unwrap();
    env.broadcast_and_mine(&tx);
}
