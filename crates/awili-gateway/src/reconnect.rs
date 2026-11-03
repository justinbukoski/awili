use std::time::Duration;

/// Default base delay for the first reconnect attempt.
pub const RECONNECT_BASE: Duration = Duration::from_millis(500);

/// Hard cap on reconnect backoff (slice-0 §3.2 / porting-map §1.3).
pub const RECONNECT_CAP: Duration = Duration::from_secs(15);

/// Pure exponential-backoff schedule for reconnect attempts.
///
/// - attempt 0 -> base (saturated to `cap` if `base > cap`).
/// - attempt n -> base * 2^n, saturating at `cap` once it would meet or exceed `cap`.
/// - Monotonic non-decreasing in `attempt`; once `cap` is reached it stays at `cap` for all
///   larger attempts.
/// - Overflow-safe: a very large `attempt` (e.g. `u32::MAX`) returns `cap`, never panics,
///   never wraps.
/// - If `base == Duration::ZERO`, every attempt returns `ZERO`.
pub fn next_backoff(attempt: u32, base: Duration, cap: Duration) -> Duration {
    // base == ZERO is a fast-path: every attempt stays ZERO.
    if base == Duration::ZERO {
        return Duration::ZERO;
    }

    // Start at base; if base already exceeds cap, clamp immediately.
    let mut current = base.min(cap);

    // For attempt > 0, double `current` up to `attempt` times.
    // Each step saturates at `cap`, so we never overflow or exceed `cap`.
    let mut remaining = attempt;
    while remaining > 0 {
        // Double, but cap at `cap`.
        if current >= cap {
            // Already saturated — subsequent iterations stay at `cap`.
            break;
        }
        // Double, saturating at `cap`. `checked_mul` keeps this panic-free even for a
        // pathologically large `cap` near `Duration::MAX`, where `current * 2` would
        // overflow before the `.min(cap)` (Duration's `Mul` panics on overflow). On
        // overflow the doubled value is necessarily >= cap, so saturate to `cap`.
        current = current.checked_mul(2).map_or(cap, |d| d.min(cap));
        remaining -= 1;
    }

    current
}

/// Backoff using the crate default base/cap.
pub fn default_next_backoff(attempt: u32) -> Duration {
    next_backoff(attempt, RECONNECT_BASE, RECONNECT_CAP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempt_zero_returns_base() {
        assert_eq!(
            next_backoff(0, Duration::from_millis(500), RECONNECT_CAP),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn attempt_zero_base_above_cap_clamps_to_cap() {
        assert_eq!(
            next_backoff(0, Duration::from_secs(30), Duration::from_secs(15)),
            Duration::from_secs(15)
        );
    }

    #[test]
    fn doubling_sequence() {
        let base = Duration::from_millis(500);
        assert_eq!(
            next_backoff(0, base, RECONNECT_CAP),
            Duration::from_millis(500),
            "attempt 0 -> 500ms"
        );
        assert_eq!(
            next_backoff(1, base, RECONNECT_CAP),
            Duration::from_millis(1000),
            "attempt 1 -> 1s"
        );
        assert_eq!(
            next_backoff(2, base, RECONNECT_CAP),
            Duration::from_millis(2000),
            "attempt 2 -> 2s"
        );
        assert_eq!(
            next_backoff(3, base, RECONNECT_CAP),
            Duration::from_millis(4000),
            "attempt 3 -> 4s"
        );
    }

    #[test]
    fn saturation_at_cap() {
        let base = Duration::from_millis(500);
        // 500ms * 2^5 = 16s, which exceeds 15s cap → saturate at 15s.
        assert_eq!(
            next_backoff(5, base, RECONNECT_CAP),
            RECONNECT_CAP,
            "attempt 5 -> 15s (cap)"
        );
        // attempt 4 = 8s, still below cap.
        assert_eq!(
            next_backoff(4, base, RECONNECT_CAP),
            Duration::from_secs(8),
            "attempt 4 -> 8s (pre-cap)"
        );
    }

    #[test]
    fn stays_capped_for_large_attempts() {
        let base = Duration::from_millis(500);
        // Once saturated, every larger attempt stays at cap.
        assert_eq!(next_backoff(10, base, RECONNECT_CAP), RECONNECT_CAP);
        assert_eq!(next_backoff(100, base, RECONNECT_CAP), RECONNECT_CAP);
        assert_eq!(next_backoff(1000, base, RECONNECT_CAP), RECONNECT_CAP);
    }

    #[test]
    fn u32_max_does_not_panic() {
        // The largest possible attempt must return cap, not panic or wrap.
        assert_eq!(
            next_backoff(u32::MAX, Duration::from_millis(500), RECONNECT_CAP),
            RECONNECT_CAP
        );
    }

    #[test]
    fn huge_cap_does_not_panic_on_doubling_overflow() {
        // With a pathologically large cap, doubling `current` would overflow Duration
        // (whose `Mul` panics) before the `.min(cap)`. The schedule must saturate to cap
        // instead of panicking, honoring the documented overflow-safety contract.
        let base = Duration::from_secs(1);
        let cap = Duration::MAX;
        let result = next_backoff(u32::MAX, base, cap);
        assert_eq!(
            result, cap,
            "overflowing doubling must saturate to cap, not panic"
        );
        // A base already past the overflow point on the very next double also saturates.
        assert_eq!(
            next_backoff(1, Duration::MAX / 2 + Duration::from_nanos(1), cap),
            cap
        );
    }

    #[test]
    fn base_zero_always_zero() {
        let base = Duration::ZERO;
        let cap = Duration::from_secs(15);
        for attempt in 0..=5 {
            assert_eq!(
                next_backoff(attempt, base, cap),
                Duration::ZERO,
                "attempt {attempt} with ZERO base"
            );
        }
        // Also verify u32::MAX with zero base.
        assert_eq!(next_backoff(u32::MAX, base, cap), Duration::ZERO);
    }

    #[test]
    fn monotonic_non_decreasing() {
        let base = Duration::from_millis(500);
        let cap = Duration::from_secs(15);
        let mut prev = Duration::ZERO;
        for attempt in 0..=20 {
            let backoff = next_backoff(attempt, base, cap);
            assert!(
                backoff >= prev,
                "non-decreasing: attempt {attempt} ({backoff:?}) < prev ({prev:?})"
            );
            prev = backoff;
        }
    }

    #[test]
    fn default_wrapper_matches_with_consts() {
        for attempt in 0..=10 {
            assert_eq!(
                default_next_backoff(attempt),
                next_backoff(attempt, RECONNECT_BASE, RECONNECT_CAP),
                "default_next_backoff differs at attempt {attempt}",
            );
        }
    }

    #[test]
    fn cap_is_15_seconds() {
        // Sanity-check the constant value.
        assert_eq!(RECONNECT_CAP, Duration::from_secs(15));
    }

    #[test]
    fn base_is_500_milliseconds() {
        assert_eq!(RECONNECT_BASE, Duration::from_millis(500));
    }
}
