//! RC2-R617 (usability RC2 round 5, Major 1): one warm-up at a time per model
//! server, and a turn for the same server waits until the warm-up is done.
//!
//! A warm-up ([`awili_seam::warm_prompt_cache`]) has the server read what the
//! session's requests begin with before the owner's first question. Two
//! requests read at once on the Studio's mlx_lm are batched and both get
//! slower (measured 9.0 s for two against 6.6 s for one, RC2-R1F6), so a
//! turn that arrives while its server is still warming waits for it: what
//! the warm-up reads is exactly what the turn needs, and the turn then reads
//! only the question. A Work conversation's own warm-up runs in its worker
//! before the turn anyway; this gate is for a council member, whose turn has
//! a worker of its own.
//!
//! RC2-R6M2 (RC2-R6 review P2 on 0fc8434b: a warm-up was dropped whenever its
//! server was busy, even with another warm-up's text — a Work conversation's
//! rules while a council member's brief was being read — and the app had
//! already counted it warm): a second warm-up of a busy server waits its
//! turn instead, then reads its own text (the same text again is a cache hit,
//! cheap). A turn waits for the queued warm-ups of its server too.

use std::collections::HashMap;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

/// One server's warm-ups: the one reading now, with its progress (done,
/// total), and how many wait to begin.
#[derive(Default)]
struct Server {
    reading: Option<(u64, u64)>,
    queued: usize,
}

/// The servers with a warm-up under way or waiting.
struct Gate {
    servers: Mutex<HashMap<String, Server>>,
    changed: Condvar,
}

fn gate() -> &'static Gate {
    static GATE: OnceLock<Gate> = OnceLock::new();
    GATE.get_or_init(|| Gate {
        servers: Mutex::new(HashMap::new()),
        changed: Condvar::new(),
    })
}

fn servers() -> std::sync::MutexGuard<'static, HashMap<String, Server>> {
    gate().servers.lock().unwrap_or_else(|e| e.into_inner())
}

/// Forget `server` once nothing of it is under way or waiting.
fn tidy(w: &mut HashMap<String, Server>, server: &str) {
    if w.get(server)
        .is_some_and(|s| s.reading.is_none() && s.queued == 0)
    {
        w.remove(server);
    }
}

/// A warm-up of one server, under way; dropping it ends it and wakes every
/// turn and warm-up that waits for that server.
pub(crate) struct Warming {
    server: String,
}

impl Warming {
    /// The server's reading progress, for the turns that wait.
    pub(crate) fn progress(&self, done: u64, total: u64) {
        if let Some(s) = servers().get_mut(&self.server) {
            s.reading = Some((done, total));
        }
        gate().changed.notify_all();
    }
}

impl Drop for Warming {
    fn drop(&mut self) {
        let mut w = servers();
        if let Some(s) = w.get_mut(&self.server) {
            s.reading = None;
        }
        tidy(&mut w, &self.server);
        drop(w);
        gate().changed.notify_all();
    }
}

