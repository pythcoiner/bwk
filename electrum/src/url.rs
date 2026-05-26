//! Parsing of Electrum server URLs of the form `[scheme://]host[:port]`.
//!
//! Supported schemes are `tcp` (the default when no scheme is given) and
//! `ssl`. Any other `scheme://` prefix is a hard error.

/// Transport scheme for an Electrum connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElectrumScheme {
    /// Plaintext TCP (the default).
    Tcp,
    /// TLS/SSL.
    Ssl,
}

/// Parse an Electrum URL of the form `[scheme://]host[:port]`.
///
/// Returns `(host, port, scheme)` where `host` and `port` are `None` when
/// absent. An empty input yields `(None, None, Tcp)`. When no `scheme://`
/// prefix is present the scheme defaults to `Tcp`; an unknown `scheme://`
/// prefix is a hard error.
pub fn parse_electrum_url(
    url: &str,
) -> Result<(Option<String>, Option<u16>, ElectrumScheme), String> {
    if url.is_empty() {
        return Ok((None, None, ElectrumScheme::Tcp));
    }

    let (scheme, rest) = if let Some(rest) = url.strip_prefix("tcp://") {
        (ElectrumScheme::Tcp, rest)
    } else if let Some(rest) = url.strip_prefix("ssl://") {
        (ElectrumScheme::Ssl, rest)
    } else if let Some((bad_scheme, _)) = url.split_once("://") {
        return Err(format!("unsupported electrum scheme '{bad_scheme}'"));
    } else {
        (ElectrumScheme::Tcp, url)
    };

    if let Some((host, port)) = rest.rsplit_once(':') {
        if let Ok(port) = port.parse::<u16>() {
            return Ok((Some(host.to_string()), Some(port), scheme));
        }
    }

    Ok((Some(rest.to_string()), None, scheme))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty() {
        assert_eq!(
            parse_electrum_url("").unwrap(),
            (None, None, ElectrumScheme::Tcp)
        );
    }

    #[test]
    fn bare_host() {
        assert_eq!(
            parse_electrum_url("electrum.example.com").unwrap(),
            (
                Some("electrum.example.com".to_string()),
                None,
                ElectrumScheme::Tcp
            )
        );
    }

    #[test]
    fn host_and_port() {
        assert_eq!(
            parse_electrum_url("electrum.example.com:50001").unwrap(),
            (
                Some("electrum.example.com".to_string()),
                Some(50001),
                ElectrumScheme::Tcp
            )
        );
    }

    #[test]
    fn tcp_prefix() {
        assert_eq!(
            parse_electrum_url("tcp://host:50001").unwrap(),
            (Some("host".to_string()), Some(50001), ElectrumScheme::Tcp)
        );
    }

    #[test]
    fn ssl_prefix() {
        assert_eq!(
            parse_electrum_url("ssl://host:50002").unwrap(),
            (Some("host".to_string()), Some(50002), ElectrumScheme::Ssl)
        );
    }

    #[test]
    fn ssl_prefix_no_port() {
        assert_eq!(
            parse_electrum_url("ssl://host").unwrap(),
            (Some("host".to_string()), None, ElectrumScheme::Ssl)
        );
    }

    #[test]
    fn unknown_scheme_is_error() {
        assert!(parse_electrum_url("wss://host:443").is_err());
    }

    #[test]
    fn non_numeric_port_falls_back_to_host() {
        // The whole remainder is treated as host when the trailing part after
        // ':' is not a valid u16.
        assert_eq!(
            parse_electrum_url("host:notaport").unwrap(),
            (Some("host:notaport".to_string()), None, ElectrumScheme::Tcp)
        );
    }
}
