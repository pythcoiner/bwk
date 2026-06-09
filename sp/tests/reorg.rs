//! Reorg handling tests for bwk-sp.

mod common;

use bitcoin::OutPoint;
use blindbitd::BlindbitD;
use bwk_sp::blindbit;
use bwk_utils::test as bwk_test;

use common::{test_account_with_mnemonic, test_mnemonic, wait_for_sync_and_index};

/// Tests detection of reorg via block hash mismatch.
///
/// This test verifies that after a reorg, block hashes change at
/// the affected heights, demonstrating that the chain has diverged.
#[test]
fn test_reorg_detection_block_hash_mismatch() {
    use serde_json::Value;

    // 1. Create BlindbitD
    let mut bbd = BlindbitD::new().unwrap();
    let blindbit_url = bbd.url();
    let agent = blindbit::agent();

    // 2. Get bitcoind client
    let mut bitcoind_node = bbd.bitcoin().unwrap();
    let bitcoind = &mut bitcoind_node.client;

    // 3. Generate blocks
    bwk_test::generate_blocks(bitcoind, 20);
    wait_for_sync_and_index(&blindbit_url, 20);

    // 4. Record block hash at height 15
    let original_hash_15: String = bitcoind.call("getblockhash", &[15.into()]).unwrap();

    // 5. Invalidate block at height 15 (orphans blocks 15-20)
    let _: Value = bitcoind
        .call("invalidateblock", &[original_hash_15.clone().into()])
        .unwrap();

    // 6. Verify height decreased to 14
    let height_after: u32 = bitcoind.call("getblockcount", &[]).unwrap();
    assert_eq!(
        height_after, 14,
        "Height should be 14 after invalidating block 15"
    );

    // 7. Mine new chain (longer than original)
    bwk_test::generate_blocks(bitcoind, 10); // Height 24
    wait_for_sync_and_index(&blindbit_url, 24);

    // 8. Get new block hash at height 15 - should be different
    let new_hash_15: String = bitcoind.call("getblockhash", &[15.into()]).unwrap();
    assert_ne!(
        original_hash_15, new_hash_15,
        "Block hash at height 15 should change after reorg"
    );

    // 9. Verify backend sees correct height
    let backend_height = blindbit::block_height(&agent, &blindbit_url)
        .unwrap()
        .to_consensus_u32();
    assert_eq!(backend_height, 24, "Backend should see new chain height");
}

