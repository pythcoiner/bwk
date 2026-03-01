# bwk-electrum

**Experimental — do not use in production or with real coins. API will break.**

Electrum protocol client with TCP/SSL support.

Implements the Electrum JSON-RPC protocol for querying blockchain data. Provides
both a threaded async listener (for wallet sync) and sync methods (for one-off
queries). Handles request batching and connection management.

**Scope:** Electrum protocol, script subscription, transaction history/fetch,
broadcasting. Does NOT parse transactions (use rust-bitcoin) or track UTXOs
(use bwk crate).

## Usage

### Async Listener (Recommended)

```rust
use bwk_electrum::client::{Client, CoinRequest, CoinResponse};

// Connect to server
let client = Client::new("ssl://electrum.example.com", 50002)?;

// Start listener thread, get channel pair
let (sender, receiver) = client.listen::<CoinRequest, CoinResponse>();

// Subscribe to scripts
let scripts = vec![my_script_pubkey];
sender.send(CoinRequest::Subscribe(scripts))?;

// Handle responses
loop {
    match receiver.recv()? {
        CoinResponse::Status(statuses) => {
            for (spk, status) in statuses {
                println!("Script status changed: {:?}", status);
            }
        }
        CoinResponse::History(histories) => {
            for (spk, txs) in histories {
                println!("Found {} transactions", txs.len());
            }
        }
        CoinResponse::Txs(transactions) => {
            for tx in transactions {
                println!("Got tx: {}", tx.compute_txid());
            }
        }
        CoinResponse::Stopped => break,
        CoinResponse::Error(e) => eprintln!("Error: {}", e),
    }
}
```

### Sync Methods

```rust
let mut client = Client::new("127.0.0.1", 50001)?;

// Fetch transaction
let tx = client.get_tx(txid)?;

// Get UTXOs at script
let (coins, txs) = client.get_coins_at(&script)?;

// Broadcast transaction
client.broadcast(&signed_tx)?;
```

## Architecture

```
Consumer
    │
    ├──► mpsc::Sender<CoinRequest>
    │         │
    │         ▼
    │    listen_txs() thread
    │         │
    │         ▼
    │    RawClient (TCP or SSL)
    │         │
    │         ▼
    │    Electrum server
    │
    └──◄ mpsc::Receiver<CoinResponse>
```

## Request/Response Types

**Requests:**
- `CoinRequest::Subscribe(Vec<ScriptBuf>)` — Subscribe to script status changes
- `CoinRequest::History(Vec<ScriptBuf>)` — Get transaction history for scripts
- `CoinRequest::Txs(Vec<Txid>)` — Fetch raw transactions
- `CoinRequest::Stop` — Stop the listener thread

**Responses:**
- `CoinResponse::Status(BTreeMap<ScriptBuf, Option<String>>)` — Script statuses
- `CoinResponse::History(BTreeMap<ScriptBuf, Vec<(Txid, Option<u64>)>>)` — Tx history
- `CoinResponse::Txs(Vec<Transaction>)` — Raw transactions
- `CoinResponse::Stopped` — Listener stopped
- `CoinResponse::Error(String)` — Error message

## Local Development

For self-signed certificates (regtest):
```rust
let client = Client::new_local("127.0.0.1", 50001)?;
```
