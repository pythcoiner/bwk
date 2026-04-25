# bwk-sp

**Experimental — do not use in production or with real coins. API will break.**

Silent Payments (BIP352) account using Blindbit backend for chain data.

High-level account orchestrator for Silent Payment wallets. Handles UTXO
scanning, coin management, and transaction history. Uses Blindbit oracle for
efficient SP-specific blockchain queries.

**Scope:** SP account lifecycle, stores (coins/txs/labels/scan state), background
scanner thread, SP address generation. Does NOT handle standard descriptor
wallets (use bwk) or direct Electrum queries (use bwk-electrum).

## Usage

```rust
use bwk_sp::{Account, Config, Notification, ScanMode};
use bitcoin::Network;

// Create account from SP keys
let config = Config::new(sp_client, Network::Signet)
    .blindbit_url("https://blindbit.example.com")
    .data_dir(data_path);
let mut account = Account::new(config)?;

// Take notification receiver
let receiver = account.receiver().unwrap();

// Start background scanning
account.start_scan(ScanMode::Continuous);

// Handle notifications
loop {
    match receiver.recv() {
        Ok(Notification::NewCoin(coin)) => {
            println!("Found coin: {} sats", coin.value);
        }
        Ok(Notification::ScanProgress { height, tip }) => {
            println!("Scanned {}/{}", height, tip);
        }
        _ => {}
    }
}
```

## Architecture

```
Blindbit oracle
     │
     ▼
Scanner thread ──► SpCoinStore (UTXOs from SP scanning)
     │
     ▼
SpTxStore (transaction history)
     │
     ▼
Notification ──► Account consumer
```

## Stores

- `SpCoinStore` — Detected SP outputs with spend status
- `SpTxStore` — Transaction history (incoming/outgoing)
- `bwk::LabelStore` — User labels for coins and transactions (shared with bwk)
- `ScanState` — Scan progress and checkpoint management
