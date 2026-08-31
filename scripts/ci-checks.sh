#!/bin/bash

# The single definition of what "green" means for this repo. Every CI job and
# every local check runs a section of this script, so a change to the feature
# set or the flags applies everywhere at once.
#
# Usage: ci-checks.sh [fmt|clippy|tests|build]
# With no section, runs all four in order (the per-commit CI loop, see
# .github/workflows/test-each-commit.yml).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || (cd "$SCRIPT_DIR/.." && pwd))"
cd "$REPO_ROOT"

# test and sqlite are built explicitly: both gate large amounts of code that a
# default build never compiles, so breakage there goes unnoticed. sqlite gates
# its backend and that backend's tests; test gates the integration test files
# and the test-only constructors they reach for.
FEATURES="test,sqlite"

run_fmt() {
    echo "=== fmt check ==="
    cargo fmt --all -- --check
}

run_clippy() {
    echo "=== clippy ==="
    cargo clippy --all-targets --features "$FEATURES" -- -D warnings
}

run_tests() {
    echo "=== tests ==="
    # Run integration tests single-threaded: each spins up its own bitcoind/blindbit
    # (and electrsd) regtest daemon, and running dozens concurrently starves CPU/IO,
    # causing flaky sync timeouts and wallet-state races (e.g. spurious -6 "Insufficient
    # funds" after invalidateblock). One daemon set at a time trades runtime for reliability.
    cargo test --features "$FEATURES" --verbose --color always -- --nocapture --test-threads=1
}

run_build() {
    echo "=== build ==="
    cargo build --release --features "$FEATURES"
}

case "${1:-all}" in
    fmt) run_fmt ;;
    clippy) run_clippy ;;
    tests) run_tests ;;
    build) run_build ;;
    all)
        echo "=== Commit under test ==="
        git log -1 --oneline
        run_fmt
        run_clippy
        run_tests
        run_build
        ;;
    *)
        echo "usage: $(basename "$0") [fmt|clippy|tests|build]" >&2
        exit 1
        ;;
esac
