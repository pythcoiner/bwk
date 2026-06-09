use crate::{
    scanner::MaybeSync, ChainBackend, OutputSpendStatus, OwnedOutput, SpClient, SpScanner, Updater,
};
use bitcoin::{
    absolute::Height,
    bip158::BlockFilter,
    hashes::{sha256, Hash},
    secp256k1::{self, All, PublicKey, SecretKey},
    OutPoint,
};
use silentpayments::SilentPaymentAddress;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Instant,
};

pub struct SpAccount<B, U>
where
    B: ChainBackend,
    U: Updater,
{
    backend: B,
    client: SpClient,
    updater: U,
    stop: Arc<AtomicBool>,
    owned_outpoints: HashSet<OutPoint>,
}

impl<B: ChainBackend, U: Updater> SpAccount<B, U> {
    pub fn new(backend: B, client: SpClient, updater: U, stop: Arc<AtomicBool>) -> Self {
        Self {
            backend,
            client,
            updater,
            stop,
            owned_outpoints: Default::default(),
        }
    }

    pub fn restore(
        backend: B,
        client: SpClient,
        updater: U,
        stop: Arc<AtomicBool>,
    ) -> crate::error::Result<Self> {
        let owned_outpoints = updater.restore_owned_outpoints()?;
        Ok(Self {
            backend,
            client,
            updater,
            stop,
            owned_outpoints,
        })
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn get_sp_address(&self) -> SilentPaymentAddress {
        self.client.get_receiving_address()
    }

    pub fn outpoints(&self) -> Vec<OutPoint> {
        self.owned_outpoints.clone().into_iter().collect()
    }

    pub fn block_height(&self) -> crate::error::Result<Height> {
        self.backend.block_height()
    }

    pub fn scan_key(&self) -> SecretKey {
        self.client.get_scan_key()
    }

    pub fn spend_key(&self, secp: &secp256k1::Secp256k1<All>) -> PublicKey {
        match self.client.get_spend_key() {
            crate::SpendKey::Secret(secret_key) => secret_key.public_key(secp),
            crate::SpendKey::Public(pubkey) => pubkey,
        }
    }
}

// `MaybeSync` is `Sync` on native+parallel (so the windowed scan can share
// `&SpAccount` across rayon threads for read-only matching) and a no-op bound on
// wasm/no-parallel, where the scan stays sequential.
impl<B: ChainBackend + MaybeSync, U: Updater + MaybeSync> SpScanner for SpAccount<B, U> {
    fn scan_blocks(
        &mut self,
        start: bitcoin::absolute::Height,
        end: bitcoin::absolute::Height,
        dust_limit: Option<bitcoin::Amount>,
        with_cutthrough: bool,
    ) -> crate::error::Result<()> {
        if start > end {
            return Err(crate::error::Error::InvalidRange(
                start.to_consensus_u32(),
                end.to_consensus_u32(),
            ));
        }

        log::info!("start: {} end: {}", start, end);
        let start_time: Instant = Instant::now();

        // Seed the in-memory owned set from the restored outpoints; the spend
        // sweep is bounded by the spend frontier, not by per-coin birth height.
        let owned = self.owned_outpoints.clone();
        let owned = crate::scanner::process_two_phase(
            self,
            start,
            end,
            dust_limit,
            with_cutthrough,
            owned,
        )?;
        // Refresh the owned-outpoint view from the order-free result.
        self.owned_outpoints = owned;

        log::info!(
            "Blindbit scan completed in {} seconds",
            start_time.elapsed().as_secs()
        );

        Ok(())
    }

    fn match_block_outputs(
        &self,
        blockdata: &crate::BlockData,
    ) -> crate::error::Result<std::collections::HashMap<bitcoin::OutPoint, crate::OwnedOutput>>
    {
        // `new_utxo_filter` is cloned because the read-only match takes the block
        // by reference (the window holds every block until its ordered commit).
        self.process_block_outputs(
            blockdata.blkheight,
            &blockdata.tweaks,
            blockdata.new_utxo_filter.clone(),
        )
    }

