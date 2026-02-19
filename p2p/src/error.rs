use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid peer address")]
    InvalidAddress,
    #[error("TCP connection failed: {0}")]
    TcpConnect(#[from] std::io::Error),
    #[error("failed to decode peer response")]
    Decode,
    #[error("failed to clone TCP stream")]
    StreamClone,
    #[error("failed to get local IP address")]
    LocalAddress,
    #[error("client not connected")]
    NotConnected,
    #[error("client stopped unexpectedly")]
    Stopped,
    #[error("connection timed out")]
    Timeout,
    #[error("DNS lookup failed: {0}")]
    DnsLookup(String),
}
