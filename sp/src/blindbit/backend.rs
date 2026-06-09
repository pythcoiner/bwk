use std::{
    ops::RangeInclusive,
    sync::{mpsc, Arc},
};

use bitcoin::{absolute::Height, Amount};

use crate::blindbit::client;
use crate::spdk_core::{BlockData, SpentIndexData, UtxoData};

pub type BlockDataObserver = Arc<dyn Fn(&BlockData) + Send + Sync>;
pub type HeightObserver = Arc<dyn Fn(Height) + Send + Sync>;

type BlockDataIterator =
    Box<dyn Iterator<Item = crate::spdk_core::error::Result<BlockData>> + Send>;

const CONCURRENT_FILTER_REQUESTS: usize = 64;
const BLOCK_CHANNEL_CAPACITY: usize = 64;

fn fetch_concurrency() -> usize {
    std::env::var("BWK_SP_FETCH_CONCURRENCY")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(CONCURRENT_FILTER_REQUESTS)
}

fn fetch_channel_cap() -> usize {
    std::env::var("BWK_SP_FETCH_CHANNEL_CAP")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(BLOCK_CHANNEL_CAPACITY)
}

pub fn agent() -> ureq::Agent {
    client::agent()
}

pub fn block_height(agent: &ureq::Agent, url: &str) -> crate::blindbit::error::Result<Height> {
    client::block_height(agent, url)
}

pub fn info(
    agent: &ureq::Agent,
    url: &str,
) -> crate::blindbit::error::Result<crate::blindbit::InfoResponse> {
    client::info(agent, url)
}

pub fn spent_filter(
    agent: &ureq::Agent,
    url: &str,
    block_height: Height,
    observer: Option<&HeightObserver>,
) -> crate::blindbit::error::Result<crate::spdk_core::FilterData> {
    if let Some(observer) = observer {
        observer(block_height);
    }
    Ok(client::filter_spent(agent, url, block_height)?.into())
}

pub fn spent_index(
    agent: &ureq::Agent,
    url: &str,
    block_height: Height,
) -> crate::blindbit::error::Result<SpentIndexData> {
    Ok(client::spent_index(agent, url, block_height)?.into())
}

pub fn utxos(
    agent: &ureq::Agent,
    url: &str,
    block_height: Height,
) -> crate::blindbit::error::Result<Vec<UtxoData>> {
    Ok(client::utxos(agent, url, block_height)?
        .into_iter()
        .map(Into::into)
        .collect())
}

pub fn forward_tx(
    agent: &ureq::Agent,
    url: &str,
    tx_hex: String,
) -> crate::blindbit::error::Result<bitcoin::Txid> {
    client::forward_tx(agent, url, tx_hex)
}

pub fn get_block_data_for_range(
    agent: Arc<ureq::Agent>,
    url: String,
    mut range: RangeInclusive<u32>,
    dust_limit: Option<Amount>,
    with_cutthrough: bool,
    block_data_observer: Option<BlockDataObserver>,
) -> BlockDataIterator {
    use crate::blindbit::thread_pool::ThreadPool;

    if *range.start() == 0 {
        range = RangeInclusive::new(1, *range.end());
    }

    let (sender, receiver) = mpsc::sync_channel(fetch_channel_cap());
    spawn_block_fetchers(
        agent,
        url,
        range,
        dust_limit,
        with_cutthrough,
        sender,
        block_data_observer,
        ThreadPool::new(fetch_concurrency()),
    );
    Box::new(receiver.into_iter())
}

#[allow(clippy::too_many_arguments)]
fn spawn_block_fetchers(
    agent: Arc<ureq::Agent>,
    url: String,
    range: RangeInclusive<u32>,
    dust_limit: Option<Amount>,
    with_cutthrough: bool,
    sender: mpsc::SyncSender<crate::spdk_core::error::Result<BlockData>>,
    block_data_observer: Option<BlockDataObserver>,
    pool: crate::blindbit::thread_pool::ThreadPool,
) {
    for height in range {
        let agent = agent.clone();
        let url = url.clone();
        let sender = sender.clone();
        let block_data_observer = block_data_observer.clone();
        pool.execute(move || {
            fetch_block_data_for_height(
                agent,
                url,
                height,
                dust_limit,
                with_cutthrough,
                sender,
                block_data_observer,
            );
        });
    }
}

fn fetch_block_data_for_height(
    agent: Arc<ureq::Agent>,
    url: String,
    height: u32,
    dust_limit: Option<Amount>,
    with_cutthrough: bool,
    sender: mpsc::SyncSender<crate::spdk_core::error::Result<BlockData>>,
    block_data_observer: Option<BlockDataObserver>,
) {
    let blkheight = match Height::from_consensus(height) {
        Ok(bh) => bh,
        Err(e) => {
            let _ = sender.send(Err(crate::spdk_core::Error::from(e)));
            return;
        }
    };
    let tweaks = match with_cutthrough {
        true => client::tweaks(&agent, &url, blkheight, dust_limit),
        false => client::tweak_index(&agent, &url, blkheight, dust_limit),
    };
    let tweaks = match tweaks {
        Ok(t) => t,
        Err(e) => {
            let _ = sender.send(Err(crate::spdk_core::Error::from(e)));
            return;
        }
    };
    let new_utxo_filter = match client::filter_new_utxos(&agent, &url, blkheight) {
        Ok(f) => f,
        Err(e) => {
            let _ = sender.send(Err(crate::spdk_core::Error::from(e)));
            return;
        }
    };
    let blkhash = new_utxo_filter.block_hash;
    let block_data = BlockData {
        blkheight,
        blkhash,
        tweaks,
        new_utxo_filter: new_utxo_filter.into(),
    };
    if let Some(observer) = &block_data_observer {
        observer(&block_data);
    }
    let _ = sender.send(Ok(block_data));
}
