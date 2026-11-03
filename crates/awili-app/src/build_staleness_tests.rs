//! P2f required proof (offline): the pure staleness rule for the local Build
//! poll loops — both the failure count AND the elapsed time must hold.

use std::time::{Duration, Instant};

use crate::build_staleness::{
    failing_line, PollStaleness, PollVerdict, STALE_AFTER, STALE_MIN_FAILURES,
};

fn mins(m: u64) -> Duration {
    Duration::from_secs(m * 60)
}

#[test]
fn success_resets_streak() {
    let t0 = Instant::now();
    let mut s = PollStaleness::with_limits(mins(10), 3);
    s.observe(Err("down"), t0);
    s.observe(Err("down"), t0 + mins(5));
    assert_eq!(s.observe(Ok(()), t0 + mins(6)), PollVerdict::Healthy);
    // The streak restarted: two more failures well past the window are
    // failures 1 and 2 of a NEW streak that began at minute 7.
    assert!(matches!(
        s.observe(Err("down"), t0 + mins(7)),
        PollVerdict::Failing { streak: 1, .. }
    ));
    assert!(matches!(
        s.observe(Err("down"), t0 + mins(30)),
        PollVerdict::Failing { streak: 2, .. }
    ));
}

#[test]
fn stale_needs_both_time_and_count() {
    assert_eq!((STALE_AFTER, STALE_MIN_FAILURES), (mins(10), 3));
    let t0 = Instant::now();
    // 2 failures over 20 min: the count is missing.
    let mut s = PollStaleness::default();
    s.observe(Err("e"), t0);
    assert!(matches!(
        s.observe(Err("e"), t0 + mins(20)),
        PollVerdict::Failing { streak: 2, .. }
    ));
    // 50 failures inside 1 min: the time is missing.
    let mut s = PollStaleness::default();
    for i in 0..50 {
        let v = s.observe(Err("e"), t0 + Duration::from_millis(i * 1000));
        assert!(matches!(v, PollVerdict::Failing { .. }), "{i}: {v:?}");
    }
    // 3 failures spanning 10 min: stale.
    let mut s = PollStaleness::default();
    s.observe(Err("e"), t0);
    s.observe(Err("e"), t0 + mins(5));
    assert!(matches!(
        s.observe(Err("e"), t0 + mins(10)),
        PollVerdict::Stale(_)
    ));
}

#[test]
fn stale_reason_is_exact() {
    let t0 = Instant::now();
    let mut s = PollStaleness::with_limits(Duration::from_secs(1), 3);
    s.observe(Err("first"), t0);
    s.observe(Err("second"), t0 + Duration::from_secs(60));
    let v = s.observe(Err("ssh: connect timed out"), t0 + Duration::from_secs(125));
    assert_eq!(
        v,
        PollVerdict::Stale(
            "stale: build host unreachable for 2m after 3 failed status polls \
             (last error: ssh: connect timed out)"
                .to_string()
        )
    );
}

#[test]
fn failing_reports_remaining() {
    let t0 = Instant::now();
    let mut s = PollStaleness::with_limits(mins(10), 3);
    s.observe(Err("e"), t0);
    let v = s.observe(Err("e"), t0 + mins(4));
    assert_eq!(
        v,
        PollVerdict::Failing {
            streak: 2,
            remaining: mins(6)
        }
    );
    assert_eq!(
        failing_line(2, mins(6) - Duration::from_secs(1), "host down"),
        "build status unreachable (2 tries) — marking stale in 6m: host down"
    );
    // Count reached, time not: still failing, with the time left.
    let v = s.observe(Err("e"), t0 + mins(9));
    assert_eq!(
        v,
        PollVerdict::Failing {
            streak: 3,
            remaining: mins(1)
        }
    );
}
