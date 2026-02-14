//! Test utilities for bwk-sp integration tests.
//!
//! This module provides:
//! - `MockBackend` for testing without real network
//! - Test fixtures (mnemonic, config, outpoints, owned outputs)
//! - Temporary directory helpers for persistence tests
//! - Blindbitd helpers for integration tests (Phase 10.4)

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::{thread, time::Duration};

use bitcoin::absolute::Height;
use bitcoin::hashes::Hash;
use bitcoin::{Amount, OutPoint, ScriptBuf, TxOut, Txid, XOnlyPublicKey};

#[allow(unused_imports)]
use backend_blindbit_native_non_async::BlindbitBackend;
use spdk_core::ChainBackend;

use bwk_sp::Config;
use spdk_core::{OutputSpendStatus, OwnedOutput};

//=============================================================================
// MockBackendError
//=============================================================================

/// Errors that can occur in MockBackend operations.
#[derive(Debug, thiserror::Error)]
pub enum MockBackendError {
    /// Simulated network failure
    #[error("simulated network failure after {0} calls")]
    SimulatedFailure(u32),
    /// Block not found (reserved for future use)
    #[error("block not found at height {0}")]
    #[allow(dead_code)]
    BlockNotFound(u32),
}

//=============================================================================
// MockBlock
//=============================================================================

/// A mock block for testing.
#[derive(Debug, Clone)]
pub struct MockBlock {
    /// Block height
    pub height: u32,
    /// Block hash as bytes (reserved for future use in reorg tests)
    #[allow(dead_code)]
    pub hash: [u8; 32],
    /// Outputs found in this block (outpoint -> owned output)
    pub outputs: Vec<(OutPoint, OwnedOutput)>,
    /// Inputs spent in this block (outpoints that were consumed)
    pub spent_inputs: Vec<OutPoint>,
}

impl MockBlock {
    /// Create a new mock block with no transactions.
    pub fn new(height: u32) -> Self {
        let mut hash = [0u8; 32];
        hash[0..4].copy_from_slice(&height.to_le_bytes());
        Self {
            height,
            hash,
            outputs: Vec::new(),
            spent_inputs: Vec::new(),
        }
    }

    /// Add an output to this block.
    pub fn with_output(mut self, outpoint: OutPoint, output: OwnedOutput) -> Self {
        self.outputs.push((outpoint, output));
        self
    }

    /// Add a spent input to this block.
    pub fn with_spent_input(mut self, outpoint: OutPoint) -> Self {
        self.spent_inputs.push(outpoint);
        self
    }
}

//=============================================================================
// MockBackend
//=============================================================================

/// A mock backend for testing scanner logic without real network.
///
/// This can be used to:
/// - Simulate different chain states
/// - Test error handling with `fail_after`
/// - Track call counts for retry testing
pub struct MockBackend {
    /// Predefined blocks
    blocks: Vec<MockBlock>,
    /// Simulate network failure after N calls (None = no failures)
    fail_after: Option<u32>,
    /// Track number of calls for retry testing
    call_count: AtomicU32,
    /// Current chain tip height
    tip_height: u32,
}

impl MockBackend {
    //-------------------------------------------------------------------------
    // Constructors
    //-------------------------------------------------------------------------

    /// Create a new MockBackend with specified tip height.
    pub fn new(tip_height: u32) -> Self {
        Self {
            blocks: Vec::new(),
            fail_after: None,
            call_count: AtomicU32::new(0),
            tip_height,
        }
    }

    /// Create a MockBackend with predefined blocks.
    pub fn with_blocks(blocks: Vec<MockBlock>) -> Self {
        let tip_height = blocks.iter().map(|b| b.height).max().unwrap_or(0);
        Self {
            blocks,
            fail_after: None,
            call_count: AtomicU32::new(0),
            tip_height,
        }
    }

    //-------------------------------------------------------------------------
    // Configuration (builder pattern)
    //-------------------------------------------------------------------------

    /// Configure the backend to fail after N calls.
    pub fn fail_after(mut self, n: u32) -> Self {
        self.fail_after = Some(n);
        self
    }

    /// Set the tip height (reserved for future use).
    #[allow(dead_code)]
    pub fn set_tip_height(mut self, height: u32) -> Self {
        self.tip_height = height;
        self
    }

