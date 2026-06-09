use std::{
    ops::RangeInclusive,
    sync::{mpsc, Arc},
};

use bitcoin::{absolute::Height, Amount};

#[cfg(feature = "parallel")]
use rayon::{
    iter::{IntoParallelIterator, ParallelIterator},
    ThreadPoolBuilder,
};

use crate::blindbit::client::{BlindbitClient, HttpClient};
use crate::spdk_core::{BlockData, ChainBackend, SpentIndexData, UtxoData};

/// Number of blocks fetched concurrently. Enough to stay ahead of the
/// single-threaded matcher even on a high-latency link (~3 requests/block), while
/// keeping the thread/connection count modest for mobile.
const CONCURRENT_FILTER_REQUESTS: usize = 64;

/// Bound on the channel of fetched-but-unprocessed blocks. Fetching outruns the
/// matcher, so without a bound the whole range is buffered in RAM (~6 GB for
/// mainnet); a bounded channel applies backpressure (fetch workers block on send
/// when full), capping memory to ~this many blocks regardless of range. 64 blocks
/// is ~0.5s of matcher lookahead, so the matcher never starves.
const BLOCK_CHANNEL_CAPACITY: usize = 64;

/// Fetch concurrency, overridable via `BWK_SP_FETCH_CONCURRENCY` for tuning a
/// given link/oracle (more helps only when fetch is latency-bound, not when the
/// network bandwidth or the oracle is the ceiling). Defaults to the const above.
fn fetch_concurrency() -> usize {
    std::env::var("BWK_SP_FETCH_CONCURRENCY")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(CONCURRENT_FILTER_REQUESTS)
}

/// Fetch lookahead buffer (blocks), overridable via `BWK_SP_FETCH_CHANNEL_CAP`.
/// Larger smooths bursty fetch but raises peak RAM; keep modest on mobile.
fn fetch_channel_cap() -> usize {
    std::env::var("BWK_SP_FETCH_CHANNEL_CAP")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(BLOCK_CHANNEL_CAPACITY)
}

pub struct BlindbitBackend<H: HttpClient> {
    client: BlindbitClient<H>,
}

