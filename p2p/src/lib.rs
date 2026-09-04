mod client;
mod connection;
mod dns;
mod error;

pub use client::Client;
pub use connection::network_to_magic;
pub use dns::{fetch_peers, fetch_peers_with_port, DNS_SEED_SERVERS};
pub use error::Error;
