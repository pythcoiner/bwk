mod listener;
pub mod tx_listener;
use crate::{
    electrum::{
        request::Request,
        response::{
            ErrorResponse, ErrorResult, Response, SHGetHistoryResponse, TxBroadcastResponse,
            TxGetResponse, TxGetResult,
        },
        types::ScriptHash,
    },
    raw_client::{self, Client as RawClient},
};
use bwk_utils::short_string;
use hex_conservative::FromHex;
use miniscript::bitcoin::{
    consensus::{self, encode::serialize_hex, Decodable},
    OutPoint, Script, ScriptBuf, Transaction, TxOut, Txid,
};
use std::{
    collections::{BTreeMap, HashMap},
    fmt::{Debug, Display},
    sync::mpsc,
    thread::{self},
    time::{Duration, Instant},
};
use tx_listener::listen_txs;

const SEND_MAX_RETRIES: usize = 3;
const SEND_RETRY_DELAY: Duration = Duration::from_millis(300);

#[derive(Debug, Clone)]
pub enum Error {
    Electrum(String),
    TxParsing,
    WrongResponse,
    WrongOutPoint,
    TxDoesNotExists,
    Rejected(String),
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Electrum(e) => write!(f, "{e:?}"),
            Error::TxParsing => write!(f, "Fail to parse the transaction"),
            Error::WrongResponse => write!(f, "Wrong response from electrum server"),
            Error::WrongOutPoint => write!(f, "Requested outpoint did not exists"),
            Error::TxDoesNotExists => write!(f, "Requested transaction did not exists"),
            Error::Rejected(msg) => write!(f, "server rejected transaction: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<raw_client::Error> for Error {
    fn from(value: raw_client::Error) -> Self {
        Error::Electrum(format!("{value:?}"))
    }
}

pub fn short_hash(s: &ScriptBuf) -> String {
    let s = ScriptHash::new(s).to_string();
    short_string(s, 10)
}

#[derive(Clone)]
pub enum CoinRequest {
    Subscribe(Vec<ScriptBuf>),
    History(Vec<ScriptBuf>),
    Txs(Vec<Txid>),
    Stop,
}

impl Debug for CoinRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Subscribe(vec) => {
                let hashes: Vec<_> = vec.iter().map(short_hash).collect();
                f.debug_tuple("Subscribe").field(&hashes).finish()
            }
            Self::History(vec) => {
                let hashes: Vec<_> = vec.iter().map(short_hash).collect();
                f.debug_tuple("History").field(&hashes).finish()
            }
            Self::Txs(arg0) => f.debug_tuple("Txs").field(arg0).finish(),
            Self::Stop => write!(f, "Stop"),
        }
    }
}

