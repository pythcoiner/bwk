# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with
code in this repository.

## Project Overview

Bitcoin Wallet Kit (bwk) - a modular Rust workspace providing components for
building Bitcoin wallets. Experimental, API unstable.

See [CODE_GUIDELINES.md](CODE_GUIDELINES.md) for code style rules.

## Build Commands

```bash
# Build release
cargo build --release

# Run all tests (requires test feature)
cargo test --features test

# Run tests with output
cargo test --features test -- --nocapture

# Single test
cargo test --features test <test_name>

# Lint
cargo clippy --all-targets -- -D warnings
cargo fmt -- --check
```

MSRV: 1.88

**Before committing:** run `scripts/ci-checks.sh`. It is the single definition
of green for this repo (fmt check, clippy, tests, release build, in that
order), and every CI job runs a section of it; pass a section name to run just
one, e.g. `scripts/ci-checks.sh clippy`. Commit messages must be single-line
and follow existing style (e.g., `crate: short description`).

## Workspace Crates

```
+----------------+-------------------------------------------------------------+
| Crate          | Purpose                                                     |
+----------------+-------------------------------------------------------------+
| bwk            | Main library - Account orchestrator for descriptor-based    |
|                | wallets (Electrum backend)                                  |
| bwk-sp         | Silent Payments account orchestrator (BIP352, Blindbit)     |
| bwk-tx         | Transaction building, coin selection, fee estimation, PSBT  |
| bwk-electrum   | Electrum protocol client (TCP/SSL)                          |
| bwk-sign       | Hot signer, SigningManager for BIP32 key management         |
| bwk-descriptor | Miniscript descriptor handling, SpkDerivator                |
| bwk-keys       | Key derivation utilities (OXpriv, OXpub, KeyDerivator)      |
| bwk-p2p        | Bitcoin P2P network client, DNS seed resolution             |
| bwk-error      | In-house derive for error impls, reached as `thiserror`     |
| bwk-backoff    | Exponential backoff utility                                 |
| bwk-utils      | Test helpers (behind `test` feature)                        |
+----------------+-------------------------------------------------------------+
```

See crate READMEs for usage examples:
- [bwk/README.md](bwk/README.md): Account, stores, address generation
- [sign/README.md](sign/README.md): SigningManager, Signer trait
- [descriptor/README.md](descriptor/README.md): SpkDerivator, descriptor helpers
- [electrum/README.md](electrum/README.md): Electrum client modes
- [error/README.md](error/README.md): the in-house error derive

## Architecture

### Standard Wallet (`bwk`)
```
Account (bwk/src/account.rs)
├── CoinStore (manages UTXOs, derives addresses via SpkDerivator)
├── TxStore (transaction history)
├── LabelStore (user labels)
├── SigningManager (hot signers)
├── HeaderStore (validated header chain for SPV verification)
└── Electrum client thread (background sync)
```

### Silent Payments Wallet (`bwk-sp`)
```
Account (sp/src/account.rs)
├── SpReceiver - SP key management and scan matching
├── Blindbit transport - blockchain data via Blindbit oracle
├── SpCoinStore, SpTxStore, SpLabelStore, ScanState
└── Scanner thread (background block scanning)
```

### Transaction Building (`bwk-tx`)
```
TxBuilder
├── RecipientProvider trait - outputs (Recipient, SpRecipient, change providers)
├── CoinSource trait - input selection
├── coin_selection module - weighted random selection algorithm
└── TxTemplate -> Psbt via generate()
```

Key traits:
- `RecipientProvider`: Defines transaction outputs (address, amount, PSBT
  metadata)
- `CoinSource`: Provides spendable coins for selection
- `ChangeTip`: Manages change address index progression
- `Signer`: Signs PSBTs (implemented by HotSigner)

### Notification Pattern

Both Account types use `mpsc::channel<Notification>` for async events
(connection status, new coins, scan progress). Call `account.receiver()` to
take the receiver.

## Testing

Integration tests require the `test` feature flag which enables:
- `bwk-utils/test`: Test helpers (funding_tx, corepc_node utilities)
- `bwk-sign/test`: Test signer constructors
- `bwk-tx/test`: TxBuilder test methods (fund_with_bitcoind, mark_tx_mined)

Tests in `bwk/src/account.rs` use `electrsd` for regtest Electrum integration.
Tests in `bwk-sp/tests/` use `blindbitd` for regtest Blindbit integration.

## External Dependencies

- `spdk-core`, `spdk-wallet`, `backend-blindbit-v1`: optional comparison
  benchmark dependencies only (`bwk-sp/bench-spdk`)
- `miniscript`: Descriptor parsing and script generation
- `bitcoin`: Bitcoin primitives (v0.32)