    /// Add a block to the backend (reserved for future use).
    #[allow(dead_code)]
    pub fn add_block(mut self, block: MockBlock) -> Self {
        if block.height > self.tip_height {
            self.tip_height = block.height;
        }
        self.blocks.push(block);
        self
    }

    //-------------------------------------------------------------------------
    // Getters
    //-------------------------------------------------------------------------

    /// Returns the current call count.
    pub fn call_count(&self) -> u32 {
        self.call_count.load(Ordering::Relaxed)
    }

    /// Reset the call count to zero.
    pub fn reset_call_count(&self) {
        self.call_count.store(0, Ordering::Relaxed);
    }

    //-------------------------------------------------------------------------
    // Mock API methods
    //-------------------------------------------------------------------------

    /// Get the current block height (simulates backend.block_height()).
    ///
    /// Increments call count and may return an error if `fail_after` is set.
    pub fn block_height(&self) -> Result<u32, MockBackendError> {
        let count = self.call_count.fetch_add(1, Ordering::Relaxed);

        if let Some(fail_after) = self.fail_after {
            if count >= fail_after {
                return Err(MockBackendError::SimulatedFailure(fail_after));
            }
        }

        Ok(self.tip_height)
    }

    /// Get block data at a specific height.
    ///
    /// Increments call count and may return an error if `fail_after` is set.
    pub fn get_block(&self, height: u32) -> Result<Option<&MockBlock>, MockBackendError> {
        let count = self.call_count.fetch_add(1, Ordering::Relaxed);

        if let Some(fail_after) = self.fail_after {
            if count >= fail_after {
                return Err(MockBackendError::SimulatedFailure(fail_after));
            }
        }

        Ok(self.blocks.iter().find(|b| b.height == height))
    }

    /// Get all blocks in a range.
    pub fn get_blocks_in_range(
        &self,
        start: u32,
        end: u32,
    ) -> Result<Vec<&MockBlock>, MockBackendError> {
        let count = self.call_count.fetch_add(1, Ordering::Relaxed);

        if let Some(fail_after) = self.fail_after {
            if count >= fail_after {
                return Err(MockBackendError::SimulatedFailure(fail_after));
            }
        }

        Ok(self
            .blocks
            .iter()
            .filter(|b| b.height >= start && b.height <= end)
            .collect())
    }
}

//=============================================================================
// Test Fixtures
//=============================================================================

/// Returns a fixed 12-word test mnemonic.
///
/// This is the standard BIP39 test vector mnemonic.
/// WARNING: Never use this mnemonic for real funds!
pub fn test_mnemonic() -> &'static str {
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
}

/// Returns a valid test Config pointing to a temporary directory.
///
/// The config has:
/// - account_name: "test-account"
/// - network: Signet
/// - mnemonic: standard test mnemonic
/// - blindbit_url: placeholder URL
/// - persist: false (to avoid file I/O in tests)
pub fn test_config(temp_dir: &std::path::Path) -> Config {
    Config::new(
        "test-account".to_string(),
        bitcoin::Network::Signet,
        test_mnemonic().to_string(),
        "https://blindbit.test.example.com".to_string(),
        temp_dir.to_path_buf(),
    )
    .enable_persist(false)
}

/// Returns a test OutPoint with a deterministic txid.
///
/// The txid is created from byte array [1u8; 32] with vout=0.
pub fn test_outpoint() -> OutPoint {
    OutPoint {
        txid: Txid::from_byte_array([1u8; 32]),
        vout: 0,
    }
}

/// Returns a second test OutPoint with a different txid.
///
/// The txid is created from byte array [2u8; 32] with vout=1.
pub fn test_outpoint_2() -> OutPoint {
    OutPoint {
        txid: Txid::from_byte_array([2u8; 32]),
        vout: 1,
    }
}

/// Returns a third test OutPoint with a different txid.
///
/// The txid is created from byte array [3u8; 32] with vout=2.
pub fn test_outpoint_3() -> OutPoint {
    OutPoint {
        txid: Txid::from_byte_array([3u8; 32]),
        vout: 2,
    }
}

