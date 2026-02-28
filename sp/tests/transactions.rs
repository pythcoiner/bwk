//! Transaction building and signing tests for bwk-sp.

mod common;

use backend_blindbit_native_non_async::{BlindbitBackend, UreqClient};
use bitcoin::OutPoint;
use blindbitd::BlindbitD;
use bwk_utils::test as bwk_test;

use common::{test_account_named, test_mnemonic, wait_for_sync_and_index};

/// Tests error on insufficient funds for transaction.
///
/// This test verifies that create_transaction returns an error when
/// the account has no spendable coins.
#[test]
fn test_create_transaction_insufficient_funds() {
    use bitcoin::Amount;
    use bwk_tx::Fees;
    use spdk_core::RecipientAddress;

    // 1. Create BlindbitD
    let mut bbd = BlindbitD::new().unwrap();
    let client = UreqClient::new();
    let backend = BlindbitBackend::new(bbd.url(), client).unwrap();

    // 2. Get bitcoind client
    let mut bitcoind_node = bbd.bitcoin().unwrap();
    let bitcoind = &mut bitcoind_node.client;

    // 3. Generate blocks
    bwk_test::generate_blocks(bitcoind, 100);
    wait_for_sync_and_index(&backend, 100);

    // 4. Create Account with no coins (fresh account)
    let account = test_account_named("test-insufficient-funds", &bbd.url());

    // 5. Verify account has no coins
    assert_eq!(
        account.balance(),
        0,
        "Fresh account should have zero balance"
    );
    assert!(
        account.coins().is_empty(),
        "Fresh account should have no coins"
    );

    // 6. Try to create a transaction (should fail due to no spendable coins)
    let sp_address = account.sp_address();
    let recipient = RecipientAddress::SpAddress(sp_address);

    let result = account.create_transaction(
        vec![(recipient, Amount::from_sat(100_000))],
        Fees::MilliSatsVb(1000),
    );

    // 7. Verify it returns an error
    assert!(
        result.is_err(),
        "create_transaction should fail with no coins"
    );
    let error = result.unwrap_err();
    let error_msg = error.to_string();
    assert!(
        error_msg.contains("no spendable coins") || error_msg.contains("insufficient"),
        "Error should indicate no spendable coins, got: {}",
        error_msg
    );

}