    fn process_block_outputs(
        &self,
        blkheight: bitcoin::absolute::Height,
        tweaks: &[bitcoin::secp256k1::PublicKey],
        new_utxo_filter: crate::FilterData,
    ) -> crate::error::Result<std::collections::HashMap<bitcoin::OutPoint, crate::OwnedOutput>>
    {
        let mut res = HashMap::new();

        if !tweaks.is_empty() {
            // Derive the candidate spks in one native call per tweak; the shared
            // secrets needed to recover spend tweaks are recomputed only on a
            // filter match below.
            let candidate_spks = self.client.get_candidate_spks(tweaks)?;
            let candidate_spks: Vec<&[u8; 34]> = candidate_spks.iter().collect();

            //get block gcs & check match
            let __t = std::time::Instant::now();
            let blkfilter = BlockFilter::new(&new_utxo_filter.data);
            let blkhash = new_utxo_filter.block_hash;

            let matched_outputs = Self::check_block_outputs(blkfilter, blkhash, candidate_spks)?;
            crate::scan_profile::add(&crate::scan_profile::OUTPUT_FILTER_NS, __t.elapsed());

            //if match: fetch and scan utxos
            if matched_outputs {
                log::info!("matched outputs on: {}", blkheight);
                let secrets_map = self.client.get_script_to_secret_map(tweaks.to_vec())?;
                let __t = std::time::Instant::now();
                let found = self.scan_utxos(blkheight, secrets_map)?;
                crate::scan_profile::add(&crate::scan_profile::SCAN_UTXOS_NS, __t.elapsed());

                if !found.is_empty() {
                    for (label, utxo, tweak) in found {
                        let outpoint = OutPoint {
                            txid: utxo.txid,
                            vout: utxo.vout,
                        };

                        let out = OwnedOutput {
                            blockheight: blkheight,
                            tweak: tweak.to_be_bytes(),
                            amount: utxo.value,
                            script: utxo.scriptpubkey,
                            label,
                            spend_status: OutputSpendStatus::Unspent,
                        };

                        res.insert(outpoint, out);
                    }
                }
            }
        }
        Ok(res)
    }

    fn get_block_data_iterator(
        &self,
        range: std::ops::RangeInclusive<u32>,
        dust_limit: Option<bitcoin::Amount>,
        with_cutthrough: bool,
    ) -> crate::BlockDataIterator {
        self.backend
            .get_block_data_for_range(range, dust_limit, with_cutthrough)
    }