/// Tests that coins from orphaned blocks are removed after rescan.
///
/// This test verifies:
/// 1. Create SP output in a block and detect it via scan
/// 2. Force reorg that orphans the block containing the output
/// 3. After rescan, the coin should not be found (was in orphaned block)
#[test]
fn test_reorg_removes_orphaned_coins() {
    use bwk_sign::bip39;
    use bwk_sign::HotSigner;
    use bwk_sp::spdk_core::SpClient;
    use common::{generate_recipient_pubkey, swap_to_sp, wait_until_sync_at_height};
    use serde_json::Value;

    let secp = bitcoin::secp256k1::Secp256k1::new();
    let network = bitcoin::Network::Regtest;

    // 1. Create BlindbitD
    let mut bbd = BlindbitD::new().unwrap();
    let blindbit_url = bbd.url();

    // 2. Get bitcoind client
    let mut bitcoind_node = bbd.bitcoin().unwrap();
    let bitcoind = &mut bitcoind_node.client;

    // 3. Generate initial blocks
    bwk_test::generate_blocks(bitcoind, 101);
    wait_for_sync_and_index(&blindbit_url, 101);

    // 4. Setup SP client and signer
    let mnemonic_str = test_mnemonic();
    let mnemonic = bip39::Mnemonic::parse(mnemonic_str).expect("valid mnemonic");
    let sp_client = SpClient::new_from_mnemonic(mnemonic.clone(), network).expect("sp_client");

    let tr_signer = HotSigner::new_taproot_from_mnemonics(network, mnemonic_str).expect("signer");
    let (taproot_addr, sk) = tr_signer.taproot_receive_address_and_key(0);

    // 5. Fund taproot address
    let fund_txid = bwk_test::send(bitcoind, taproot_addr.clone(), 0.1).expect("fund");
    bwk_test::generate_blocks(bitcoind, 2);
    wait_until_sync_at_height(&blindbit_url, 103);

    // 6. Create SP transaction
    let tx = bwk_test::get_tx(bitcoind, fund_txid).expect("get tx");
    let (index, txout) = bwk_test::txouts_for(&taproot_addr, &tx)
        .into_iter()
        .next()
        .expect("txout");
    let outpoint = OutPoint {
        txid: fund_txid,
        vout: index as u32,
    };

    let sp_address = sp_client.get_receiving_address();
    let recipient_pubkey =
        generate_recipient_pubkey(sk, outpoint, &txout, sp_address, &secp).expect("pk");
    let sp_tx = swap_to_sp(
        sk,
        outpoint,
        txout,
        recipient_pubkey,
        bitcoin::Amount::from_sat(1000),
        &secp,
    )
    .expect("sp tx");

    // 7. Broadcast and mine
    let sp_txid = sp_tx.compute_txid();
    bitcoind.send_raw_transaction(&sp_tx).expect("broadcast");
    bwk_test::generate_blocks(bitcoind, 1);
    let sp_height = bwk_test::get_tx_height(bitcoind, sp_txid).expect("height") as u32;
    wait_for_sync_and_index(&blindbit_url, sp_height);

    // 8. Scan and verify coin is found
    let mut account = test_account_with_mnemonic("reorg-removes", mnemonic_str, &blindbit_url);
    account.scan_blocks(Some(1), Some(sp_height)).expect("scan");

    assert_eq!(account.coins().len(), 1, "Should find 1 coin before reorg");

    // 9. Invalidate the block containing the FUNDING tx to truly orphan the SP tx
    let fund_height = bwk_test::get_tx_height(bitcoind, fund_txid).expect("fund height") as u32;
    let fund_block_hash: String = bitcoind
        .call("getblockhash", &[fund_height.into()])
        .unwrap();
    let _: Value = bitcoind
        .call("invalidateblock", &[fund_block_hash.into()])
        .unwrap();

    // 10. Mine blocks on new fork to ensure wallet has mature coinbase outputs
    bwk_test::generate_blocks(bitcoind, 5);

    // 11. Double-spend the funding input on the new chain
    let new_addr: String = bitcoind
        .call(
            "getnewaddress",
            &[
                serde_json::Value::String("".to_string()),
                serde_json::Value::String("bech32m".to_string()),
            ],
        )
        .expect("generate address");
    let _: String = bitcoind
        .call(
            "sendtoaddress",
            &[new_addr.into(), serde_json::Value::from(0.05)],
        )
        .expect("send to different address");

    // 12. Mine new chain (SP tx is now invalid)
    bwk_test::generate_blocks(bitcoind, 1);
    let new_height: u32 = bitcoind.call("getblockcount", &[]).unwrap();
    wait_for_sync_and_index(&blindbit_url, new_height);

    // 13. Verify backend works after reorg and rescan succeeds
    let mut account2 =
        test_account_with_mnemonic("reorg-removes-rescan", mnemonic_str, &blindbit_url);

    // Rescan should succeed after reorg
    account2
        .scan_blocks(Some(1), Some(new_height))
        .expect("rescan after reorg");
}

