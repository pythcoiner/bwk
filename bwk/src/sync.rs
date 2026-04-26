//! EMA-based sync time estimator.
//!
//! Tracks seconds-per-block using an exponential moving average and estimates
//! remaining sync time from the current progress notifications.

use std::time::Instant;

const DEFAULT_ALPHA: f64 = 0.01;
const DEFAULT_WARMUP: u32 = 100;

/// Estimates remaining sync time using EMA of seconds-per-block.
#[derive(Debug, Clone)]
pub struct SyncEstimator {
    alpha: f64,
    warmup: u32,
    last_height: u32,
    remaining_blocks: u32,
    last_time: Option<Instant>,
    ema_secs_per_block: f64,
    samples: u32,
}

impl SyncEstimator {
    /// Create a new estimator with the given EMA smoothing factor and warmup count.
    pub fn new(alpha: f64, warmup: u32) -> Self {
        SyncEstimator {
            alpha,
            warmup,
            last_height: 0,
            remaining_blocks: 0,
            last_time: None,
            ema_secs_per_block: 0.0,
            samples: 0,
        }
    }

    /// Create a new estimator with default alpha (0.01) and warmup (100).
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_ALPHA, DEFAULT_WARMUP)
    }

    /// Update state from a progress sample: the current scanned height and the chain tip.
    pub fn update(&mut self, current: u32, end: u32) {
        let now = Instant::now();

        if let Some(last_time) = self.last_time {
            let blocks_done = current.saturating_sub(self.last_height);
            if blocks_done == 0 {
                return;
            }

            let elapsed = now.duration_since(last_time).as_secs_f64();

            // Skip if elapsed is unreasonably large (pause/restart): update anchors but do
            // not record a sample.
            if elapsed > 60.0 {
                self.last_height = current;
                self.last_time = Some(now);
                return;
            }

            let rate = elapsed / blocks_done as f64;

            if self.samples > 0 {
                self.ema_secs_per_block =
                    self.alpha * rate + (1.0 - self.alpha) * self.ema_secs_per_block;
            } else {
                self.ema_secs_per_block = rate;
            }
            self.samples += 1;
        }

        self.last_height = current;
        self.remaining_blocks = end.saturating_sub(current);
        self.last_time = Some(now);
    }

    /// Get the estimated remaining seconds (0 if not enough samples yet).
    pub fn estimate_secs(&self) -> u64 {
        if self.samples < self.warmup {
            return 0;
        }
        (self.remaining_blocks as f64 * self.ema_secs_per_block) as u64
    }

    /// Reset all state (call on scan start/stop).
    pub fn reset(&mut self) {
        self.last_height = 0;
        self.remaining_blocks = 0;
        self.last_time = None;
        self.ema_secs_per_block = 0.0;
        self.samples = 0;
    }
}

impl Default for SyncEstimator {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_no_estimate_during_warmup() {
        let mut est = SyncEstimator::new(0.1, 3);
        est.update(100, 1000);
        assert_eq!(est.estimate_secs(), 0, "No estimate after first update");

        thread::sleep(Duration::from_millis(50));
        est.update(200, 1000);
        assert_eq!(est.estimate_secs(), 0, "No estimate with only 1 sample");

        thread::sleep(Duration::from_millis(50));
        est.update(300, 1000);
        assert_eq!(est.estimate_secs(), 0, "No estimate with only 2 samples");
    }

    #[test]
    fn test_estimate_after_warmup() {
        let mut est = SyncEstimator::new(0.1, 3);
        est.update(100, 1000);
        for i in 1..=3 {
            thread::sleep(Duration::from_millis(50));
            est.update(100 + i * 100, 1000);
        }
        assert_eq!(est.samples, 3);
        let eta = est.estimate_secs();
        assert!(eta < 10, "ETA should be small for fast progress, got {eta}");
    }

    #[test]
    fn test_reset_clears_state() {
        let mut est = SyncEstimator::new(0.1, 3);
        est.update(100, 1000);
        thread::sleep(Duration::from_millis(50));
        est.update(200, 1000);
        assert!(est.samples > 0);
        est.reset();
        assert_eq!(est.estimate_secs(), 0);
        assert_eq!(est.samples, 0);
    }

    #[test]
    fn test_zero_blocks_done_skipped() {
        let mut est = SyncEstimator::new(0.1, 3);
        est.update(100, 1000);
        thread::sleep(Duration::from_millis(50));
        // Same height — should not count as a sample.
        est.update(100, 1000);
        assert_eq!(est.samples, 0, "No sample when blocks_done == 0");
    }

    #[test]
    fn test_estimate_within_tolerance_after_warmup() {
        // Steady cadence: 100 blocks per update, 50ms between updates → 0.5ms/block.
        let warmup = 5;
        let mut est = SyncEstimator::new(0.5, warmup);
        let mut height = 0u32;
        let end = 100_000u32;
        est.update(height, end);
        for _ in 0..warmup {
            thread::sleep(Duration::from_millis(50));
            height += 100;
            est.update(height, end);
        }
        let remaining = (end - height) as f64;
        let expected = (remaining * 0.0005) as u64; // 0.5ms per block
        let actual = est.estimate_secs();
        let lower = (expected as f64 * 0.8) as u64;
        let upper = (expected as f64 * 1.2).ceil() as u64;
        assert!(
            actual >= lower && actual <= upper,
            "estimate {actual}s outside ±20% of {expected}s ({lower}..={upper})",
        );
    }
}
