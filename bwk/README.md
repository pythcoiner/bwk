# bwk

**Experimental — do not use in production or with real coins. API will break.**

Descriptor-based Bitcoin wallet account using Electrum for chain data.

High-level account orchestrator that ties together UTXO tracking, address
generation, transaction history, and labels. Handles background sync with
Electrum server and emits notifications on state changes.

**Scope:** Account lifecycle, stores (coins/txs/addresses/labels), Electrum
sync thread, change address management. Does NOT handle signing (use bwk-sign)
or transaction construction (use bwk-tx).

## Usage

```rust
use bwk::{Account, Config};
use miniscript::bitcoin::Network;

// Create account from descriptor
let config = Config::new(descriptor, Network::Regtest)
    .electrum("ssl://127.0.0.1", 50001)
    .data_dir(data_path);
let mut account = Account::try_new(config).expect("failed to open account");

// Take notification receiver (can only be called once)
let receiver = account.receiver().unwrap();

// Start Electrum sync
account.start();

// Handle notifications
loop {
    match receiver.recv() {
        Ok(Notification::CoinUpdate) => {
            let state = account.coin_state();
            println!("Balance: {}", state.confirmed_balance);
        }
        Ok(Notification::Stopped) => break,
        _ => {}
    }
}
```

## Architecture

```
Electrum server
     │
     ▼
listen_txs() thread ◄──► AddressStore (watch tip updates)
     │
     ▼
CoinStore.handle_history_response() / handle_txs_response()
     │
     ▼
TxStore (raw txs + metadata)
     │
     ▼
CoinStore.generate() ──► coins cache + address statuses
     │
     ▼
Notification::CoinUpdate ──► Account consumer
```

## Store Relationships

- `TxStore`: Source of truth for transactions. Persisted to JSON.
- `CoinStore`: Generated cache from TxStore. Tracks UTXOs and their status
  (Unconfirmed/ConfirmedUnverified/Confirmed/Spent). A confirmed coin follows
  the inclusion lifecycle Unconfirmed -> ConfirmedUnverified (server reports a
  height, its header is known) -> Verified (a merkle proof checks against that
  header). A CTA pass that mutates tx state emits `Notification::HeaderStoreUpdated`;
  a failed merkle proof or a header store that fails its own validation emits
  `Notification::ValidationFailed`.
- `AddressStore`: Tracks generated addresses (recv/change tips + look_ahead).
  Notifies Electrum thread when new addresses need watching.
- `LabelStore`: User labels keyed by OutPoint or Txid.
- `HeaderStore`: Shared validated chain-header store used to verify merkle
  proofs and track reorgs across accounts.

Stores wrap the typed `bwk-persist::Store` abstraction. Serialization lives in
store-local encode/decode helpers; file or database layout details live in
`PersistenceBackend` implementations.

## Address Generation

`recv_generated_tip` / `change_generated_tip` track last user-requested index.
`recv_watch_tip` / `change_watch_tip` = generated_tip + look_ahead + 1.

Electrum subscribes to all SPKs up to watch_tip. When a coin is received at
index N, if N >= generated_tip, the tip advances and new addresses get watched.

## Change Output Derivation

`ChangeRecipientProvider` implements `RecipientProvider` for TxBuilder.
Uses `ChangeTipUpdater` to increment change_generated_tip on each
`create_script()` call. The index is stored in a `Cell` so `psbt_output_info()`
can return correct BIP32 derivation path.
