# bwk-backoff

**Experimental — do not use in production or with real coins. API will break.**

Exponential backoff utility for polling loops.

Starts with thread yields, then transitions to exponential sleep with jitter.
Useful for background threads that poll for work without busy-waiting.

**Scope:** Backoff timing only. Does NOT handle retries, circuit breaking, or
error classification.

## Usage

```rust
use bwk_backoff::Backoff;

let mut backoff = Backoff::new_ms(100);  // max 100ms sleep

loop {
    match try_work() {
        Some(work) => {
            process(work);
            backoff.reset();  // reset on success
        }
        None => {
            backoff.snooze();  // yield or sleep
        }
    }
}
```

## Behavior

- Steps 0-9: `yield_now()` (no sleep)
- Steps 10+: exponential sleep starting at 10μs, doubling each step
- Jitter: random 0-50% added to prevent thundering herd
- Capped at configured max sleep duration