/// Begin a warm-up of `server`. When another warm-up of it is under way,
/// wait until it ends (each change of its progress is handed to
/// `on_progress`); `None` only when `stop` says so while waiting.
pub(crate) fn begin(
    server: &str,
    stop: impl Fn() -> bool,
    mut on_progress: impl FnMut(u64, u64),
) -> Option<Warming> {
    let mut w = servers();
    w.entry(server.to_string()).or_default().queued += 1;
    let mut last = None;
    loop {
        let reading = w.get(server).and_then(|s| s.reading);
        let Some(p) = reading else {
            break;
        };
        if stop() {
            if let Some(s) = w.get_mut(server) {
                s.queued -= 1;
            }
            tidy(&mut w, server);
            drop(w);
            gate().changed.notify_all();
            return None;
        }
        if last != Some(p) {
            last = Some(p);
            drop(w);
            on_progress(p.0, p.1);
            w = servers();
            continue;
        }
        // A Stop is noticed within this wait; progress and the end wake it.
        w = gate()
            .changed
            .wait_timeout(w, Duration::from_millis(200))
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
    let s = w.entry(server.to_string()).or_default();
    s.queued -= 1;
    s.reading = Some((0, 0));
    Some(Warming {
        server: server.to_string(),
    })
}

/// Wait until no warm-up of `server` is under way or waiting to begin, or
/// `stop` says so. Each change of the reading one's progress is handed to
/// `on_progress`. Returns at once when there is none.
pub(crate) fn wait_idle(
    server: &str,
    stop: impl Fn() -> bool,
    mut on_progress: impl FnMut(u64, u64),
) {
    let mut w = servers();
    let mut last = None;
    while let Some(s) = w.get(server) {
        if stop() {
            return;
        }
        if let Some(p) = s.reading {
            if last != Some(p) {
                last = Some(p);
                drop(w);
                on_progress(p.0, p.1);
                w = servers();
                continue;
            }
        }
        // A Stop is noticed within this wait; progress and the end wake it.
        w = gate()
            .changed
            .wait_timeout(w, Duration::from_millis(200))
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}
/// RC2-R6M2: how many warm-ups of `server` wait to begin (for tests).
#[cfg(test)]
pub(crate) fn queued(server: &str) -> usize {
    servers().get(server).map_or(0, |s| s.queued)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A second warm-up of the same server does not start while the first
    /// reads (RC2-R6M2: it waits its turn); another server's
    /// is. A turn waits for the warm-up of its server, sees its progress,
    /// and goes on once it ends; with none under way it does not wait.
    #[test]
    fn rc2r617_a_turn_waits_for_its_servers_warm_up() {
        let server = "http://r617-gate-test-a:1";
        let other = "http://r617-gate-test-b:1";
        let warm = begin(server, || false, |_, _| {}).expect("the first warm-up begins");
        assert!(
            begin(server, || true, |_, _| {}).is_none(),
            "one at a time: a second waits (here it is stopped while it waits)"
        );
        let b = begin(other, || true, |_, _| {}).expect("another server warms on its own");
        drop(b);

        let (seen_tx, seen_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            wait_idle(server, || false, |d, t| seen_tx.send((d, t)).unwrap());
            done_tx.send(()).unwrap();
        });
        assert_eq!(seen_rx.recv().unwrap(), (0, 0), "it sees the warm-up");
        warm.progress(2048, 4096);
        assert_eq!(seen_rx.recv().unwrap(), (2048, 4096), "and its progress");
        assert!(done_rx.try_recv().is_err(), "it still waits");
        drop(warm);
        done_rx.recv().unwrap();
        waiter.join().unwrap();
        assert!(
            begin(server, || true, |_, _| {}).is_some(),
            "a later warm-up may begin"
        );

        // Nothing warming: no wait, no progress.
        let mut called = false;
        wait_idle("http://r617-gate-test-c:1", || false, |_, _| called = true);
        assert!(!called);
        // A stopped turn stops waiting.
        let _held = begin("http://r617-gate-test-d:1", || false, |_, _| {}).unwrap();
        wait_idle("http://r617-gate-test-d:1", || true, |_, _| {});
    }

    /// RC2-R6M2 (RC2-R6 review P2 on 0fc8434b): a second warm-up of a busy
    /// server waits for the first (seeing its progress) and then begins; a
    /// turn waits for both, the queued one included; a warm-up stopped
    /// while it waits leaves nothing behind.
    #[test]
    fn rc2r6m2_a_second_warm_up_waits_its_turn() {
        let server = "http://r6m2-gate-test-a:1";
        let first = begin(server, || false, |_, _| {}).expect("the first begins");
        first.progress(10, 100);
        let (seen_tx, seen_rx) = std::sync::mpsc::channel();
        let (began_tx, began_rx) = std::sync::mpsc::channel();
        let second = std::thread::spawn(move || {
            let w = begin(server, || false, |d, t| seen_tx.send((d, t)).unwrap())
                .expect("the second begins once the first ends");
            began_tx.send(()).unwrap();
            w
        });
        assert_eq!(seen_rx.recv().unwrap(), (10, 100), "it waits on the first");
        assert!(began_rx.try_recv().is_err(), "not while the first reads");
        // A turn arriving now waits for the first and the queued second.
        let (idle_tx, idle_rx) = std::sync::mpsc::channel();
        let turn = std::thread::spawn(move || {
            wait_idle(server, || false, |_, _| {});
            idle_tx.send(()).unwrap();
        });
        drop(first);
        began_rx.recv().unwrap();
        let second = second.join().unwrap();
        assert!(
            idle_rx.try_recv().is_err(),
            "the turn waits for the second too"
        );
        drop(second);
        idle_rx.recv().unwrap();
        turn.join().unwrap();

        // Stopped while waiting: no warm-up, and the server is idle after.
        let held = begin(server, || false, |_, _| {}).unwrap();
        assert!(begin(server, || true, |_, _| {}).is_none());
        drop(held);
        let mut called = false;
        wait_idle(server, || false, |_, _| called = true);
        assert!(!called, "nothing left waiting");
    }
}