impl<H: HttpClient + Clone + 'static> BlindbitBackend<H> {
    /// Create a new async Blindbit backend with a custom HTTP client.
    ///
    /// # Arguments
    /// * `blindbit_url` - Base URL of the Blindbit server
    /// * `http_client` - HTTP client implementation
    pub fn new(blindbit_url: String, http_client: H) -> crate::blindbit::error::Result<Self> {
        Ok(Self {
            client: BlindbitClient::new(blindbit_url, http_client)?,
        })
    }

    /// Get block data for a range of blocks as an Iterator
    ///
    /// This fetches blocks concurrently for better performance.
    ///
    /// # Arguments
    /// * `range` - Range of block heights to fetch
    /// * `dust_limit` - Minimum amount to consider (dust outputs are ignored)
    /// * `with_cutthrough` - Whether to use cutthrough optimization
    ///
    /// # Returns
    /// A Iterator of BlockData results
    pub fn get_block_data_for_range(
        &self,
        mut range: RangeInclusive<u32>,
        dust_limit: Option<Amount>,
        with_cutthrough: bool,
    ) -> crate::spdk_core::BlockDataIterator {
        // blindbit will return an error 500 for genesis block
        if *range.start() == 0 {
            range = RangeInclusive::new(1, *range.end());
        }

        #[cfg(feature = "parallel")]
        let iter = self.get_block_data_for_range_rayon(range, dust_limit, with_cutthrough);

        #[cfg(not(feature = "parallel"))]
        let iter = self.get_block_data_for_range_thread_pool(range, dust_limit, with_cutthrough);

        iter
    }

    #[cfg(not(feature = "parallel"))]
    pub fn get_block_data_for_range_thread_pool(
        &self,
        range: RangeInclusive<u32>,
        dust_limit: Option<Amount>,
        with_cutthrough: bool,
    ) -> crate::spdk_core::BlockDataIterator {
        use crate::blindbit::thread_pool::ThreadPool;

        let client = Arc::new(self.client.clone());

        // Bounded channel: applies backpressure so fetch workers block on send when
        // the matcher is behind, capping buffered blocks (and thus RAM) regardless
        // of range. See BLOCK_CHANNEL_CAPACITY.
        let (sender, receiver) = mpsc::sync_channel(fetch_channel_cap());

        let pool = ThreadPool::new(fetch_concurrency());

        for height in range {
            let client = client.clone();
            let sender = sender.clone();

            pool.execute(move || {
                get_block_data_for_height(height, dust_limit, with_cutthrough, sender, client);
            });
        }
        Box::new(receiver.into_iter())
    }
    #[cfg(feature = "parallel")]
    pub fn get_block_data_for_range_rayon(
        &self,
        range: RangeInclusive<u32>,
        dust_limit: Option<Amount>,
        with_cutthrough: bool,
    ) -> crate::spdk_core::BlockDataIterator {
        let client = Arc::new(self.client.clone());

        // Bounded channel for backpressure; see BLOCK_CHANNEL_CAPACITY.
        let (sender, receiver) = mpsc::sync_channel(fetch_channel_cap());

        let pool = ThreadPoolBuilder::new()
            .num_threads(fetch_concurrency())
            .build()
            .unwrap();

        // `pool.install` blocks the calling thread until the parallel fetch
        // finishes, but the workers block on the bounded send once the channel
        // fills and the only drainer is `receiver` below, so the fetch must run
        // on its own thread or producer and consumer deadlock.
        std::thread::spawn(move || {
            pool.install(|| {
                range.into_par_iter().for_each(move |height| {
                    let client = client.clone();
                    let sender = sender.clone();

                    get_block_data_for_height(height, dust_limit, with_cutthrough, sender, client);
                })
            });
        });
        Box::new(receiver.into_iter())
    }

    /// Fetch the spent filter for a single height (two-phase spend pass).
    pub fn spent_filter(
        &self,
        block_height: Height,
    ) -> crate::blindbit::error::Result<crate::spdk_core::FilterData> {
        Ok(self.client.filter_spent(block_height)?.into())
    }

    /// Get spent index data for a block height
    pub fn spent_index(
        &self,
        block_height: Height,
    ) -> crate::blindbit::error::Result<SpentIndexData> {
        Ok(self.client.spent_index(block_height)?.into())
    }

    /// Get UTXO data for a block height
    pub fn utxos(&self, block_height: Height) -> crate::blindbit::error::Result<Vec<UtxoData>> {
        Ok(self
            .client
            .utxos(block_height)?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// Get the current block height from the server
    pub fn block_height(&self) -> crate::blindbit::error::Result<Height> {
        self.client.block_height()
    }

    /// Get server info (network, supported modes, etc.)
    pub fn info(&self) -> crate::blindbit::error::Result<crate::blindbit::InfoResponse> {
        Ok(self.client.info()?)
    }
}

fn get_block_data_for_height<H>(
    height: u32,
    dust_limit: Option<Amount>,
    with_cutthrough: bool,
    sender: mpsc::SyncSender<crate::spdk_core::error::Result<BlockData>>,
    client: Arc<BlindbitClient<H>>,
) where
    H: HttpClient,
{
    let blkheight = match Height::from_consensus(height) {
        Ok(bh) => bh,
        Err(e) => {
            let _ = sender.send(Err(crate::spdk_core::Error::from(e)));
            return;
        }
    };
    let tweaks = match with_cutthrough {
        true => client.tweaks(blkheight, dust_limit),
        false => client.tweak_index(blkheight, dust_limit),
    };
    let tweaks = match tweaks {
        Ok(t) => t,
        Err(e) => {
            let _ = sender.send(Err(crate::spdk_core::Error::from(e)));
            return;
        }
    };
    // Receive-only fetch (two-phase receive pass): the spent filter is fetched
    // separately by the spend sweep, never here.
    let new_utxo_filter = match client.filter_new_utxos(blkheight) {
        Ok(f) => f,
        Err(e) => {
            let _ = sender.send(Err(crate::spdk_core::Error::from(e)));
            return;
        }
    };
    let blkhash = new_utxo_filter.block_hash;
    let _ = sender.send(Ok(BlockData {
        blkheight,
        blkhash,
        tweaks,
        new_utxo_filter: new_utxo_filter.into(),
    }));
}

impl<H: HttpClient + Clone + 'static> ChainBackend for BlindbitBackend<H> {
    fn get_block_data_for_range(
        &self,
        range: RangeInclusive<u32>,
        dust_limit: Option<Amount>,
        with_cutthrough: bool,
    ) -> crate::spdk_core::BlockDataIterator {
        self.get_block_data_for_range(range, dust_limit, with_cutthrough)
    }

    fn spent_filter(
        &self,
        block_height: Height,
    ) -> crate::spdk_core::error::Result<crate::spdk_core::FilterData> {
        Ok(self.spent_filter(block_height)?)
    }

    fn spent_index(&self, block_height: Height) -> crate::spdk_core::error::Result<SpentIndexData> {
        Ok(self.spent_index(block_height)?)
    }

    fn utxos(&self, block_height: Height) -> crate::spdk_core::error::Result<Vec<UtxoData>> {
        Ok(self.utxos(block_height)?)
    }

    fn block_height(&self) -> crate::spdk_core::error::Result<Height> {
        Ok(self.block_height()?)
    }
}