/// Returns a test OwnedOutput with specified height and amount.
///
/// The output is unspent and has default values for tweak and script.
pub fn test_owned_output(height: u32, amount: u64) -> OwnedOutput {
    OwnedOutput {
        blockheight: Height::from_consensus(height).unwrap_or(Height::ZERO),
        tweak: [0u8; 32],
        amount: Amount::from_sat(amount),
        script: ScriptBuf::new(),
        label: None,
        spend_status: OutputSpendStatus::Unspent,
    }
}

/// Returns a test OwnedOutput that has been spent.
///
/// The output is marked as Spent with a zeroed spending txid.
pub fn test_spent_output(height: u32, amount: u64) -> OwnedOutput {
    OwnedOutput {
        blockheight: Height::from_consensus(height).unwrap_or(Height::ZERO),
        tweak: [0u8; 32],
        amount: Amount::from_sat(amount),
        script: ScriptBuf::new(),
        label: None,
        spend_status: OutputSpendStatus::Spent([0u8; 32]),
    }
}

/// Creates a unique temporary directory for tests.
///
/// The directory is created under the system temp directory with a unique name
/// based on the current timestamp and a random suffix.
///
/// Note: The caller is responsible for cleaning up the directory after the test.
pub fn temp_dir() -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    let dir = std::env::temp_dir().join(format!("bwk-sp-test-{}", timestamp));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Cleanup helper for temporary directories.
///
/// Removes the directory and all its contents. Ignores errors.
pub fn cleanup_temp_dir(path: &std::path::Path) {
    let _ = std::fs::remove_dir_all(path);
}

//=============================================================================
// Blindbitd Helpers (Phase 10.4)
//=============================================================================

/// Dust threshold for Silent Payment outputs.
#[allow(dead_code)]
pub const DUST: u64 = 330;

/// Wait until the backend has synced to at least the given height.
///
/// Polls the backend every 100ms until `block_height()` returns at least `height`.
#[allow(dead_code)]
pub fn wait_until_sync_at_height<B: ChainBackend>(backend: &B, height: u32) {
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(60);
    loop {
        if start.elapsed() > timeout {
            panic!(
                "wait_until_sync_at_height: timed out waiting for height {}",
                height
            );
        }
        if let Ok(h) = backend.block_height() {
            if h.to_consensus_u32() >= height {
                return;
            }
        }
        thread::sleep(Duration::from_secs(2));
    }
}

/// Wait for sync and add extra delay for indexing.
///
/// Waits until sync reaches `height`, then sleeps an additional 2 seconds
/// to allow BlindbitD time to index the new blocks.
#[allow(dead_code)]
pub fn wait_for_sync_and_index<B: ChainBackend>(backend: &B, height: u32) {
    wait_until_sync_at_height(backend, height);
    // Give blindbitd extra time to index new blocks
    thread::sleep(Duration::from_secs(2));
}

/// Extract the XOnlyPublicKey from a P2TR output script.
///
/// # Panics
///
/// Panics if the script is not a valid P2TR output (OP_1 <32-byte key>).
#[allow(dead_code)]
pub fn get_taproot_pubkey(txout: &TxOut) -> XOnlyPublicKey {
    let script_bytes = txout.script_pubkey.as_bytes();
    assert_eq!(script_bytes[0], 0x51); // OP_1
    assert_eq!(script_bytes[1], 0x20); // 32 bytes
    bitcoin::key::XOnlyPublicKey::from_slice(&script_bytes[2..34]).expect("valid output key")
}

