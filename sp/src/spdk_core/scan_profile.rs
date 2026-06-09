//! Lightweight always-on phase timers for the scan hot path.
//!
//! Each counter accumulates wall-clock nanoseconds spent in one phase of
//! `process_block`, summed across all worker threads. The timing is coarse (one
//! `Instant` pair per phase per block, not per tweak), so the overhead is
//! negligible relative to the work measured. A benchmark/diagnostic reads the
//! snapshot to attribute `process_secs` across phases; reset before a run.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

/// Per-tweak candidate spk derivation: vartime ECDH plus the `k = 0` candidate
/// output keys (unlabeled and labelled), in one native call per tweak (parallel).
pub static CANDIDATES_NS: AtomicU64 = AtomicU64::new(0);
/// Building the output GCS filter and testing candidate spks against it.
pub static OUTPUT_FILTER_NS: AtomicU64 = AtomicU64::new(0);
/// Fetching + scanning a block's UTXOs after a filter match (rare).
pub static SCAN_UTXOS_NS: AtomicU64 = AtomicU64::new(0);
/// Whole input side: input-hash derivation + spent GCS filter test.
pub static INPUT_NS: AtomicU64 = AtomicU64::new(0);

/// Add an elapsed duration to a phase counter.
#[inline]
pub fn add(counter: &AtomicU64, elapsed: Duration) {
    counter.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
}

/// Reset every counter to zero (call before a measured run).
pub fn reset() {
    for c in [&CANDIDATES_NS, &OUTPUT_FILTER_NS, &SCAN_UTXOS_NS, &INPUT_NS] {
        c.store(0, Ordering::Relaxed);
    }
}

/// `(label, seconds)` for each phase, in hot-path order.
pub fn snapshot_secs() -> [(&'static str, f64); 4] {
    let s = |c: &AtomicU64| c.load(Ordering::Relaxed) as f64 / 1e9;
    [
        ("candidates", s(&CANDIDATES_NS)),
        ("output_filter", s(&OUTPUT_FILTER_NS)),
        ("scan_utxos", s(&SCAN_UTXOS_NS)),
        ("input", s(&INPUT_NS)),
    ]
}
