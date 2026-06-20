use bitcoin::{absolute::Height, secp256k1::PublicKey, Amount, Txid};

use crate::blindbit::error::{Error, Result};

use crate::blindbit::client::structs::InfoResponse;

use super::http_trait::HttpClient;
use super::structs::{
    BlockHeightResponse, FilterResponse, ForwardTxRequest, SpentIndexResponse, UtxoResponse,
};

/// Client for interacting with a Blindbit server.
///
/// Generic over the HTTP client implementation, allowing consumers to provide
/// their own HTTP client by implementing the `HttpClient` trait.
#[derive(Clone)]
pub struct BlindbitClient<H: HttpClient> {
    http_client: H,
    host_url: String,
}

impl<H: HttpClient> BlindbitClient<H> {
    /// Create a new Blindbit client with a custom HTTP client implementation.
    ///
    /// # Arguments
    /// * `host_url` - Base URL of the Blindbit server
    /// * `http_client` - HTTP client implementation
    pub fn new(mut host_url: String, http_client: H) -> Result<Self> {
        // we need a trailing slash, if not present we append it
        if !host_url.ends_with('/') {
            host_url = format!("{}/", host_url);
        }

        Ok(BlindbitClient {
            http_client,
            host_url,
        })
    }

    pub fn block_height(&self) -> Result<Height> {
        let url = join(&self.host_url, "block-height");
        let body = self.http_client.get(url.as_str(), &[])?;
        let blkheight: BlockHeightResponse = serde_json::from_str(&body)?;
        Ok(blkheight.block_height)
    }

    pub fn tweaks(
        &self,
        block_height: Height,
        dust_limit: Option<Amount>,
    ) -> Result<Vec<[u8; 33]>> {
        let url = join(&self.host_url, &format!("tweaks/{}", block_height));
        let params = dust_limit
            .map(|dl| vec![("dustLimit", dl.to_sat().to_string())])
            .unwrap_or_default();
        let body = self.http_client.get(url.as_str(), &params)?;
        parse_tweaks(&body)
    }

    pub fn tweak_index(
        &self,
        block_height: Height,
        dust_limit: Option<Amount>,
    ) -> Result<Vec<[u8; 33]>> {
        let url = join(&self.host_url, &format!("tweak-index/{}", block_height));
        let params = dust_limit
            .map(|dl| vec![("dustLimit", dl.to_sat().to_string())])
            .unwrap_or_default();
        let body = self.http_client.get(url.as_str(), &params)?;
        parse_tweaks(&body)
    }

    pub fn utxos(&self, block_height: Height) -> Result<Vec<UtxoResponse>> {
        let url = join(&self.host_url, &format!("utxos/{}", block_height));
        let body = self.http_client.get(url.as_str(), &[])?;
        Ok(serde_json::from_str(&body)?)
    }

    pub fn spent_index(&self, block_height: Height) -> Result<SpentIndexResponse> {
        let url = join(&self.host_url, &format!("spent-index/{}", block_height));
        let body = self.http_client.get(url.as_str(), &[])?;
        Ok(serde_json::from_str(&body)?)
    }

    pub fn filter_new_utxos(&self, block_height: Height) -> Result<FilterResponse> {
        let url = join(
            &self.host_url,
            &format!("filter/new-utxos/{}", block_height),
        );
        let body = self.http_client.get(url.as_str(), &[])?;
        Ok(serde_json::from_str(&body)?)
    }

    pub fn filter_spent(&self, block_height: Height) -> Result<FilterResponse> {
        let url = join(&self.host_url, &format!("filter/spent/{}", block_height));
        let body = self.http_client.get(url.as_str(), &[])?;
        Ok(serde_json::from_str(&body)?)
    }

    pub fn forward_tx(&self, tx_hex: String) -> Result<Txid> {
        let url = join(&self.host_url, "forward-tx");
        let request = ForwardTxRequest::new(tx_hex);
        let json_body = serde_json::to_string(&request)?;
        let body = self.http_client.post_json(url.as_str(), &json_body)?;
        Ok(serde_json::from_str(&body)?)
    }

    pub fn info(&self) -> Result<InfoResponse> {
        let url = join(&self.host_url, "info");
        let body = self.http_client.get(url.as_str(), &[])?;
        Ok(serde_json::from_str(&body)?)
    }
}

fn join(url: &str, route: &str) -> String {
    let url = url.trim_end_matches('/');
    format!("{url}/{route}")
}

/// Deserialize the oracle's tweak list (a JSON array of hex-encoded 33-byte
/// compressed points) into raw bytes WITHOUT validating the points. Point
/// validation is crypto and is deferred to the scan kernel, which runs on the
/// bounded compute threads, so the fetch workers stay pure I/O.
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