/// Tests drain transaction uses all UTXOs.
///
/// This test verifies:
/// - Account detects multiple SP outputs
/// - Account.create_drain_transaction uses ALL available UTXOs
/// - The resulting transaction has a single output (drain target)
/// - All coins are used as inputs
#[test]
fn test_create_drain_transaction() {
    use bwk_sign::HotSigner;
    use bwk_tx::Fees;
    use common::{generate_recipient_pubkey, swap_to_sp};
    use spdk_core::RecipientAddress;

    let secp = bitcoin::secp256k1::Secp256k1::new();
    let network = bitcoin::Network::Regtest;

    // 1. Create BlindbitD
    let mut bbd = BlindbitD::new().unwrap();

    // 2. Get bitcoind client
    let mut bitcoind_node = bbd.bitcoin().unwrap();
    let bitcoind = &mut bitcoind_node.client;

    // 3. Generate 101 blocks (coinbase maturity)
    bwk_test::generate_blocks(bitcoind, 101);
    wait_for_sync_and_index(
        &BlindbitBackend::new(bbd.url(), UreqClient::new()).unwrap(),
        101,
    );

    // 4. Create Account
    let mnemonic_str = test_mnemonic();
    let mut account = test_account_named("test-drain", &bbd.url());

    // 5. Create taproot signer for funding
    let tr_signer = HotSigner::new_taproot_from_mnemonics(network, mnemonic_str)
        .expect("create taproot signer");

    let sp_address = account.sp_address();
    let mut sp_txids = Vec::new();
    let mut final_height = 101u32;

    // Create 2 SP outputs
    for i in 0..2 {
        let i = i as u32;
        let (taproot_addr, sk) = tr_signer.taproot_receive_address_and_key(i);

        // Fund the taproot address
        let fund_txid = bwk_test::send(bitcoind, taproot_addr.clone(), 0.1).expect("fund taproot");
        bwk_test::generate_blocks(bitcoind, 2);
        let current_height = 101 + (i + 1) * 2;
        wait_for_sync_and_index(
            &BlindbitBackend::new(bbd.url(), UreqClient::new()).unwrap(),
            current_height,
        );

        // Get the funded UTXO
        let tx = bwk_test::get_tx(bitcoind, fund_txid).expect("get tx");
        let (index, txout) = bwk_test::txouts_for(&taproot_addr, &tx)
            .into_iter()
            .next()
            .expect("find txout");
        let outpoint = OutPoint {
            txid: fund_txid,
            vout: index as u32,
        };

        // Create SP transaction
        let recipient_pubkey = generate_recipient_pubkey(sk, outpoint, &txout, sp_address, &secp)
            .expect("generate recipient pubkey");

        let sp_tx = swap_to_sp(
            sk,
            outpoint,
            txout,
            recipient_pubkey,
            bitcoin::Amount::from_sat(1000),
            &secp,
        )
        .expect("create sp tx");

        // Broadcast and mine
        let sp_txid = sp_tx.compute_txid();
        bitcoind
            .send_raw_transaction(&sp_tx)
            .expect("broadcast sp tx");
        bwk_test::generate_blocks(bitcoind, 1);
        final_height = bwk_test::get_tx_height(bitcoind, sp_txid).expect("get tx height") as u32;
        wait_for_sync_and_index(
            &BlindbitBackend::new(bbd.url(), UreqClient::new()).unwrap(),
            final_height,
        );

        sp_txids.push(sp_txid);
    }

    // 6. Scan to detect SP outputs
    account
        .scan_blocks(Some(1), Some(final_height))
        .expect("scan");

    // 7. Verify we have 2 coins
    assert_eq!(account.coins().len(), 2, "Should have 2 coins");

    // 8. Create a drain transaction to ourselves
    let drain_recipient = RecipientAddress::SpAddress(account.sp_address());
    let fees = Fees::MilliSatsVb(1000);

    let unsigned_tx = account
        .create_drain_transaction(drain_recipient, fees)
        .expect("create drain transaction");

    // 9. Verify drain transaction uses ALL 2 inputs
    assert_eq!(
        unsigned_tx.selected_utxos.len(),
        2,
        "Drain transaction should use all 2 UTXOs as inputs"
    );

    // 10. Verify the selected UTXOs match our SP outputs
    for sp_txid in &sp_txids {
        let expected_op = OutPoint {
            txid: *sp_txid,
            vout: 0,
        };
        assert!(
            unsigned_tx
                .selected_utxos
                .iter()
                .any(|(op, _)| *op == expected_op),
            "Drain should include output {}:0",
            sp_txid
        );
    }

    // 11. Verify there's exactly 1 recipient (the drain target)
    assert_eq!(
        unsigned_tx.recipients.len(),
        1,
        "Drain transaction should have exactly 1 recipient"
    );

}

