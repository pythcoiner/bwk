# Code Guidelines

## Documentation

Update docs (CLAUDE.md, README, doc comments) when making changes.

## Bitcoin Specifics

Never invent Bitcoin protocol details. When uncertain about derivation paths,
script formats, BIP specifications, or cryptographic operations — ask or
read the relevant BIP. Getting Bitcoin wrong breaks real money.

## Markdown

- Text (not tables or code) must wrap at 80-90 characters per line
- Tables must be ASCII art, no markdown table syntax

## Commits

Single-line commit messages only. Prefix with the crate name and a colon
(`bwk-sp: short description`). Use `workspace:` for workspace-level changes,
`doc:` for documentation-only changes.

## Comments

Don't state the obvious. No "returns the name" on `fn name() -> &str`.

## Helpers

Extract helpers for repeated logic. Avoid duplicating code across tests or
modules — shared helpers make maintenance easier and reduce rewrite effort.

## Dependencies

Keep dependencies minimal. Don't add new crates unless strictly necessary.

## No Async

Use threads + channels for concurrency, no async/await.

## Imports

Module-level imports. No imports inside functions unless truly necessary.

Use nested imports to group items from the same crate:
```rust
// Bad - separate use statements
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

// Good - nested imports
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
```

## Notification Channels

Account types expose a takeable receiver:
```rust
receiver: Option<mpsc::Receiver<Notification>>,

pub fn receiver(&mut self) -> Option<mpsc::Receiver<Notification>> {
    self.receiver.take()
}
```

## Mutex Panics

Use `.expect("poisoned")` - no recovery from poisoned locks:
```rust
self.store.lock().expect("poisoned")
```

## Tests

Use `unwrap()` in tests, not `expect("...")`. Tests already show the
failing line on panic — the extra message is noise.

## Section Separators

Forbidden — no `//====`, `//----`, or any decorative banner lines.
Use a simple `// Section name` comment if grouping is needed.

## Test Feature

The `test` feature exposes private APIs for integration testing without leaking them to consumers:
```rust
#[cfg(feature = "test")]
pub fn fund_with_bitcoind(&mut self, ...) { ... }
```
