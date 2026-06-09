use bitcoin::{absolute::Height, Amount, Txid};

use crate::blindbit::client::structs::InfoResponse;
use crate::blindbit::error::{Error, Result};

use super::structs::{
    BlockHeightResponse, FilterResponse, ForwardTxRequest, SpentIndexResponse, UtxoResponse,
};

pub fn block_height(agent: &ureq::Agent, host_url: &str) -> Result<Height> {
    let url = join(host_url, "block-height");
    let body = super::ureq_impl::get(agent, url.as_str(), &[])?;
    let blkheight: BlockHeightResponse = serde_json::from_str(&body)?;
    Ok(blkheight.block_height)
}

pub fn tweaks(
    agent: &ureq::Agent,
    host_url: &str,
    block_height: Height,
    dust_limit: Option<Amount>,
) -> Result<Vec<[u8; 33]>> {
    let url = join(host_url, &format!("tweaks/{}", block_height));
    let params = dust_limit
        .map(|dl| vec![("dustLimit", dl.to_sat().to_string())])
        .unwrap_or_default();
    let body = super::ureq_impl::get(agent, url.as_str(), &params)?;
    parse_tweaks(&body)
}

pub fn tweak_index(
    agent: &ureq::Agent,
    host_url: &str,
    block_height: Height,
    dust_limit: Option<Amount>,
) -> Result<Vec<[u8; 33]>> {
    let url = join(host_url, &format!("tweak-index/{}", block_height));
    let params = dust_limit
        .map(|dl| vec![("dustLimit", dl.to_sat().to_string())])
        .unwrap_or_default();
    let body = super::ureq_impl::get(agent, url.as_str(), &params)?;
    parse_tweaks(&body)
}

pub fn utxos(
    agent: &ureq::Agent,
    host_url: &str,
    block_height: Height,
) -> Result<Vec<UtxoResponse>> {
    let url = join(host_url, &format!("utxos/{}", block_height));
    let body = super::ureq_impl::get(agent, url.as_str(), &[])?;
    Ok(serde_json::from_str(&body)?)
}

pub fn spent_index(
    agent: &ureq::Agent,
    host_url: &str,
    block_height: Height,
) -> Result<SpentIndexResponse> {
    let url = join(host_url, &format!("spent-index/{}", block_height));
    let body = super::ureq_impl::get(agent, url.as_str(), &[])?;
    Ok(serde_json::from_str(&body)?)
}

pub fn filter_new_utxos(
    agent: &ureq::Agent,
    host_url: &str,
    block_height: Height,
) -> Result<FilterResponse> {
    let url = join(host_url, &format!("filter/new-utxos/{}", block_height));
    let body = super::ureq_impl::get(agent, url.as_str(), &[])?;
    Ok(serde_json::from_str(&body)?)
}

pub fn filter_spent(
    agent: &ureq::Agent,
    host_url: &str,
    block_height: Height,
) -> Result<FilterResponse> {
    let url = join(host_url, &format!("filter/spent/{}", block_height));
    let body = super::ureq_impl::get(agent, url.as_str(), &[])?;
    Ok(serde_json::from_str(&body)?)
}

pub fn forward_tx(agent: &ureq::Agent, host_url: &str, tx_hex: String) -> Result<Txid> {
    let url = join(host_url, "forward-tx");
    let request = ForwardTxRequest::new(tx_hex);
    let json_body = serde_json::to_string(&request)?;
    let body = super::ureq_impl::post_json(agent, url.as_str(), &json_body)?;
    Ok(serde_json::from_str(&body)?)
}

pub fn info(agent: &ureq::Agent, host_url: &str) -> Result<InfoResponse> {
    let url = join(host_url, "info");
    let body = super::ureq_impl::get(agent, url.as_str(), &[])?;
    Ok(serde_json::from_str(&body)?)
}

fn join(url: &str, route: &str) -> String {
    let url = url.trim_end_matches('/');
    format!("{url}/{route}")
}

fn parse_tweaks(body: &str) -> Result<Vec<[u8; 33]>> {
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