/// Tests full flow: create SP output, scan, create tx, sign, broadcast, mine, rescan.
///
/// This test verifies the complete send flow using bwk_sp::Account:
/// 1. Create SP output and scan to detect it
/// 2. Create, finalize, and sign a transaction spending the output
/// 3. Broadcast the signed transaction via bitcoind
/// 4. Mine a block to confirm the transaction
/// 5. Rescan and verify new outputs are received (change from the transaction)
#[test]
fn test_sign_and_broadcast_full_flow() {
    use bitcoin::Amount;
    use bwk_sign::HotSigner;
    use bwk_tx::Fees;
    use common::{generate_recipient_pubkey, swap_to_sp};
    use spdk_core::RecipientAddress;

    let secp = bitcoin::secp256k1::Secp256k1::new();
    let network = bitcoin::Network::Regtest;

    // 1. Create BlindbitD
    let mut bbd = BlindbitD::new().unwrap();

    // 2. Get bitcoind client
    let mut bitcoind_node = bbd.bitcoin().unwrap();
    let bitcoind = &mut bitcoind_node.client;

    // 3. Generate 101 blocks (coinbase maturity)
    bwk_test::generate_blocks(bitcoind, 101);
    wait_for_sync_and_index(
        &BlindbitBackend::new(bbd.url(), UreqClient::new()).unwrap(),
        101,
    );

    // 4. Create Account
    let mnemonic_str = test_mnemonic();
    let mut account = test_account_named("test-sign-broadcast", &bbd.url());

    // 5. Create taproot signer for funding
    let tr_signer = HotSigner::new_taproot_from_mnemonics(network, mnemonic_str)
        .expect("create taproot signer");
    let (taproot_addr, sk) = tr_signer.taproot_receive_address_and_key(0);

    // 6. Fund the taproot address with 0.5 BTC
    let fund_txid = bwk_test::send(bitcoind, taproot_addr.clone(), 0.5).expect("fund taproot");
    bwk_test::generate_blocks(bitcoind, 2);
    wait_for_sync_and_index(
        &BlindbitBackend::new(bbd.url(), UreqClient::new()).unwrap(),
        103,
    );

    // 7. Get the funded UTXO
    let tx = bwk_test::get_tx(bitcoind, fund_txid).expect("get tx");
    let (index, txout) = bwk_test::txouts_for(&taproot_addr, &tx)
        .into_iter()
        .next()
        .expect("find txout");
    let outpoint = OutPoint {
        txid: fund_txid,
        vout: index as u32,
    };

    // 8. Create SP transaction to fund the account
    let sp_address = account.sp_address();
    let recipient_pubkey = generate_recipient_pubkey(sk, outpoint, &txout, sp_address, &secp)
        .expect("generate recipient pubkey");

    let sp_tx = swap_to_sp(
        sk,
        outpoint,
        txout,
        recipient_pubkey,
        bitcoin::Amount::from_sat(1000),
        &secp,
    )
    .expect("create sp tx");

    // 9. Broadcast and mine
    let sp_txid = sp_tx.compute_txid();
    bitcoind
        .send_raw_transaction(&sp_tx)
        .expect("broadcast sp tx");
    bwk_test::generate_blocks(bitcoind, 1);
    let sp_tx_height = bwk_test::get_tx_height(bitcoind, sp_txid).expect("get tx height") as u32;
    wait_for_sync_and_index(
        &BlindbitBackend::new(bbd.url(), UreqClient::new()).unwrap(),
        sp_tx_height,
    );

    // 10. Scan to detect the SP output
    account
        .scan_blocks(Some(1), Some(sp_tx_height))
        .expect("scan");

    // 11. Verify we found the coin
    assert_eq!(account.coins().len(), 1, "Should have 1 coin");
    let original_balance = account.balance();
    assert!(original_balance > 0, "Should have positive balance");

    // 12. Create a transaction sending to ourselves (creates change)
    let send_amount = Amount::from_sat(100_000); // 0.001 BTC
    let recipient_addr = RecipientAddress::SpAddress(account.sp_address());
    let recipients = vec![(recipient_addr, send_amount)];
    let fees = Fees::MilliSatsVb(1000); // 1 sat/vB

    let unsigned_tx = account
        .create_transaction(recipients, fees)
        .expect("create transaction");

    // Verify the unsigned transaction structure
    assert!(
        !unsigned_tx.selected_utxos.is_empty(),
        "Transaction should have at least 1 input"
    );
    let expected_input_op = OutPoint {
        txid: sp_txid,
        vout: 0,
    };
    assert!(
        unsigned_tx
            .selected_utxos
            .iter()
            .any(|(op, _)| *op == expected_input_op),
        "Transaction should use our SP output as input"
    );
    assert!(
        !unsigned_tx.recipients.is_empty(),
        "Transaction should have at least 1 recipient"
    );

    // 13. Finalize transaction
    let finalized_tx = account
        .finalize_transaction(unsigned_tx)
        .expect("finalize transaction");

    // 14. Sign transaction
    let signed_tx = account
        .sign_transaction(finalized_tx)
        .expect("sign transaction");

    // Verify the signed transaction has witness data
    assert!(
        !signed_tx.input.is_empty(),
        "Signed transaction should have at least one input"
    );
    for (i, input) in signed_tx.input.iter().enumerate() {
        assert!(
            !input.witness.is_empty(),
            "Input {} should have witness data after signing",
            i
        );
        // Schnorr signature should be 64 bytes (or 65 with sighash type)
        let witness_len = input.witness.iter().next().map(|w| w.len()).unwrap_or(0);
        assert!(
            witness_len >= 64,
            "Witness for input {} should be at least 64 bytes (Schnorr sig), got {}",
            i,
            witness_len
        );
    }

    // 15. Broadcast the signed transaction using bitcoind
    let spend_txid = signed_tx.compute_txid();
    bitcoind
        .send_raw_transaction(&signed_tx)
        .expect("broadcast signed tx");

    // 16. Mine a block to confirm
    bwk_test::generate_blocks(bitcoind, 1);
    let spend_height =
        bwk_test::get_tx_height(bitcoind, spend_txid).expect("get spend tx height") as u32;
    wait_for_sync_and_index(
        &BlindbitBackend::new(bbd.url(), UreqClient::new()).unwrap(),
        spend_height,
    );

    // 17. Rescan to detect the new outputs from the spend transaction
    account
        .scan_blocks(Some(sp_tx_height + 1), Some(spend_height))
        .expect("rescan");

    // 18. Verify we received new outputs (the send-to-self and change)
    let coins = account.coins();
    let new_outputs: Vec<_> = coins
        .iter()
        .filter(|(op, _)| op.txid == spend_txid)
        .collect();
    assert!(
        !new_outputs.is_empty(),
        "Should have at least 1 new output from the spend transaction"
    );

    // Verify the new outputs have value (send amount + change)
    let new_total: u64 = new_outputs.iter().map(|(_, e)| e.amount_sat()).sum();
    assert!(new_total > 0, "New outputs should have value");
    // New total should be original minus fees
    assert!(
        new_total < original_balance,
        "New outputs total ({}) should be less than original ({}) due to fees",
        new_total,
        original_balance
    );

}