/// Generate a recipient public key for a Silent Payment transaction.
///
/// This function:
/// 1. Tweaks the input secret key for taproot
/// 2. Verifies it matches the prevout script
/// 3. Calculates the partial secret from input keys and outpoints
/// 4. Generates the recipient pubkey using the SP address
///
/// # Arguments
///
/// * `sk` - Internal (untweaked) secret key for the input being spent
/// * `outpoint` - The outpoint being spent
/// * `txout` - The prevout (previous output being spent)
/// * `sp_addr` - The Silent Payment address to send to
/// * `secp` - Secp256k1 context
///
/// # Returns
///
/// The recipient's XOnlyPublicKey for the SP output, or None if generation fails.
#[allow(dead_code, non_snake_case)]
pub fn generate_recipient_pubkey(
    sk: bitcoin::secp256k1::SecretKey,
    outpoint: OutPoint,
    txout: &TxOut,
    sp_addr: spdk_core::silentpayments::SilentPaymentAddress,
    secp: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
) -> Option<XOnlyPublicKey> {
    use bitcoin::key::TapTweak;

    // tweak the key
    let keypair = bitcoin::secp256k1::Keypair::from_secret_key(secp, &sk);
    #[allow(deprecated)]
    let keypair = keypair.tap_tweak(secp, None).to_inner();
    let taproot_pubkey = get_taproot_pubkey(txout);

    // check the secret key we pass to calculate_partial_secret() is the one related to
    // the txout script_pubkey
    let (sp_pk, _parity) = keypair.x_only_public_key();
    assert_eq!(taproot_pubkey, sp_pk);

    // process partial secret
    let sp_sk = keypair.secret_key();

    let input_keys = vec![(sp_sk, true /* is taproot */)];
    let outpoints = vec![(outpoint.txid.to_string(), outpoint.vout)];
    let partial_secret = spdk_core::silentpayments::utils::sending::calculate_partial_secret(
        &input_keys,
        &outpoints,
    )
    .ok()?;

    // generate recipient pubkey
    spdk_core::silentpayments::sending::generate_recipient_pubkeys(vec![sp_addr], partial_secret)
        .ok()?
        .into_iter()
        .next()
        .and_then(|(_addr, k)| k.into_iter().next())
}

/// Build and sign a transaction that sends to a Silent Payment output.
///
/// Creates a simple 1-input, 1-output transaction spending from a P2TR input
/// to a P2TR output with the given recipient pubkey.
///
/// # Arguments
///
/// * `sk` - Internal (untweaked) secret key for the input
/// * `outpoint` - The outpoint being spent
/// * `txout` - The prevout (previous output being spent)
/// * `recipient_pubkey` - The SP recipient's XOnlyPublicKey
/// * `fees` - Transaction fee amount
/// * `secp` - Secp256k1 context
///
/// # Returns
///
/// A signed transaction, or None if the input value is insufficient.
#[allow(dead_code)]
pub fn swap_to_sp(
    sk: bitcoin::secp256k1::SecretKey,
    outpoint: OutPoint,
    txout: TxOut,
    recipient_pubkey: XOnlyPublicKey,
    fees: bitcoin::Amount,
    secp: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
) -> Option<bitcoin::Transaction> {
    use bitcoin::key::TapTweak;
    use bitcoin::{absolute, sighash, transaction::Version, Sequence, TxIn, Witness};

    // craft tx
    let script = ScriptBuf::new_p2tr_tweaked(recipient_pubkey.dangerous_assume_tweaked());
    if txout.value < (fees + Amount::from_sat(DUST)) {
        return None;
    }
    let value = txout.value - fees;
    let output = vec![TxOut {
        value,
        script_pubkey: script,
    }];
    let input = vec![TxIn {
        previous_output: outpoint,
        script_sig: Default::default(),
        sequence: Sequence::ZERO,
        witness: Default::default(),
    }];
    let mut tx = bitcoin::Transaction {
        version: Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input,
        output,
    };

    // tweak the key
    let keypair = bitcoin::secp256k1::Keypair::from_secret_key(secp, &sk);
    #[allow(deprecated)]
    let keypair = keypair.tap_tweak(secp, None).to_inner();

    // process sighash
    let mut cache = sighash::SighashCache::new(tx.clone());
    let sighash_type = sighash::TapSighashType::Default;
    let txouts = vec![txout.clone()];
    let prevouts = sighash::Prevouts::All(&txouts);
    let sighash = cache
        .taproot_key_spend_signature_hash(0, &prevouts, sighash_type)
        .ok()?;
    let sighash = bitcoin::secp256k1::Message::from_digest_slice(
        &bitcoin::hashes::Hash::to_byte_array(sighash),
    )
    .expect("Sighash is always 32 bytes.");

    // sign
    let signature = secp.sign_schnorr_no_aux_rand(&sighash, &keypair);
    let sig = bitcoin::taproot::Signature {
        signature,
        sighash_type,
    };

    // craft & add witness
    let witness = Witness::p2tr_key_spend(&sig);
    tx.input[0].witness = witness;

    Some(tx)
}

