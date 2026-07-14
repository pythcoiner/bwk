//! Source: adapted from cygnet3/spdk. See `sp/NOTICE`.

use std::time::Duration;

use bitcoin::{absolute::Height, Amount, BlockHash, Network, ScriptBuf, Txid};
use serde::{Deserialize, Deserializer};

pub mod error;

use crate::receiver::{FilterData, SpentIndexData, UtxoData};
use error::Error;

// HTTP agent and low-level requests.

const DNS_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const DNS_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(target_os = "android")]
fn android_root_certs() -> Result<ureq::tls::RootCerts, Error> {
    let mut certs = Vec::new();

    for data in bwk::bwk_utils::android_root_certs().map_err(|e| Error::TlsConfig(e.to_string()))? {
        let cert = match ureq::tls::Certificate::from_pem(&data) {
            Ok(cert) => cert,
            Err(_) => ureq::tls::Certificate::from_der(&data).to_owned(),
        };
        certs.push(cert);
    }

    Ok(ureq::tls::RootCerts::new_with_certs(&certs))
}

fn native_tls_config() -> Result<ureq::tls::TlsConfig, Error> {
    let builder = ureq::tls::TlsConfig::builder().provider(ureq::tls::TlsProvider::NativeTls);

    #[cfg(target_os = "android")]
    let builder = builder.root_certs(android_root_certs()?);

    Ok(builder.build())
}

pub fn agent() -> Result<ureq::Agent, Error> {
    agent_with_fetch_concurrency(crate::scan::fetch_concurrency())
}

