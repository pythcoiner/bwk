use std::time::Duration;

use crate::blindbit::error::{Error, Result};

/// TLS config selecting the native-tls (openssl) provider. ureq defaults to
/// rustls, which we don't compile (the backend builds ureq with only the
/// `native-tls`/`vendored` providers); root certs default to the bundled WebPki
/// roots, so HTTPS works without a system cert store (e.g. Android).
fn native_tls_config() -> ureq::tls::TlsConfig {
    ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .build()
}

pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(native_tls_config())
        .timeout_global(Some(Duration::from_secs(30)))
        .max_idle_connections(200)
        .max_idle_connections_per_host(200)
        .build()
        .into()
}

pub fn get(agent: &ureq::Agent, url: &str, query_params: &[(&str, String)]) -> Result<String> {
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

pub fn post_json(agent: &ureq::Agent, url: &str, json_body: &str) -> Result<String> {
    let response = agent
        .post(url)
        .header("Content-Type", "application/json")
        .send(json_body)
        .map_err(|e| Error::HttpPost(e.to_string()))?
        .body_mut()
        .read_to_string()
        .map_err(|e| Error::ResponseBody(e.to_string()))?;

    Ok(response)
}