    fn should_interrupt(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    fn save_state(&mut self) -> crate::error::Result<()> {
        self.updater.save_to_persistent_storage()
    }

    fn record_outputs(
        &mut self,
        height: bitcoin::absolute::Height,
        block_hash: bitcoin::BlockHash,
        outputs: std::collections::HashMap<bitcoin::OutPoint, crate::OwnedOutput>,
    ) -> crate::error::Result<()> {
        self.updater
            .record_block_outputs(height, block_hash, outputs)
    }

    fn record_inputs(
        &mut self,
        height: bitcoin::absolute::Height,
        block_hash: bitcoin::BlockHash,
        inputs: std::collections::HashSet<bitcoin::OutPoint>,
    ) -> crate::error::Result<()> {
        self.updater.record_block_inputs(height, block_hash, inputs)
    }

    fn record_progress(
        &mut self,
        start: bitcoin::absolute::Height,
        current: bitcoin::absolute::Height,
        end: bitcoin::absolute::Height,
    ) -> crate::error::Result<()> {
        self.updater.record_scan_progress(start, current, end)
    }

    fn client(&self) -> &SpClient {
        &self.client
    }

    fn backend(&self) -> &dyn ChainBackend {
        &self.backend
    }

    fn updater(&mut self) -> &mut dyn Updater {
        &mut self.updater
    }

    fn input_hashes_for(
        &self,
        blkhash: bitcoin::BlockHash,
        owned: &std::collections::HashSet<bitcoin::OutPoint>,
    ) -> crate::error::Result<std::collections::HashMap<[u8; 8], bitcoin::OutPoint>> {
        let mut map: HashMap<[u8; 8], OutPoint> = HashMap::new();

        for outpoint in owned {
            let mut arr = [0u8; 68];
            arr[..32].copy_from_slice(outpoint.txid.to_raw_hash().as_byte_array());
            arr[32..36].copy_from_slice(&outpoint.vout.to_le_bytes());
            arr[36..].copy_from_slice(&blkhash.to_byte_array());
            let hash = sha256::Hash::hash(&arr);

            let mut res = [0u8; 8];
            res.copy_from_slice(&hash[..8]);

            map.insert(res, *outpoint);
        }

        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{updater::DummyUpdater, SpentIndexData, Updater, UtxoData};
    use bitcoin::{absolute::Height, Amount, Network, Txid};
    use std::ops::RangeInclusive;

    /// Mock backend for testing stop flag behavior
    struct MockBackend;

    impl ChainBackend for MockBackend {
        fn get_block_data_for_range(
            &self,
            _range: RangeInclusive<u32>,
            _dust_limit: Option<Amount>,
            _with_cutthrough: bool,
        ) -> crate::BlockDataIterator {
            Box::new(std::iter::empty())
        }

        fn spent_filter(&self, _block_height: Height) -> crate::error::Result<crate::FilterData> {
            Ok(crate::FilterData {
                block_hash: bitcoin::BlockHash::from_byte_array([0u8; 32]),
                data: vec![],
            })
        }

        fn spent_index(&self, _block_height: Height) -> crate::error::Result<SpentIndexData> {
            Ok(SpentIndexData { data: vec![] })
        }

        fn utxos(&self, _block_height: Height) -> crate::error::Result<Vec<UtxoData>> {
            Ok(vec![])
        }

        fn block_height(&self) -> crate::error::Result<Height> {
            Ok(Height::from_consensus(100).expect("valid height"))
        }
    }

    fn create_test_sp_client() -> SpClient {
        let mnemonic = bip39::Mnemonic::parse(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
        ).unwrap();
        SpClient::new_from_mnemonic(mnemonic, Network::Regtest).unwrap()
    }

    #[test]
    fn test_stop_flag_initial_state() {
        let stop = Arc::new(AtomicBool::new(false));
        let account = SpAccount::new(
            MockBackend,
            create_test_sp_client(),
            DummyUpdater::new(),
            stop.clone(),
        );

        // Initially should not be interrupted
        assert!(!account.should_interrupt());
    }

    #[test]
    fn test_stop_flag_after_external_set() {
        let stop = Arc::new(AtomicBool::new(false));
        let account = SpAccount::new(
            MockBackend,
            create_test_sp_client(),
            DummyUpdater::new(),
            stop.clone(),
        );

        // Set stop flag externally (simulating what bwk-sp does)
        stop.store(true, Ordering::Relaxed);

        // should_interrupt() should now return true
        assert!(account.should_interrupt());
    }

    #[test]
    fn test_stop_method_sets_flag() {
        let stop = Arc::new(AtomicBool::new(false));
        let account = SpAccount::new(
            MockBackend,
            create_test_sp_client(),
            DummyUpdater::new(),
            stop.clone(),
        );

        assert!(!account.should_interrupt());

        // Call the stop method
        account.stop();

        // Both the shared flag and should_interrupt should reflect the change
        assert!(stop.load(Ordering::Relaxed));
        assert!(account.should_interrupt());
    }

    #[test]
    fn test_shared_stop_flag_across_accounts() {
        // This test verifies the key use case: multiple SpAccounts sharing
        // the same stop flag (as in bwk-sp's continuous scan loop)
        let shared_stop = Arc::new(AtomicBool::new(false));

        let account1 = SpAccount::new(
            MockBackend,
            create_test_sp_client(),
            DummyUpdater::new(),
            shared_stop.clone(),
        );

        // Neither should be interrupted initially
        assert!(!account1.should_interrupt());

        // Setting the shared flag affects account1
        shared_stop.store(true, Ordering::Relaxed);
        assert!(account1.should_interrupt());

        // Create a new account with the same flag (simulates loop iteration)
        let account2 = SpAccount::new(
            MockBackend,
            create_test_sp_client(),
            DummyUpdater::new(),
            shared_stop.clone(),
        );

        // account2 should also see the stop signal immediately
        assert!(account2.should_interrupt());
    }

    /// Mock updater that returns a predefined set of outpoints on restore
    struct MockUpdater {
        outpoints: HashSet<OutPoint>,
    }

    impl MockUpdater {
        fn new(outpoints: HashSet<OutPoint>) -> Self {
            Self { outpoints }
        }
    }

    impl Updater for MockUpdater {
        fn record_scan_progress(
            &mut self,
            _start: Height,
            _current: Height,
            _end: Height,
        ) -> crate::error::Result<()> {
            Ok(())
        }

        fn record_block_outputs(
            &mut self,
            _height: Height,
            _blkhash: bitcoin::BlockHash,
            _found_outputs: HashMap<OutPoint, crate::OwnedOutput>,
        ) -> crate::error::Result<()> {
            Ok(())
        }

        fn record_block_inputs(
            &mut self,
            _blkheight: Height,
            _blkhash: bitcoin::BlockHash,
            _found_inputs: HashSet<OutPoint>,
        ) -> crate::error::Result<()> {
            Ok(())
        }

        fn save_to_persistent_storage(&mut self) -> crate::error::Result<()> {
            Ok(())
        }

        fn restore_owned_outpoints(&self) -> crate::error::Result<HashSet<OutPoint>> {
            Ok(self.outpoints.clone())
        }
    }

    #[test]
    fn test_restore_populates_owned_outpoints() {
        let stop = Arc::new(AtomicBool::new(false));

        let op1 = OutPoint {
            txid: Txid::from_byte_array([1u8; 32]),
            vout: 0,
        };
        let op2 = OutPoint {
            txid: Txid::from_byte_array([2u8; 32]),
            vout: 1,
        };
        let expected: HashSet<OutPoint> = [op1, op2].into_iter().collect();

        let updater = MockUpdater::new(expected.clone());
        let account =
            SpAccount::restore(MockBackend, create_test_sp_client(), updater, stop).unwrap();

        let restored: HashSet<OutPoint> = account.outpoints().into_iter().collect();
        assert_eq!(restored, expected);
    }

    #[test]
    fn test_new_starts_with_empty_outpoints() {
        let stop = Arc::new(AtomicBool::new(false));
        let account = SpAccount::new(
            MockBackend,
            create_test_sp_client(),
            DummyUpdater::new(),
            stop,
        );

        assert!(account.outpoints().is_empty());
    }

    // Regression: when the persisted spend frontier lags the receive frontier (a
    // crash resumed receive past heights whose spend was never swept), the spend
    // sweep must still cover heights below this call's `start`. Frontier is 30 and
    // the scan is [61, 120], so the sweep must cover [31, 120], not just [61, 120].
    #[test]
    fn test_spend_sweep_covers_lagging_frontier_below_start() {
        use std::sync::Mutex;

        struct SweepBackend {
            spent: Arc<Mutex<Vec<u32>>>,
        }
        impl ChainBackend for SweepBackend {
            fn get_block_data_for_range(
                &self,
                range: RangeInclusive<u32>,
                _dust_limit: Option<Amount>,
                _with_cutthrough: bool,
            ) -> crate::BlockDataIterator {
                let blocks: Vec<crate::error::Result<crate::BlockData>> = range
                    .map(|h| {
                        Ok(crate::BlockData {
                            blkheight: Height::from_consensus(h).expect("valid height"),
                            blkhash: bitcoin::BlockHash::from_byte_array([0u8; 32]),
                            tweaks: vec![],
                            new_utxo_filter: crate::FilterData {
                                block_hash: bitcoin::BlockHash::from_byte_array([0u8; 32]),
                                data: vec![0u8],
                            },
                        })
                    })
                    .collect();
                Box::new(blocks.into_iter())
            }
            fn spent_filter(
                &self,
                block_height: Height,
            ) -> crate::error::Result<crate::FilterData> {
                self.spent
                    .lock()
                    .expect("poisoned")
                    .push(block_height.to_consensus_u32());
                // Valid GCS filter over zero elements (one CompactSize-0 byte), so
                // match_any reads cleanly and matches nothing.
                Ok(crate::FilterData {
                    block_hash: bitcoin::BlockHash::from_byte_array([0u8; 32]),
                    data: vec![0u8],
                })
            }
            fn spent_index(&self, _h: Height) -> crate::error::Result<SpentIndexData> {
                Ok(SpentIndexData { data: vec![] })
            }
            fn utxos(&self, _h: Height) -> crate::error::Result<Vec<UtxoData>> {
                Ok(vec![])
            }
            fn block_height(&self) -> crate::error::Result<Height> {
                Ok(Height::from_consensus(120).expect("valid height"))
            }
        }

        struct LagUpdater {
            seed: HashSet<OutPoint>,
            frontier: Option<u32>,
        }
        impl Updater for LagUpdater {
            fn record_scan_progress(
                &mut self,
                _: Height,
                _: Height,
                _: Height,
            ) -> crate::error::Result<()> {
                Ok(())
            }
            fn record_block_outputs(
                &mut self,
                _: Height,
                _: bitcoin::BlockHash,
                _: HashMap<OutPoint, crate::OwnedOutput>,
            ) -> crate::error::Result<()> {
                Ok(())
            }
            fn record_block_inputs(
                &mut self,
                _: Height,
                _: bitcoin::BlockHash,
                _: HashSet<OutPoint>,
            ) -> crate::error::Result<()> {
                Ok(())
            }
            fn save_to_persistent_storage(&mut self) -> crate::error::Result<()> {
                Ok(())
            }
            fn restore_owned_outpoints(&self) -> crate::error::Result<HashSet<OutPoint>> {
                Ok(self.seed.clone())
            }
            fn spend_frontier(&self) -> crate::error::Result<Option<u32>> {
                Ok(self.frontier)
            }
        }

        let stop = Arc::new(AtomicBool::new(false));
        let spent = Arc::new(Mutex::new(Vec::new()));
        let seed: HashSet<OutPoint> = [OutPoint {
            txid: Txid::from_byte_array([7u8; 32]),
            vout: 0,
        }]
        .into_iter()
        .collect();

        let mut account = SpAccount::restore(
            SweepBackend {
                spent: spent.clone(),
            },
            create_test_sp_client(),
            LagUpdater {
                seed,
                frontier: Some(30),
            },
            stop,
        )
        .unwrap();

        account
            .scan_blocks(
                Height::from_consensus(61).unwrap(),
                Height::from_consensus(120).unwrap(),
                None,
                false,
            )
            .unwrap();

        let swept = spent.lock().expect("poisoned").clone();
        let expected: Vec<u32> = (31..=120).collect();
        assert_eq!(
            swept, expected,
            "spend sweep must cover the lagging-frontier gap below start"
        );
    }
}
