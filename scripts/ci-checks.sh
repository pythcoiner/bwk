#!/bin/bash

# Run all CI checks for a single commit: format, clippy, tests, and release
# build. Invoked from the per-commit CI loop (see
# .github/workflows/test-each-commit.yml) and runnable locally to reproduce
# what CI does.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || (cd "$SCRIPT_DIR/.." && pwd))"
cd "$REPO_ROOT"

echo "=== Commit under test ==="
git log -1 --oneline

echo "=== fmt check ==="
cargo fmt -- --check

echo "=== clippy ==="
cargo clippy --all-targets -- -D warnings

echo "=== tests ==="
# Run integration tests single-threaded: each spins up its own bitcoind/blindbit
# (and electrsd) regtest daemon, and running dozens concurrently starves CPU/IO,
# causing flaky sync timeouts and wallet-state races (e.g. spurious -6 "Insufficient
# funds" after invalidateblock). One daemon set at a time trades runtime for reliability.
cargo test --features test --verbose --color always -- --nocapture --test-threads=1

echo "=== build ==="
cargo build --release
