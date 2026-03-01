# bwk-utils

**Experimental — do not use in production or with real coins. API will break.**

Shared utilities and test helpers.

Provides common utilities used across bwk crates. Test helpers are behind the
`test` feature flag to avoid leaking test dependencies to consumers.

**Scope:** String formatting, test fixtures. Does NOT contain wallet logic.

## Usage

```rust
use bwk_utils::short_string;

// Truncate long strings with ellipsis
let s = short_string("abcdefghijklmnop".to_string(), 10);
assert_eq!(s, "abcd..mnop");
```

## Test Helpers (feature = "test")

```rust
use bwk_utils::test::{corepc_node, generate_blocks, get_tx};

// Create regtest node connection
let node = corepc_node();

// Generate blocks
generate_blocks(&node, 10);

// Fetch transaction
let tx = get_tx(&node, txid);
```

Requires `bitcoind` running on regtest for integration tests.
