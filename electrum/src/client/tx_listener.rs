use crate::electrum::{
    request::Request,
    response::{
        HistoryResult, Response, SHGetHistoryResponse, SHNotification, SHSubscribeResponse,
        TxGetResponse, TxGetResult,
    },
    types::ScriptHash,
};
use miniscript::bitcoin::{consensus, ScriptBuf, Transaction, Txid};
use std::{collections::BTreeMap, fmt::Debug, sync::mpsc};

use super::{
    listener::{run_listener, Dispatch, Flow},
    Client, CoinError, CoinRequest, CoinResponse,
};

/// Request-id and script-hash tracking shared by the request and response
/// halves of the listener.
#[derive(Default)]
struct TxState {
    req_id_spk: BTreeMap<usize /* request_id */, ScriptBuf>,
    watched_spks_sh: BTreeMap<usize /* request_id */, ScriptHash>,
    sh_spk: BTreeMap<ScriptHash, ScriptBuf>,
}

/// One read's worth of folded responses, grouped for batched emission.
#[derive(Default)]
struct TxBatch {
    statuses: BTreeMap<ScriptBuf, Option<String>>,
    txs: Vec<Transaction>,
    histories: BTreeMap<ScriptBuf, Vec<(Txid, Option<u64> /* height */)>>,
    /// Error responses, emitted after the grouped responses.
    trailing: Vec<CoinResponse>,
}

fn handle_tx_response(state: &mut TxState, batch: &mut TxBatch, r: Response) {
    match r {
        Response::SHSubscribe(SHSubscribeResponse { result: status, id }) => {
            let Some(sh) = state.watched_spks_sh.get(&id) else {
                log::warn!("Client::listen_txs() SHSubscribe: unknown id {id}");
                return;
            };
            let Some(spk) = state.sh_spk.get(sh) else {
                log::warn!("Client::listen_txs() SHSubscribe: unknown sh {sh}");
                return;
            };
            batch.statuses.insert(spk.clone(), status);
        }
        Response::SHNotification(SHNotification {
            status: (sh, status),
            ..
        }) => {
            let Some(spk) = state.sh_spk.get(&sh) else {
                log::warn!("Client::listen_txs() SHNotification: unknown sh {sh}");
                return;
            };
            batch.statuses.insert(spk.clone(), status);
        }
        Response::SHGetHistory(SHGetHistoryResponse { history, id }) => {
            let Some(spk) = state.req_id_spk.remove(&id) else {
                log::warn!("Client::listen_txs() SHGetHistory: unknown id {id}");
                return;
            };
            let mut spk_hist = vec![];
            for tx in history {
                let HistoryResult { txid, height, .. } = tx;
                let height = if height < 1 {
                    None
                } else {
                    Some(height as u64)
                };
                spk_hist.push((txid, height));
            }
            batch.histories.insert(spk, spk_hist);
        }
        Response::TxGet(TxGetResponse {
            result: TxGetResult::Raw(raw_tx),
            ..
        }) => match consensus::encode::deserialize_hex::<Transaction>(&raw_tx) {
            Ok(tx) => batch.txs.push(tx),
            Err(source) => batch
                .trailing
                .push(CoinResponse::Error(CoinError::TxDecode(source))),
        },
        Response::Error(e) => batch
            .trailing
            .push(CoinResponse::Error(CoinError::Server(e))),
        _ => {}
    }
}

/// Emission order: History, Status, Txs, then the trailing error responses.
fn drain_batch(batch: TxBatch) -> Vec<CoinResponse> {
    let TxBatch {
        statuses,
        txs,
        histories,
        trailing,
    } = batch;
    let mut out = Vec::new();
    if !histories.is_empty() {
        out.push(CoinResponse::History(histories));
    }
    if !statuses.is_empty() {
        out.push(CoinResponse::Status(statuses));
    }
    if !txs.is_empty() {
        out.push(CoinResponse::Txs(txs));
    }
    out.extend(trailing);
    out
}

fn send_batch<RS>(client: &mut Client, batch: Vec<Request>, send: &mpsc::Sender<RS>) -> Dispatch
where
    RS: From<CoinResponse>,
{
    if batch.is_empty() {
        return Dispatch::Empty;
    }
    log::debug!("Client::listen_txs() last_request = {}", batch.len());
    if client.send_with_retry(&batch, send, |e| {
        CoinResponse::Error(CoinError::Send(e)).into()
    }) {
        Dispatch::Sent(batch)
    } else {
        Dispatch::Terminate
    }
}

fn dispatch_tx<RS>(
    client: &mut Client,
    state: &mut TxState,
    rq: CoinRequest,
    send: &mpsc::Sender<RS>,
) -> Dispatch
where
    RS: From<CoinResponse>,
{
    log::debug!("Client::listen_txs() recv request: {}", rq.summary());
    match rq {
        CoinRequest::Subscribe(spks) => {
            let mut batch = vec![];
            for spk in spks {
                let mut sub = Request::subscribe_sh(&spk);
                let id = client.register(&mut sub);
                let sh = ScriptHash::new(&spk);
                state.watched_spks_sh.insert(id, sh);
                state.sh_spk.insert(sh, spk);
                batch.push(sub);
            }
            send_batch(client, batch, send)
        }
        CoinRequest::History(sbfs) => {
            let mut batch = vec![];
            for spk in sbfs {
                let mut history = Request::sh_get_history(&spk);
                let id = client.register(&mut history);
                state.req_id_spk.insert(id, spk);
                batch.push(history);
            }
            send_batch(client, batch, send)
        }
        CoinRequest::Txs(txids) => {
            let mut batch = vec![];
            for txid in txids {
                let mut tx = Request::tx_get(txid);
                client.register(&mut tx);
                batch.push(tx);
            }
            send_batch(client, batch, send)
        }
        CoinRequest::Stop => {
            let _ = send.send(CoinResponse::Stopped.into());
            Dispatch::Terminate
        }
    }
}

fn handle_tx_responses<RS>(
    state: &mut TxState,
    responses: Vec<Response>,
    send: &mpsc::Sender<RS>,
) -> Flow
where
    RS: From<CoinResponse>,
{
    let mut batch = TxBatch::default();
    for r in responses {
        handle_tx_response(state, &mut batch, r);
    }
    for rsp in drain_batch(batch) {
        log::debug!("Client::listen_txs() send response: {}", rsp.summary());
        if send.send(rsp.into()).is_err() {
            return Flow::Terminate;
        }
    }
    Flow::Continue
}

pub fn listen_txs<RQ, RS>(client: Client, send: mpsc::Sender<RS>, recv: mpsc::Receiver<RQ>)
where
    RQ: Into<CoinRequest> + Debug + Send + 'static,
    RS: From<CoinResponse> + Debug + Send + 'static,
{
    log::debug!("Client::listen_txs()");
    run_listener(
        client,
        send,
        recv,
        TxState::default(),
        dispatch_tx,
        |_client, state, responses, send| handle_tx_responses(state, responses, send),
        |e| CoinResponse::Error(CoinError::Transport(e)).into(),
    );
}