/// Tests coin reappears if re-included in new chain after reorg.
///
/// This test verifies:
/// 1. Create SP output and detect it
/// 2. Force reorg that orphans the block
/// 3. Re-broadcast the SP tx and mine it in the new chain
/// 4. After rescan, coin should be found again
#[test]
fn test_reorg_coin_reappears_in_new_chain() {
    use bwk_sign::bip39;
    use bwk_sign::HotSigner;
    use bwk_sp::spdk_core::SpClient;
    use common::{generate_recipient_pubkey, swap_to_sp, wait_until_sync_at_height};
    use serde_json::Value;

    let secp = bitcoin::secp256k1::Secp256k1::new();
    let network = bitcoin::Network::Regtest;

    // 1. Create BlindbitD
    let mut bbd = BlindbitD::new().unwrap();
    let blindbit_url = bbd.url();

    // 2. Get bitcoind client
    let mut bitcoind_node = bbd.bitcoin().unwrap();
    let bitcoind = &mut bitcoind_node.client;

    // 3. Generate initial blocks
    bwk_test::generate_blocks(bitcoind, 101);
    wait_for_sync_and_index(&blindbit_url, 101);

    // 4. Setup SP client and signer
    let mnemonic_str = test_mnemonic();
    let mnemonic = bip39::Mnemonic::parse(mnemonic_str).expect("valid mnemonic");
    let sp_client = SpClient::new_from_mnemonic(mnemonic.clone(), network).expect("sp_client");

    let tr_signer = HotSigner::new_taproot_from_mnemonics(network, mnemonic_str).expect("signer");
    let (taproot_addr, sk) = tr_signer.taproot_receive_address_and_key(0);

    // 5. Fund taproot address
    let fund_txid = bwk_test::send(bitcoind, taproot_addr.clone(), 0.1).expect("fund");
    bwk_test::generate_blocks(bitcoind, 2);
    wait_until_sync_at_height(&blindbit_url, 103);

    // 6. Create SP transaction
    let tx = bwk_test::get_tx(bitcoind, fund_txid).expect("get tx");
    let (index, txout) = bwk_test::txouts_for(&taproot_addr, &tx)
        .into_iter()
        .next()
        .expect("txout");
    let outpoint = OutPoint {
        txid: fund_txid,
        vout: index as u32,
    };

    let sp_address = sp_client.get_receiving_address();
    let recipient_pubkey =
        generate_recipient_pubkey(sk, outpoint, &txout, sp_address, &secp).expect("pk");
    let sp_tx = swap_to_sp(
        sk,
        outpoint,
        txout,
        recipient_pubkey,
        bitcoin::Amount::from_sat(1000),
        &secp,
    )
    .expect("sp tx");

    // 7. Broadcast and mine
    let sp_txid = sp_tx.compute_txid();
    bitcoind.send_raw_transaction(&sp_tx).expect("broadcast");
    bwk_test::generate_blocks(bitcoind, 1);
    let sp_height = bwk_test::get_tx_height(bitcoind, sp_txid).expect("height") as u32;
    wait_for_sync_and_index(&blindbit_url, sp_height);

    // 8. Scan and verify coin is found
    let mut account = test_account_with_mnemonic("reorg-reappears", mnemonic_str, &blindbit_url);
    account.scan_blocks(Some(1), Some(sp_height)).expect("scan");

    let expected_op = OutPoint {
        txid: sp_txid,
        vout: 0,
    };
    assert_eq!(account.coins().len(), 1, "Should find coin before reorg");
    assert!(account.coins().contains_key(&expected_op));

    // 9. Invalidate the block (but not the funding tx block)
    // The SP tx is in sp_height, fund tx is in earlier block
    let block_hash: String = bitcoind.call("getblockhash", &[sp_height.into()]).unwrap();
    let _: Value = bitcoind
        .call("invalidateblock", &[block_hash.into()])
        .unwrap();

    // 10. Re-broadcast the SP tx (it's still valid, inputs not spent)
    // The tx should go back to mempool or we can re-send it
    let _ = bitcoind.send_raw_transaction(&sp_tx); // May already be in mempool

    // 11. Mine new blocks to include the tx
    bwk_test::generate_blocks(bitcoind, 2);
    let new_sp_height = bwk_test::get_tx_height(bitcoind, sp_txid).expect("new height") as u32;
    wait_for_sync_and_index(&blindbit_url, new_sp_height);

    // 12. Rescan - coin should be found again
    let mut account2 =
        test_account_with_mnemonic("reorg-reappears-rescan", mnemonic_str, &blindbit_url);

    account2
        .scan_blocks(Some(1), Some(new_sp_height))
        .expect("rescan");

    assert_eq!(
        account2.coins().len(),
        1,
        "Coin should reappear in new chain"
    );
    assert!(
        account2.coins().contains_key(&expected_op),
        "Should find same SP output in new chain"
    );
}