//=============================================================================
// Tests for test utilities
//=============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_backend_new() {
        let backend = MockBackend::new(100);
        assert_eq!(backend.block_height().unwrap(), 100);
        assert_eq!(backend.call_count(), 1);
    }

    #[test]
    fn test_mock_backend_with_blocks() {
        let blocks = vec![
            MockBlock::new(100),
            MockBlock::new(101),
            MockBlock::new(102),
        ];
        let backend = MockBackend::with_blocks(blocks);

        assert_eq!(backend.block_height().unwrap(), 102);
        assert!(backend.get_block(100).unwrap().is_some());
        assert!(backend.get_block(101).unwrap().is_some());
        assert!(backend.get_block(102).unwrap().is_some());
        assert!(backend.get_block(103).unwrap().is_none());
    }

    #[test]
    fn test_mock_backend_fail_after() {
        let backend = MockBackend::new(100).fail_after(2);

        // First two calls succeed
        assert!(backend.block_height().is_ok());
        assert!(backend.block_height().is_ok());

        // Third call fails
        let result = backend.block_height();
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            MockBackendError::SimulatedFailure(2)
        ));
    }

    #[test]
    fn test_mock_backend_call_count() {
        let backend = MockBackend::new(100);

        assert_eq!(backend.call_count(), 0);
        let _ = backend.block_height();
        assert_eq!(backend.call_count(), 1);
        let _ = backend.block_height();
        assert_eq!(backend.call_count(), 2);

        backend.reset_call_count();
        assert_eq!(backend.call_count(), 0);
    }

    #[test]
    fn test_mock_block_builder() {
        let outpoint = test_outpoint();
        let output = test_owned_output(100, 50000);

        let block = MockBlock::new(100)
            .with_output(outpoint, output)
            .with_spent_input(test_outpoint_2());

        assert_eq!(block.height, 100);
        assert_eq!(block.outputs.len(), 1);
        assert_eq!(block.spent_inputs.len(), 1);
    }

    #[test]
    fn test_mock_backend_get_blocks_in_range() {
        let blocks = vec![
            MockBlock::new(100),
            MockBlock::new(101),
            MockBlock::new(102),
            MockBlock::new(103),
        ];
        let backend = MockBackend::with_blocks(blocks);

        let range = backend.get_blocks_in_range(101, 102).unwrap();
        assert_eq!(range.len(), 2);
        assert!(range.iter().any(|b| b.height == 101));
        assert!(range.iter().any(|b| b.height == 102));
    }

    #[test]
    fn test_test_mnemonic() {
        let mnemonic = test_mnemonic();
        assert_eq!(mnemonic.split_whitespace().count(), 12);
        assert!(mnemonic.starts_with("abandon"));
    }

    #[test]
    fn test_test_config() {
        let dir = temp_dir();
        let config = test_config(&dir);

        assert_eq!(config.account_name, "test-account");
        assert_eq!(config.network, bitcoin::Network::Signet);
        assert!(!config.persist);
        assert!(config.mnemonic.is_some());

        cleanup_temp_dir(&dir);
    }

    #[test]
    fn test_test_outpoints() {
        let op1 = test_outpoint();
        let op2 = test_outpoint_2();
        let op3 = test_outpoint_3();

        // All should be different
        assert_ne!(op1.txid, op2.txid);
        assert_ne!(op2.txid, op3.txid);
        assert_ne!(op1.txid, op3.txid);

        // Vouts should be as expected
        assert_eq!(op1.vout, 0);
        assert_eq!(op2.vout, 1);
        assert_eq!(op3.vout, 2);
    }

    #[test]
    fn test_test_owned_output() {
        let output = test_owned_output(100, 50000);

        assert_eq!(output.blockheight.to_consensus_u32(), 100);
        assert_eq!(output.amount.to_sat(), 50000);
        assert!(matches!(output.spend_status, OutputSpendStatus::Unspent));
    }

    #[test]
    fn test_test_spent_output() {
        let output = test_spent_output(100, 50000);

        assert_eq!(output.blockheight.to_consensus_u32(), 100);
        assert_eq!(output.amount.to_sat(), 50000);
        assert!(matches!(output.spend_status, OutputSpendStatus::Spent(_)));
    }

    #[test]
    fn test_temp_dir_creation() {
        let dir = temp_dir();
        assert!(dir.exists());
        assert!(dir.is_dir());

        cleanup_temp_dir(&dir);
        assert!(!dir.exists());
    }
}
