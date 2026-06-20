#!/usr/bin/env bash
# Build + run the libsecp256k1 C test harness for the vendored silent-payments
# module (the funds-critical crypto gate for the light-client work).
#
# Why this is needed: the vendored tree (secp256k1-sys/depend/secp256k1) is
# symbol-renamed (rustsecp256k1_v0_10_0_) and WASM-patched: malloc/context/scratch
# creation are stripped from the C (the Rust crate reimplements them), so the
# standalone `tests` binary can't allocate a context and segfaults. This script
# reconstructs a pristine, TESTABLE copy without touching the vendored source:
#   1. copy the tree to target/secp-c-test (gitignored),
#   2. de-rename (exact inverse of vendor-libsecp.sh's rename),
#   3. reverse-apply the 4 WASM patches (restore checked_malloc + context_create
#      / context_clone / context_destroy + scratch_create / scratch_destroy and
#      their declarations),
#   4. CMake build with the SP module + tests/ctime/bench, then run them.
#
# Usage: scripts/secp-c-tests.sh [test_iterations]   (default 16)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/secp256k1-sys/depend/secp256k1"
PAT="$ROOT/secp256k1-sys/depend"
T="$ROOT/target/secp-c-test"
ITERS="${1:-16}"

rm -rf "$T"
cp -r "$SRC" "$T"
chmod -R +w "$T"
rm -rf "$T/build" "$T/cmbuild"   # drop any stale CMake build dir copied from the source

# de-rename: inverse of `s/secp256k1_/rustsecp256k1_v0_10_0_/g`.
# Run the find RELATIVE to the copy: the repo path itself contains ".claude", so
# an absolute `-path '*/.*'` prune would match (and skip) every file.
( cd "$T" && find . -not -path './.*' -type f -print0 \
  | xargs -0 sed -i 's/rustsecp256k1_v0_10_0_/secp256k1_/g' )

# Restore what the WASM/library build strips, so the standalone harness can build:
#   - checked_malloc real body (util.h)
#   - context_create/clone/destroy + scratch_space_create/destroy (secp256k1.c)
#   - their public declarations (secp256k1.h)
# NOTE: the static scratch_create/destroy in scratch_impl.h are NOT stripped in the
# vendored tree (they don't clash with the Rust #[no_mangle] reimpls), so we must
# NOT reverse scratch_impl.h.patch here or we get a redefinition.
patch -R "$T/include/secp256k1.h" < "$PAT/secp256k1.h.patch"
patch -R "$T/src/secp256k1.c"     < "$PAT/secp256k1.c.patch"
patch -R "$T/src/util.h"          < "$PAT/util.h.patch"

cmake -S "$T" -B "$T/build" \
  -DSECP256K1_ENABLE_MODULE_SILENTPAYMENTS=ON \
  -DSECP256K1_BUILD_TESTS=ON \
  -DSECP256K1_BUILD_CTIME_TESTS=ON \
  -DSECP256K1_BUILD_BENCHMARK=ON
cmake --build "$T/build" -j"$(nproc)"

"$T/build/bin/tests" "$ITERS"
# ctime_tests detects secret-dependent branches/memory accesses; it is only
# meaningful under valgrind (the harness is not built with msan), and it exits
# non-zero when run outside it.
if command -v valgrind >/dev/null 2>&1; then
  valgrind --error-exitcode=1 -q "$T/build/bin/ctime_tests"
else
  echo "secp-c-tests: WARNING valgrind not found, skipping ctime_tests" >&2
fi
echo "secp-c-tests: OK"