/// Tests sending to another SP wallet.
///
/// This test verifies:
/// - Create 2 accounts with different mnemonics
/// - Send SP output to account1
/// - Scan account1 to detect the output
/// - Create transaction FROM account1 TO account2's SP address
/// - Sign and broadcast the transaction
/// - Scan account2 to verify it received the output
#[test]
fn test_send_to_another_sp_wallet() {
    use bitcoin::Amount;
    use bwk_sign::HotSigner;
    use bwk_tx::Fees;
    use common::{
        generate_recipient_pubkey, swap_to_sp, test_account_with_mnemonic, test_mnemonic_2,
    };
    use spdk_core::RecipientAddress;

    let secp = bitcoin::secp256k1::Secp256k1::new();
    let network = bitcoin::Network::Regtest;

    // 1. Create BlindbitD
    let mut bbd = BlindbitD::new().unwrap();
    let client = UreqClient::new();
    let backend = BlindbitBackend::new(bbd.url(), client).unwrap();

    // 2. Get bitcoind client
    let mut bitcoind_node = bbd.bitcoin().unwrap();
    let bitcoind = &mut bitcoind_node.client;

    // 3. Generate 101 blocks (coinbase maturity)
    bwk_test::generate_blocks(bitcoind, 101);
    wait_for_sync_and_index(&backend, 101);

    // 4. Create TWO Accounts with DIFFERENT mnemonics
    let mut account1 = test_account_with_mnemonic("account1", test_mnemonic(), &bbd.url());
    let mut account2 = test_account_with_mnemonic("account2", test_mnemonic_2(), &bbd.url());

    // Verify the two accounts have different SP addresses
    let sp_addr1 = account1.sp_address();
    let sp_addr2 = account2.sp_address();
    assert_ne!(
        sp_addr1.to_string(),
        sp_addr2.to_string(),
        "Two accounts with different mnemonics should have different SP addresses"
    );

    // 5. Create taproot signer from mnemonic1 to fund the first account
    let tr_signer = HotSigner::new_taproot_from_mnemonics(network, test_mnemonic())
        .expect("create taproot signer");
    let (taproot_addr, sk) = tr_signer.taproot_receive_address_and_key(0);

    // 6. Fund the taproot address with 0.5 BTC
    let fund_txid = bwk_test::send(bitcoind, taproot_addr.clone(), 0.5).expect("fund taproot");
    bwk_test::generate_blocks(bitcoind, 2);
    wait_for_sync_and_index(&backend, 103);

    // 7. Get the funded UTXO
    let tx = bwk_test::get_tx(bitcoind, fund_txid).expect("get tx");
    let (index, txout) = bwk_test::txouts_for(&taproot_addr, &tx)
        .into_iter()
        .next()
        .expect("find txout");
    let outpoint = OutPoint {
        txid: fund_txid,
        vout: index as u32,
    };

    // 8. Create SP transaction TO account1
    let recipient_pubkey = generate_recipient_pubkey(sk, outpoint, &txout, sp_addr1, &secp)
        .expect("generate recipient pubkey");

    let sp_tx = swap_to_sp(
        sk,
        outpoint,
        txout,
        recipient_pubkey,
        bitcoin::Amount::from_sat(1000),
        &secp,
    )
    .expect("create sp tx");

    // 9. Broadcast and mine to fund account1
    let sp_txid = sp_tx.compute_txid();
    bitcoind
        .send_raw_transaction(&sp_tx)
        .expect("broadcast sp tx");
    bwk_test::generate_blocks(bitcoind, 1);
    let sp_tx_height = bwk_test::get_tx_height(bitcoind, sp_txid).expect("get tx height") as u32;
    wait_for_sync_and_index(&backend, sp_tx_height);

    // 10. Scan account1 to detect the SP output
    account1
        .scan_blocks(Some(1), Some(sp_tx_height))
        .expect("scan account1");

    // 11. Verify account1 found the output
    let coins1 = account1.coins();
    assert_eq!(coins1.len(), 1, "Account1 should have 1 coin");
    let account1_balance = account1.balance();
    assert!(
        account1_balance > 0,
        "Account1 should have positive balance"
    );

    // 12. Create transaction FROM account1 TO account2
    let send_amount = Amount::from_sat(100_000); // 0.001 BTC
    let recipient_addr2 = RecipientAddress::SpAddress(sp_addr2);
    let recipients = vec![(recipient_addr2, send_amount)];

    let unsigned_tx = account1
        .create_transaction(recipients, Fees::MilliSatsVb(1000))
        .expect("create transaction from account1 to account2");

    // 13. Finalize transaction
    let finalized_tx = account1
        .finalize_transaction(unsigned_tx)
        .expect("finalize transaction");

    // 14. Sign transaction with account1's keys
    let signed_tx = account1
        .sign_transaction(finalized_tx)
        .expect("sign transaction");

    // Verify signature
    assert!(
        !signed_tx.input[0].witness.is_empty(),
        "Signed transaction should have witness data"
    );

    // 15. Broadcast and mine the transaction
    let transfer_txid = signed_tx.compute_txid();
    bitcoind
        .send_raw_transaction(&signed_tx)
        .expect("broadcast transfer tx");
    bwk_test::generate_blocks(bitcoind, 1);
    let transfer_height =
        bwk_test::get_tx_height(bitcoind, transfer_txid).expect("get transfer tx height") as u32;
    wait_for_sync_and_index(&backend, transfer_height);

    // 16. Scan account2 to verify it received the output
    account2
        .scan_blocks(Some(1), Some(transfer_height))
        .expect("scan account2");

    // 17. Verify account2 found the output from account1
    let coins2 = account2.coins();
    assert_eq!(coins2.len(), 1, "Account2 should have exactly 1 coin");

    // Verify the coin is from the transfer transaction
    let received_outpoint = coins2.keys().next().expect("get outpoint");
    assert_eq!(
        received_outpoint.txid, transfer_txid,
        "Account2's coin should be from the transfer transaction"
    );

    // Verify the amount
    assert!(
        account2.balance() >= send_amount.to_sat(),
        "Account2 should have received at least {} sats",
        send_amount.to_sat()
    );

}
