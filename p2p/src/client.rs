use std::{
    io::{BufReader, Write},
    net::{SocketAddr, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, SystemTime},
};

use bitcoin::consensus::{encode, Decodable};
use bitcoin::p2p::{
    message::{self, NetworkMessage},
    message_blockdata::Inventory,
    Magic,
};
use bitcoin::{Block, BlockHash, Network};

use crate::connection::network_to_magic;
use crate::Error;

#[derive(Debug)]
pub struct Client {
    address: SocketAddr,
    network: Network,
    timeout: Duration,
    stream: Option<Arc<Mutex<TcpStream>>>,
    receiver: Option<mpsc::Receiver<NetworkMessage>>,
    stop: Arc<AtomicBool>,
}

impl Clone for Client {
    fn clone(&self) -> Self {
        Self {
            address: self.address,
            network: self.network,
            timeout: self.timeout,
            stream: None,
            receiver: None,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Client {
    pub fn new(address: SocketAddr, network: Network) -> Self {
        Self {
            address,
            network,
            timeout: Duration::from_millis(1000),
            stream: None,
            receiver: None,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn connect(mut self) -> Result<Self, Error> {
        self.stop.store(false, Ordering::Relaxed);
        let (stream, reader, _height) =
            crate::connection::connect(&self.address.to_string(), self.network)?;
        let stream = Arc::new(Mutex::new(stream));
        self.stream = Some(stream.clone());
        self.start_listen(reader, stream)?;
        Ok(self)
    }

    fn start_listen(
        &mut self,
        reader: BufReader<TcpStream>,
        stream: Arc<Mutex<TcpStream>>,
    ) -> Result<(), Error> {
        if !self.is_connected() {
            return Err(Error::NotConnected);
        }

        let (thread_sender, client_receiver) = mpsc::channel();
        self.receiver = Some(client_receiver);

        let magic = network_to_magic(self.network);
        let stop = self.stop.clone();

        std::thread::spawn(move || {
            Self::listen(stream, reader, thread_sender, magic, stop);
        });

        Ok(())
    }

    fn listen(
        stream: Arc<Mutex<TcpStream>>,
        mut reader: BufReader<TcpStream>,
        sender: mpsc::Sender<NetworkMessage>,
        magic: Magic,
        stop: Arc<AtomicBool>,
    ) {
        let mut fail = 0;
        loop {
            match message::RawNetworkMessage::consensus_decode(&mut reader) {
                Ok(msg) => match msg.into_payload() {
                    NetworkMessage::Ping(nonce) => {
                        let pong_msg = NetworkMessage::Pong(nonce);
                        let msg = message::RawNetworkMessage::new(magic, pong_msg);
                        stream
                            .lock()
                            .expect("poisoned")
                            .write_all(encode::serialize(&msg).as_slice())
                            .unwrap();
                    }
                    m @ NetworkMessage::Block(_) => {
                        let _ = sender.send(m);
                    }
                    NetworkMessage::Addr(a) => {
                        let _ = sender.send(NetworkMessage::Addr(a.clone()));
                    }
                    _ => {}
                },
                Err(e) => {
                    fail += 1;
                    if fail > 3 {
                        log::warn!("P2P decode error: {e:?}");
                    }
                    stop.store(true, Ordering::Relaxed);
                }
            }
            if stop.load(Ordering::Relaxed) {
                return;
            }
        }
    }

    pub fn stop(&mut self) {
        self.stream = None;
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn is_started(&self) -> bool {
        self.receiver.is_some()
    }

    pub fn is_connected(&self) -> bool {
        self.stream.is_some() || self.is_started()
    }

    pub fn get_block(&mut self, block_hash: BlockHash) -> Result<Option<Block>, Error> {
        if !self.is_connected() {
            return Err(Error::NotConnected);
        }
        let stream = if let Some(stream) = &self.stream {
            stream.clone()
        } else {
            return Err(Error::NotConnected);
        };
        let timeout = self.timeout;
        let mut back_off = bwk_backoff::Backoff::new_ms(20);
        if let Some(receiver) = self.receiver.as_mut() {
            let block = Inventory::Block(block_hash);
            let getdata_msg = message::NetworkMessage::GetData(vec![block]);
            let magic = network_to_magic(self.network);
            let get_block = message::RawNetworkMessage::new(magic, getdata_msg);
            let _ = stream
                .lock()
                .expect("poisoned")
                .write_all(encode::serialize(&get_block).as_slice());
            let send = SystemTime::now();

            loop {
                if let Ok(NetworkMessage::Block(block)) = receiver.try_recv() {
                    return Ok(Some(block));
                } else {
                    let elapsed = SystemTime::now().duration_since(send).expect("valid time");
                    if elapsed > timeout {
                        return Ok(None);
                    } else {
                        back_off.snooze();
                    }
                }
                if self.stop.load(Ordering::Relaxed) {
                    return Err(Error::Stopped);
                }
            }
        } else {
            Err(Error::NotConnected)
        }
    }

    pub fn get_addr(&mut self) -> Result<Option<Vec<SocketAddr>>, Error> {
        if !self.is_connected() {
            return Err(Error::NotConnected);
        }
        let stream = if let Some(stream) = &self.stream {
            stream.clone()
        } else {
            return Err(Error::NotConnected);
        };
        let timeout = self.timeout;
        let mut back_off = bwk_backoff::Backoff::new_ms(20);
        if let Some(receiver) = self.receiver.as_mut() {
            let getdata_msg = message::NetworkMessage::GetAddr;
            let magic = network_to_magic(self.network);
            let get_addr = message::RawNetworkMessage::new(magic, getdata_msg);
            let _ = stream
                .lock()
                .expect("poisoned")
                .write_all(encode::serialize(&get_addr).as_slice());
            let send = SystemTime::now();

            loop {
                if let Ok(NetworkMessage::Addr(vec)) = receiver.try_recv() {
                    let addresses = vec
                        .into_iter()
                        .filter_map(|(_, a)| a.socket_addr().ok())
                        .collect();
                    return Ok(Some(addresses));
                } else {
                    let elapsed = SystemTime::now().duration_since(send).expect("valid time");
                    if elapsed > timeout {
                        return Ok(None);
                    } else {
                        back_off.snooze();
                    }
                }
                if self.stop.load(Ordering::Relaxed) {
                    return Err(Error::Stopped);
                }
            }
        } else {
            Err(Error::NotConnected)
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use bitcoin::{BlockHash, Network};
    use corepc_node::{Client as RpcClient, Conf, P2P};
    use std::{net::SocketAddr, str::FromStr, time::Duration};

    use crate::dns::fetch_peers;

    use super::Client;

    fn get_height(client: &mut RpcClient) -> u64 {
        client.get_block_count().unwrap().0
    }

    fn generate_blocks(client: &mut RpcClient, blocks: usize) {
        let height = get_height(client);
        for _ in 0..blocks {
            let addr = client.new_address().unwrap();
            client.generate_to_address(1, &addr).unwrap();
        }
        let new_height = get_height(client);
        assert_eq!(new_height, height + blocks as u64);
    }

    #[test]
    fn test_get_block() {
        let mut conf = Conf::default();
        conf.p2p = P2P::Yes;
        let mut node = corepc_node::Node::from_downloaded_with_conf(&conf).unwrap();
        let p2p_addr = match node.p2p_connect(true).unwrap() {
            P2P::Connect(addr, _) => SocketAddr::V4(addr),
            _ => panic!(),
        };

        let bitcoind = &mut node.client;

        generate_blocks(bitcoind, 200);

        let bh = bitcoind.get_block_hash(125).unwrap().block_hash().unwrap();

        let mut p2p_client = Client::new(p2p_addr, Network::Regtest)
            .timeout(Duration::from_millis(500))
            .connect()
            .unwrap();
        assert!(p2p_client.is_connected());
        assert!(p2p_client.is_started());

        let block = p2p_client.get_block(bh).unwrap().unwrap();
        assert_eq!(bh, block.block_hash());

        let fake_hash =
            BlockHash::from_str("0000000000000000000000000000000000000000000000000000000000000000")
                .unwrap();
        assert!(p2p_client.get_block(fake_hash).unwrap().is_none());
    }

    #[test]
    #[ignore] // requires internet connection
    fn test_seed_peers() {
        let peers = fetch_peers("seed.bitcoin.sipa.be").unwrap();

        let mut failed = 0;
        let len = peers.len();
        for (index, peer) in peers.into_iter().enumerate() {
            println!("{}/{}", index + 1, len);
            let client = Client::new(peer, Network::Bitcoin).connect();
            let mut client = match client {
                Ok(c) => c,
                Err(_) => {
                    failed += 1;
                    continue;
                }
            };
            let addrs = match client.get_addr() {
                Ok(Some(a)) => a,
                Err(_) => {
                    failed += 1;
                    continue;
                }
                _ => continue,
            };

            println!("received {} peers addresses", addrs.len());
        }

        if (failed * 10) > len {
            panic!("{failed} failed!")
        }
        let success = len - failed;
        println!("Success: {}/{}", success, len);
    }
}