impl CoinRequest {
    /// Compact one-line description (counts only) for hot-path logging.
    pub fn summary(&self) -> String {
        match self {
            Self::Subscribe(v) => format!("Subscribe({})", v.len()),
            Self::History(v) => format!("History({})", v.len()),
            Self::Txs(v) => format!("Txs({})", v.len()),
            Self::Stop => "Stop".to_string(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CoinError {
    #[error("failed to send batch request: {0}")]
    Send(#[source] raw_client::Error),
    #[error("failed to decode transaction: {0}")]
    TxDecode(consensus::encode::FromHexError),
    #[error("server error: {0}")]
    Server(ErrorResponse),
    #[error("transport error: {0}")]
    Transport(#[source] raw_client::Error),
}

pub enum CoinResponse {
    Status(BTreeMap<ScriptBuf, Option<String>>),
    History(BTreeMap<ScriptBuf, Vec<(Txid, Option<u64> /* height */)>>),
    Txs(Vec<Transaction>),
    Stopped,
    Error(CoinError),
}

impl Debug for CoinResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Txs(vec) => {
                let txids: Vec<_> = vec.iter().map(|tx| tx.compute_txid()).collect();
                f.debug_tuple("Txs").field(&txids).finish()
            }
            Self::Status(map) => {
                let statuses: Vec<_> = map
                    .iter()
                    .map(|(spk, status)| {
                        format!(
                            "{} => {:?}",
                            short_hash(spk),
                            status.as_ref().map(|st| short_string(st.to_string(), 10))
                        )
                    })
                    .collect();
                f.debug_tuple("Status").field(&statuses).finish()
            }
            Self::History(map) => {
                let map: Vec<_> = map
                    .iter()
                    .map(|(spk, v)| {
                        let conf: Vec<_> =
                            v.iter().filter(|(_, height)| height.is_some()).collect();
                        format!(
                            "{} => conf: {}, total: {}",
                            short_hash(spk),
                            conf.len(),
                            v.len()
                        )
                    })
                    .collect();
                f.debug_tuple("History").field(&map).finish()
            }
            Self::Stopped => write!(f, "Stopped"),
            Self::Error(e) => write!(f, "Error({e})"),
        }
    }
}

impl CoinResponse {
    /// Compact one-line description (counts only) for hot-path logging.
    pub fn summary(&self) -> String {
        match self {
            Self::Status(m) => format!("Status({})", m.len()),
            Self::History(m) => format!("History({} scripts)", m.len()),
            Self::Txs(v) => format!("Txs({})", v.len()),
            Self::Stopped => "Stopped".to_string(),
            Self::Error(e) => format!("Error({e})"),
        }
    }
}

#[derive(Debug)]
pub struct Client {
    inner: RawClient,
    index: HashMap<usize, Request>,
    last_id: usize,
    url: String,
    port: u16,
}

impl Client {
    /// Create a new electrum client.
    ///
    /// # Arguments
    /// * `address` - url/ip of the electrum server as String
    /// * `port` - port of the electrum server
    pub fn new(address: &str, port: u16) -> Result<Self, Error> {
        let ssl = address.starts_with("ssl://");
        let address = address.to_string().replace("ssl://", "");
        let mut inner = RawClient::new_ssl_maybe(&address, port, ssl);
        inner.try_connect(None)?;
        Ok(Client {
            inner,
            index: HashMap::new(),
            last_id: 0,
            url: address,
            port,
        })
    }

    /// Create a new local electrum client: SSL certificate validation is disabled in
    ///   order to be used with self-signed certificates.
    ///
    /// # Arguments
    /// * `address` - url/ip of the electrum server as String
    /// * `port` - port of the electrum server
    pub fn new_local(address: &str, port: u16) -> Result<Self, Error> {
        let ssl = address.starts_with("ssl://");
        let address = address.to_string().replace("ssl://", "");
        let mut inner = RawClient::new_ssl_maybe(&address, port, ssl).verif_certificate(false);
        inner.try_connect(None)?;
        Ok(Client {
            inner,
            index: HashMap::new(),
            last_id: 0,
            url: address,
            port,
        })
    }

    /// Generate a new request id
    fn id(&mut self) -> usize {
        self.last_id = self.last_id.wrapping_add(1);
        self.last_id
    }

    fn register(&mut self, req: &mut Request) -> usize {
        let id = self.id();
        req.id = id;
        self.index.insert(req.id, req.clone());
        id
    }

    /// Send `batch`, retrying a bounded number of times on transport error.
    /// On give-up the error is reported to `send` via `make_err`. Returns
    /// `false` when the consumer channel is closed, so the caller can stop.
    fn send_with_retry<RS>(
        &mut self,
        batch: &[Request],
        send: &mpsc::Sender<RS>,
        make_err: impl Fn(raw_client::Error) -> RS,
    ) -> bool {
        let mut retry = 0usize;
        while let Err(e) = self.inner.try_send_batch(batch.iter().collect()) {
            retry += 1;
            if retry > SEND_MAX_RETRIES {
                return send.send(make_err(e)).is_ok();
            }
            thread::sleep(SEND_RETRY_DELAY);
        }
        true
    }

    /// Spawn the tx listener on a background thread. `RQ`/`RS` are generic
    /// so this listener can be used standalone, outside a bwk/bwk-sp
    /// `Account`, with the consumer's own wrapper request/response types
    /// instead of `CoinRequest`/`CoinResponse` directly.
    pub fn listen_txs<RQ, RS>(self) -> (mpsc::Sender<RQ>, mpsc::Receiver<RS>)
    where
        RQ: Into<CoinRequest> + Debug + Send + 'static,
        RS: From<CoinResponse> + Debug + Send + 'static,
    {
        let (sender, request) = mpsc::channel();
        let (response, receiver) = mpsc::channel();
        thread::spawn(move || listen_txs(self, response, request));

        (sender, receiver)
    }

    /// Try to get a transaction by its txid
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///   - fail to send the request
    ///   - parsing response fails
    ///   - the response is not of expected type
    ///   - the transaction does not exist
    pub fn get_tx(&mut self, txid: Txid) -> Result<Transaction, Error> {
        let request = Request::tx_get(txid).id(self.id());
        self.inner.try_send(&request)?;
        let req_id = request.id;
        self.index.insert(request.id, request);
        let resp = match self.inner.recv(&self.index) {
            Ok(r) => r,
            Err(e) => {
                self.index.remove(&req_id);
                return Err(e.into());
            }
        };
        for r in resp {
            if let Response::TxGet(TxGetResponse {
                id,
                result: TxGetResult::Raw(res),
            }) = r
            {
                if req_id == id {
                    self.index.remove(&req_id);
                    let raw_tx = match Vec::<u8>::from_hex(&res) {
                        Ok(raw) => raw,
                        Err(_) => {
                            return Err(Error::TxParsing);
                        }
                    };
                    let tx: Result<Transaction, _> =
                        Decodable::consensus_decode(&mut raw_tx.as_slice());
                    return tx.map_err(|_| Error::TxParsing);
                }
            } else if let Response::Error(ErrorResponse { id, .. }) = r {
                if req_id == id {
                    self.index.remove(&req_id);
                    // NOTE: it's very likely if we receive an error response from the server
                    // it's because the txid does not match any Transaction, but maybe we can
                    // do a better handling of the error case (for this we need check if responses
                    // from all electrum server implementations are consistant).
                    return Err(Error::TxDoesNotExists);
                }
            }
        }
        self.index.remove(&req_id);
        Err(Error::WrongResponse)
    }

    /// Get coins that pay to the given spk and their related transaction.
    /// This method will make several calls to the electrum server:
    ///   - it will first request a list of all transactions txid that have
    ///     an output paying to the spk.
    ///   - it will then fetch all txs, store them and extract all the coins
    ///     that pay to the given spk.
    ///   - it will return a list of (TxOut, OutPoint) and a map of transactions.
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///   - a call to the electrum server fail
    #[allow(clippy::type_complexity)]
    pub fn get_coins_at(
        &mut self,
        script: &Script,
    ) -> Result<(Vec<(TxOut, OutPoint)>, HashMap<Txid, Transaction>), Error> {
        let mut txouts = Vec::new();
        let mut transactions = HashMap::new();
        let txs = self.get_coins_tx_at(script)?;
        for txid in txs {
            let tx = self.get_tx(txid)?;
            for (i, txout) in tx.output.iter().enumerate() {
                if *txout.script_pubkey == *script {
                    let outpoint = OutPoint {
                        txid,
                        vout: i as u32,
                    };
                    txouts.push((txout.clone(), outpoint));
                }
            }
            transactions.insert(txid, tx);
        }
        Ok((txouts, transactions))
    }

    /// Get a list of txid of all transaction that have an output paying to the
    ///   given spk
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///   - fail sending the request
    ///   - receive a wrong response
    pub fn get_coins_tx_at(&mut self, script: &Script) -> Result<Vec<Txid>, Error> {
        let request = Request::sh_get_history(script).id(self.id());
        self.inner.try_send(&request)?;
        let req_id = request.id;
        self.index.insert(request.id, request);
        let resp = match self.inner.recv(&self.index) {
            Ok(r) => r,
            Err(e) => {
                self.index.remove(&req_id);
                return Err(e.into());
            }
        };
        for r in resp {
            if let Response::SHGetHistory(SHGetHistoryResponse { id, history }) = r {
                if req_id == id {
                    self.index.remove(&req_id);
                    let history: Vec<_> = history.into_iter().map(|r| r.txid).collect();
                    return Ok(history);
                }
            }
        }
        self.index.remove(&req_id);
        Err(Error::WrongResponse)
    }

    /// Broadcast the given transaction.
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///   - fail to send the request
    ///   - get a wrong response
    pub fn broadcast(&mut self, tx: &Transaction) -> Result<(), Error> {
        let raw_tx = serialize_hex(tx);
        log::debug!("electrum::Client().broadcast(): {raw_tx:?}");
        let request = Request::tx_broadcast(raw_tx);
        self.inner.try_send(&request)?;
        let req_id = request.id;
        self.index.insert(request.id, request);
        let resp = match self.inner.recv(&self.index) {
            Ok(r) => r,
            Err(e) => {
                self.index.remove(&req_id);
                return Err(e.into());
            }
        };
        log::debug!("electrum::Client().broadcast(): receive response: {resp:?}");
        for r in resp {
            if let Response::TxBroadcast(TxBroadcastResponse { id, .. }) = r {
                if req_id == id {
                    self.index.remove(&req_id);
                    return Ok(());
                }
            }
        }
        self.index.remove(&req_id);
        Err(Error::WrongResponse)
    }

    /// Broadcast a fully signed transaction and return its txid on success.
    ///
    /// Unlike [`Client::broadcast`], a server rejection (`Response::Error`) is
    /// surfaced as [`Error::Rejected`] with the server's message instead of
    /// the opaque [`Error::WrongResponse`].
    pub fn broadcast_tx(&mut self, tx: &Transaction) -> Result<Txid, Error> {
        let raw_tx = serialize_hex(tx);
        let request = Request::tx_broadcast(raw_tx);
        let req_id = request.id;
        self.inner.try_send(&request)?;
        self.index.insert(req_id, request);
        // electrs may answer with an empty response while it is still busy with a
        // prior request, which surfaces as a parse error rather than our answer.
        // Re-read until the response carrying our id arrives (or we give up),
        // instead of failing on the transient empty line.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let resp = match self.inner.recv(&self.index) {
                Ok(r) => r,
                Err(_) if Instant::now() < deadline => continue,
                Err(e) => {
                    self.index.remove(&req_id);
                    return Err(e.into());
                }
            };
            for r in resp {
                match r {
                    Response::TxBroadcast(TxBroadcastResponse { id, .. }) if id == req_id => {
                        self.index.remove(&req_id);
                        return Ok(tx.compute_txid());
                    }
                    Response::Error(ErrorResponse {
                        id,
                        error: ErrorResult { message, .. },
                    }) if id == req_id => {
                        self.index.remove(&req_id);
                        return Err(Error::Rejected(message));
                    }
                    _ => {}
                }
            }
            if Instant::now() >= deadline {
                self.index.remove(&req_id);
                return Err(Error::WrongResponse);
            }
        }
    }

    /// Returns the URL of the electrum client.
    ///
    /// # Returns
    /// A `String` containing the URL of the electrum server.
    pub fn url(&self) -> String {
        self.url.clone()
    }

    /// Returns the port of the electrum client.
    ///
    /// # Returns
    /// A `u16` containing the port of the electrum server.
    pub fn port(&self) -> u16 {
        self.port
    }
}
