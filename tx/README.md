# bwk-tx

**Experimental. Do not use in production or with real coins. API will break.**

Transaction building, coin selection, and PSBT generation.

Provides `TxBuilder` for constructing transactions with automatic coin selection
and fee estimation. Supports both standard descriptor outputs and Silent Payment
recipients through the `RecipientProvider` trait.

**Scope:** Transaction construction, coin selection algorithm, fee calculation,
PSBT creation. Does NOT handle signing (use bwk-sign), broadcasting (use
bwk-electrum), or UTXO tracking (use bwk or bwk-sp).

## Usage

```rust
use bwk_tx::{TxBuilder, Recipient, Fees, Coin};

// Create builder with change provider
let mut builder = TxBuilder::new(change_provider);

// Add recipient
builder.add_recipient(Recipient::new(address, 50_000));

// Set fee rate
builder.fees(Fees::SatsVb(2));

// Set coin source for selection
builder.coin_source(wallet_coins);

// Build transaction
let result = builder.generate()?;
let psbt = result.psbt;
```

## Key Traits

- `RecipientProvider`: Creates outputs (address + amount + PSBT metadata)
- `CoinSource`: Provides spendable coins for selection
- `ChangeTip`: Manages change address index progression
- `CoinCandidate`: Coin with value and satisfaction weight for selection

## Coin Selection

Weighted random selection algorithm that:
- Prefers fewer inputs to reduce fees
- Avoids dust change outputs
- Respects target feerate

## Types

- `TxBuilder`: Main builder with fluent API
- `TxTemplate`: Inputs, outputs, and fee specification
- `Coin`: UTXO with script, value, and derivation info
- `Fees`: Fee specification (sat/vB, msat/vB, or absolute)