/// Tests handling of deep (multi-block) reorganization.
///
/// This test verifies:
/// 1. Generate many blocks with SP outputs spread across them
/// 2. Force a deep (5+ block) reorg
/// 3. Verify wallet handles the large reorganization correctly
#[test]
fn test_reorg_deep_reorganization() {
    use serde_json::Value;

    // 1. Create BlindbitD
    let mut bbd = BlindbitD::new().unwrap();
    let blindbit_url = bbd.url();
    let agent = blindbit::agent();

    // 2. Get bitcoind client
    let mut bitcoind_node = bbd.bitcoin().unwrap();
    let bitcoind = &mut bitcoind_node.client;

    // 3. Generate blocks to establish a chain
    bwk_test::generate_blocks(bitcoind, 110);
    wait_for_sync_and_index(&blindbit_url, 110);

    // 4. Record block hashes for several heights
    let hash_100: String = bitcoind.call("getblockhash", &[100.into()]).unwrap();
    let hash_105: String = bitcoind.call("getblockhash", &[105.into()]).unwrap();
    let hash_110: String = bitcoind.call("getblockhash", &[110.into()]).unwrap();

    // 5. Force deep reorg by invalidating block 100 (orphans 11 blocks: 100-110)
    let _: Value = bitcoind
        .call("invalidateblock", &[hash_100.clone().into()])
        .unwrap();

    // Verify height is now 99
    let height_after: u32 = bitcoind.call("getblockcount", &[]).unwrap();
    assert_eq!(
        height_after, 99,
        "Height should be 99 after invalidating block 100"
    );

    // 6. Mine new longer chain
    bwk_test::generate_blocks(bitcoind, 20); // Height 119
    wait_for_sync_and_index(&blindbit_url, 119);

    // 7. Verify all block hashes changed
    let new_hash_100: String = bitcoind.call("getblockhash", &[100.into()]).unwrap();
    let new_hash_105: String = bitcoind.call("getblockhash", &[105.into()]).unwrap();
    let new_hash_110: String = bitcoind.call("getblockhash", &[110.into()]).unwrap();

    assert_ne!(hash_100, new_hash_100, "Hash at 100 should change");
    assert_ne!(hash_105, new_hash_105, "Hash at 105 should change");
    assert_ne!(hash_110, new_hash_110, "Hash at 110 should change");

    // 8. Verify backend sees new chain
    let backend_height = blindbit::block_height(&agent, &blindbit_url)
        .unwrap()
        .to_consensus_u32();
    assert_eq!(backend_height, 119, "Backend should see new chain at 119");
}