pub fn agent_with_fetch_concurrency(fetch_concurrency: usize) -> Result<ureq::Agent, Error> {
    // Keep one idle connection per concurrent fetch worker so the pool is reused
    // rather than churned on every block.
    let max_idle = fetch_concurrency;
    let config = ureq::Agent::config_builder()
        .tls_config(native_tls_config()?)
        .timeout_global(Some(Duration::from_secs(30)))
        .max_idle_connections(max_idle)
        .max_idle_connections_per_host(max_idle)
        .build();
    Ok(ureq::Agent::with_parts(
        config,
        ureq::unversioned::transport::DefaultConnector::default(),
        bwk_utils::ureq_resolver::RefreshingResolver::new(
            DNS_REFRESH_INTERVAL,
            DNS_REFRESH_TIMEOUT,
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tls_config_uses_native_tls() {
        assert_eq!(
            native_tls_config().unwrap().provider(),
            ureq::tls::TlsProvider::NativeTls
        );
    }
}

fn get(agent: &ureq::Agent, url: &str, query_params: &[(&str, String)]) -> Result<String, Error> {
    let mut req = agent.get(url);

    for (key, value) in query_params {
        req = req.query(key, value);
    }

    let mut response = req.call().map_err(|e| Error::HttpGet(e.to_string()))?;

    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| Error::ResponseBody(e.to_string()))?;

    Ok(body)
}

fn join(url: &str, route: &str) -> String {
    let url = url.trim_end_matches('/');
    format!("{url}/{route}")
}

// Oracle endpoints.

pub fn block_height(agent: &ureq::Agent, url: &str) -> Result<Height, Error> {
    let url = join(url, "block-height");
    let body = get(agent, url.as_str(), &[])?;
    let blkheight: BlockHeightResponse = serde_json::from_str(&body)?;
    Ok(blkheight.block_height)
}

pub fn info(agent: &ureq::Agent, url: &str) -> Result<InfoResponse, Error> {
    let url = join(url, "info");
    let body = get(agent, url.as_str(), &[])?;
    Ok(serde_json::from_str(&body)?)
}

pub fn spent_filter(
    agent: &ureq::Agent,
    url: &str,
    block_height: Height,
    observer: Option<&crate::scan::HeightObserver>,
) -> Result<FilterData, Error> {
    if let Some(observer) = observer {
        observer(block_height);
    }
    let url = join(url, &format!("filter/spent/{}", block_height));
    let body = get(agent, url.as_str(), &[])?;
    let resp: FilterResponse = serde_json::from_str(&body)?;
    Ok(resp.into())
}

pub fn spent_index(
    agent: &ureq::Agent,
    url: &str,
    block_height: Height,
) -> Result<SpentIndexData, Error> {
    let url = join(url, &format!("spent-index/{}", block_height));
    let body = get(agent, url.as_str(), &[])?;
    let resp: SpentIndexResponse = serde_json::from_str(&body)?;
    Ok(resp.into())
}

pub fn utxos(agent: &ureq::Agent, url: &str, block_height: Height) -> Result<Vec<UtxoData>, Error> {
    let url = join(url, &format!("utxos/{}", block_height));
    let body = get(agent, url.as_str(), &[])?;
    let resp: Vec<UtxoResponse> = serde_json::from_str(&body)?;
    Ok(resp.into_iter().map(Into::into).collect())
}

pub(crate) fn tweaks(
    agent: &ureq::Agent,
    url: &str,
    block_height: Height,
    dust_limit: Option<Amount>,
) -> Result<Vec<[u8; 33]>, Error> {
    let url = join(url, &format!("tweaks/{}", block_height));
    let params = dust_limit
        .map(|dl| vec![("dustLimit", dl.to_sat().to_string())])
        .unwrap_or_default();
    let body = get(agent, url.as_str(), &params)?;
    parse_tweaks(&body)
}

pub(crate) fn tweak_index(
    agent: &ureq::Agent,
    url: &str,
    block_height: Height,
    dust_limit: Option<Amount>,
) -> Result<Vec<[u8; 33]>, Error> {
    let url = join(url, &format!("tweak-index/{}", block_height));
    let params = dust_limit
        .map(|dl| vec![("dustLimit", dl.to_sat().to_string())])
        .unwrap_or_default();
    let body = get(agent, url.as_str(), &params)?;
    parse_tweaks(&body)
}

pub(crate) fn filter_new_utxos(
    agent: &ureq::Agent,
    url: &str,
    block_height: Height,
) -> Result<FilterResponse, Error> {
    let url = join(url, &format!("filter/new-utxos/{}", block_height));
    let body = get(agent, url.as_str(), &[])?;
    Ok(serde_json::from_str(&body)?)
}

fn parse_tweaks(body: &str) -> Result<Vec<[u8; 33]>, Error> {
    #[derive(serde::Deserialize)]
    #[serde(transparent)]
    struct TweakHex(#[serde(with = "hex::serde")] Vec<u8>);

    let raw: Vec<TweakHex> = serde_json::from_str(body)?;
    raw.into_iter()
        .map(|t| {
            t.0.try_into()
                .map_err(|_| Error::ResponseBody("tweak is not 33 bytes".to_string()))
        })
        .collect()
}

// Oracle response types.

#[derive(Debug, Deserialize)]
struct BlockHeightResponse {
    block_height: Height,
}

#[derive(Debug, Deserialize)]
struct UtxoResponse {
    txid: Txid,
    vout: u32,
    value: Amount,
    scriptpubkey: ScriptBuf,
    spent: bool,
}

impl From<UtxoResponse> for UtxoData {
    fn from(value: UtxoResponse) -> Self {
        Self {
            txid: value.txid,
            vout: value.vout,
            value: value.value,
            scriptpubkey: value.scriptpubkey,
            spent: value.spent,
        }
    }
}

#[derive(Debug, Deserialize)]
struct SpentIndexResponse {
    block_hash: BlockHash,
    data: Vec<MyHex>,
}

impl From<SpentIndexResponse> for SpentIndexData {
    fn from(value: SpentIndexResponse) -> Self {
        Self {
            data: value.data.into_iter().map(|x| x.hex).collect(),
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(transparent)]
struct MyHex {
    #[serde(with = "hex::serde")]
    hex: Vec<u8>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FilterResponse {
    pub(crate) block_hash: BlockHash,
    data: MyHex,
}

impl From<FilterResponse> for FilterData {
    fn from(value: FilterResponse) -> Self {
        Self {
            block_hash: value.block_hash,
            data: value.data.hex,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct InfoResponse {
    #[serde(deserialize_with = "deserialize_network")]
    pub network: Network,
    pub height: Height,
    pub tweaks_only: bool,
    pub tweaks_full_basic: bool,
    pub tweaks_full_with_dust_filter: bool,
    pub tweaks_cut_through_with_dust_filter: bool,
}

fn deserialize_network<'de, D>(deserializer: D) -> Result<Network, D::Error>
where
    D: Deserializer<'de>,
{
    let buf = String::deserialize(deserializer)?;

    Network::from_core_arg(&buf).map_err(serde::de::Error::custom)
}