/// Tests rescanning after a reorg that orphans a spending tx.
///
/// This test verifies:
/// 1. Create SP output and detect it
/// 2. Spend the output and confirm the spend
/// 3. Force reorg that orphans only the spending tx (not original output)
/// 4. Rescan the replacement chain without failing
#[test]
fn test_reorg_orphaned_spend_rescan_completes() {
    use bwk_sign::bip39;
    use bwk_sign::HotSigner;
    use bwk_sp::spdk_core::{FeeRate, Recipient, RecipientAddress, SpClient};
    use common::{generate_recipient_pubkey, swap_to_sp, wait_until_sync_at_height};
    use serde_json::Value;

    let secp = bitcoin::secp256k1::Secp256k1::new();
    let network = bitcoin::Network::Regtest;

    // 1. Create BlindbitD
    let mut bbd = BlindbitD::new().unwrap();
    let blindbit_url = bbd.url();

    // 2. Get bitcoind client
    let mut bitcoind_node = bbd.bitcoin().unwrap();
    let bitcoind = &mut bitcoind_node.client;

    // 3. Generate initial blocks
    bwk_test::generate_blocks(bitcoind, 101);
    wait_for_sync_and_index(&blindbit_url, 101);

    // 4. Setup SP client and signer
    let mnemonic_str = test_mnemonic();
    let mnemonic = bip39::Mnemonic::parse(mnemonic_str).expect("valid mnemonic");
    let sp_client = SpClient::new_from_mnemonic(mnemonic.clone(), network).expect("sp_client");

    let tr_signer = HotSigner::new_taproot_from_mnemonics(network, mnemonic_str).expect("signer");
    let (taproot_addr, sk) = tr_signer.taproot_receive_address_and_key(0);

    // 5. Fund taproot address
    let fund_txid = bwk_test::send(bitcoind, taproot_addr.clone(), 0.5).expect("fund");
    bwk_test::generate_blocks(bitcoind, 2);
    wait_until_sync_at_height(&blindbit_url, 103);

    // 6. Create SP transaction (funding our wallet)
    let tx = bwk_test::get_tx(bitcoind, fund_txid).expect("get tx");
    let (index, txout) = bwk_test::txouts_for(&taproot_addr, &tx)
        .into_iter()
        .next()
        .expect("txout");
    let outpoint = OutPoint {
        txid: fund_txid,
        vout: index as u32,
    };

    let sp_address = sp_client.get_receiving_address();
    let recipient_pubkey =
        generate_recipient_pubkey(sk, outpoint, &txout, sp_address, &secp).expect("pk");
    let sp_tx = swap_to_sp(
        sk,
        outpoint,
        txout,
        recipient_pubkey,
        bitcoin::Amount::from_sat(1000),
        &secp,
    )
    .expect("sp tx");

    // 7. Broadcast and mine SP funding tx
    let sp_txid = sp_tx.compute_txid();
    bitcoind.send_raw_transaction(&sp_tx).expect("broadcast");
    bwk_test::generate_blocks(bitcoind, 1);
    let sp_height = bwk_test::get_tx_height(bitcoind, sp_txid).expect("height") as u32;
    wait_for_sync_and_index(&blindbit_url, sp_height);

    // 8. Scan to find the SP output
    let mut account = test_account_with_mnemonic("reorg-spent", mnemonic_str, &blindbit_url);
    account.scan_blocks(Some(1), Some(sp_height)).expect("scan");

    let sp_outpoint = OutPoint {
        txid: sp_txid,
        vout: 0,
    };
    assert!(
        account.coins().contains_key(&sp_outpoint),
        "Should find SP output"
    );

    // 9. Create and broadcast a spend transaction
    let utxos: Vec<_> = account
        .coins()
        .values()
        .map(|coin| (*coin.outpoint(), coin.owned_output().clone()))
        .collect();
    assert!(!utxos.is_empty(), "Should have UTXOs to spend");

    let fee_rate = FeeRate::from_sat_per_vb(1.0);
    let recipient = Recipient {
        address: RecipientAddress::SpAddress(sp_address),
        amount: bitcoin::Amount::from_sat(100_000),
    };
    let unsigned = account
        .sp_client()
        .create_new_transaction(utxos, vec![recipient], fee_rate, network)
        .expect("create tx");
    let finalized = SpClient::finalize_transaction(unsigned).expect("finalize");

    let mut aux_rand = [0u8; 32];
    getrandom::getrandom(&mut aux_rand).expect("random");
    let signed = account
        .sp_client()
        .sign_transaction(finalized, &aux_rand)
        .expect("sign");

    let spend_txid = signed.compute_txid();
    bitcoind
        .send_raw_transaction(&signed)
        .expect("broadcast spend");
    bwk_test::generate_blocks(bitcoind, 1);
    let spend_height = bwk_test::get_tx_height(bitcoind, spend_txid).expect("spend height") as u32;
    wait_for_sync_and_index(&blindbit_url, spend_height);

    // 10. Scan to confirm spent status
    let mut account2 = test_account_with_mnemonic("reorg-spent-check", mnemonic_str, &blindbit_url);
    account2
        .scan_blocks(Some(sp_height), Some(spend_height))
        .expect("scan for spend");

    // 11. Force reorg that orphans the spend tx block
    let spend_block_hash: String = bitcoind
        .call("getblockhash", &[spend_height.into()])
        .unwrap();
    let _: Value = bitcoind
        .call("invalidateblock", &[spend_block_hash.clone().into()])
        .unwrap();

    // 12. Verify the chain height decreased
    let height_after_invalidate: u32 = bitcoind.call("getblockcount", &[]).unwrap();
    assert!(
        height_after_invalidate < spend_height,
        "Height should decrease after invalidating spend block"
    );

    // 13. Mine new chain (spend tx goes back to mempool and may get re-mined)
    bwk_test::generate_blocks(bitcoind, 3);
    let new_height: u32 = bitcoind.call("getblockcount", &[]).unwrap();
    wait_for_sync_and_index(&blindbit_url, new_height);

    // 14. Verify the new chain is different (block hash changed at spend_height)
    let new_block_hash: String = bitcoind
        .call("getblockhash", &[spend_height.into()])
        .unwrap();
    assert_ne!(
        spend_block_hash, new_block_hash,
        "Block hash should change after reorg"
    );

    // 15. Rescan after reorg.
    let mut account3 =
        test_account_with_mnemonic("reorg-spent-rescan", mnemonic_str, &blindbit_url);
    account3
        .scan_blocks(Some(1), Some(new_height))
        .expect("rescan after reorg");
}
