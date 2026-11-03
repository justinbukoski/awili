//! The blocking HTTP transport that drives a [`Decoder`] and forwards events to
//! the async stream the UI drains.
//!
//! Deliberately a blocking `reqwest::blocking` request on a dedicated
//! `std::thread`: it reads the streaming body via `std::io::Read`, feeds chunks
//! to the decoder, and pushes each [`ProviderEvent`] into an `async_channel`.
//! That keeps providers off any tokio runtime we'd have to own — the GPUI side
//! polls the channel's receiver as an ordinary `Stream`. NO live network runs in
//! tests; only this thin wiring touches the socket, and it is exercised by the
//! Mac smoke pass, not `cargo test`.

use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;

use futures::stream::{BoxStream, StreamExt};

use crate::decode::Decoder;
use crate::event::ProviderEvent;

/// Bound on establishing the TCP/TLS connection. A host that never completes the
/// handshake fails here instead of blocking the worker thread forever.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Idle/read timeout for streaming bodies (H2). In `reqwest::blocking`,
/// `ClientBuilder::timeout` bounds each connect/read/write *operation* (it is NOT
/// a total request deadline), so it is reset by every chunk and SSE keepalive — a
/// long but healthy token stream is never killed. A reachable-but-stalled
/// endpoint trips this, and (crucially) it gives the [`pump`] loop a periodic
/// wake-up to notice the consumer was dropped so a `Stop` can retire the worker.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// A single process-wide blocking client with bounded connect + read timeouts,
/// reused across every provider call. Building a fresh `Client` per request (the
/// old behaviour) both wasted the connection pool and, worse, shipped with no
/// timeout at all. `reqwest::blocking::Client` is `Send + Sync` and internally
/// reference-counted, so one shared instance is correct and cheap.
pub(crate) fn shared_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(READ_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new())
    })
}

/// Spawn the request on a worker thread and return the event stream. `build` runs
/// on that thread (so the non-`Send` request machinery never crosses threads) and
/// yields either the streaming response or an error string. The returned stream
/// is guaranteed to end with [`ProviderEvent::Done`].
pub(crate) fn spawn_stream(
    build: impl FnOnce() -> Result<reqwest::blocking::Response, String> + Send + 'static,
    mut decoder: Box<dyn Decoder + Send>,
) -> BoxStream<'static, ProviderEvent> {
    let (tx, rx) = async_channel::unbounded::<ProviderEvent>();
    std::thread::spawn(move || match build() {
        Ok(resp) => pump(resp, decoder.as_mut(), &tx),
        Err(e) => {
            let _ = tx.send_blocking(ProviderEvent::Error(e));
            let _ = tx.send_blocking(ProviderEvent::Done);
        }
    });
    rx.boxed()
}

/// Read the body to EOF, feeding the decoder and forwarding events. Handles a
/// non-success status by surfacing the body as an error. Stops early (without
/// error) if the receiver was dropped — the user navigated away.
fn pump(
    resp: reqwest::blocking::Response,
    decoder: &mut dyn Decoder,
    tx: &async_channel::Sender<ProviderEvent>,
) {
    if !resp.status().is_success() {
        let status = resp.status();
        let body = read_error_body(resp);
        let _ = tx.send_blocking(ProviderEvent::Error(http_status_error(status, &body)));
        let _ = tx.send_blocking(ProviderEvent::Done);
        return;
    }

    pump_body(resp, decoder, tx);
}

/// Format a non-success HTTP response into the error string surfaced to the turn
/// loop. It MUST keep the numeric status code and the raw response body in the
/// string: automatic fallback (`crate::classify_error`) keys off the status code
/// (429 → RateLimit, 529 → Overloaded) or an in-body provider error type/prose
/// (`insufficient_quota`, "credit balance is too low"), so a capacity/quota
/// failure that reaches the turn loop as a *non-classifiable* string would strand
/// the turn instead of falling over to a local/fleet model. Extracted pure +
/// `pub(crate)` so that contract is unit-testable without constructing a live
/// `reqwest::blocking::Response` (which `pump`'s status branch otherwise needs) —
/// mirrors `build_anthropic_request` in `lib.rs`.
pub(crate) fn http_status_error(status: reqwest::StatusCode, body: &str) -> String {
    format!("HTTP {status}: {body}")
}

/// S7a: the most bytes of a non-success response body kept for the error.
/// 64 KiB: `classify_error` needs only the status and the provider's error
/// type or message, which sit in the first few hundred bytes of every
/// provider's error envelope. The rest of a larger body is never read.
pub(crate) const MAX_ERROR_BODY_BYTES: u64 = 64 * 1024;

/// Read at most [`MAX_ERROR_BODY_BYTES`] of an error body (S7a). A read
/// failure keeps whatever arrived before it, like the old `text()` default.
pub(crate) fn read_error_body<R: Read>(reader: R) -> String {
    let mut bytes = Vec::new();
    let _ = reader.take(MAX_ERROR_BODY_BYTES).read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The read loop, generic over any `Read` so the timeout/cancellation behaviour
/// is testable with an injected stalled reader (no live socket). Reads to EOF,
/// feeding the decoder and forwarding events.
///
/// A read timeout (the client's idle `READ_TIMEOUT`) is the cancellation seam:
/// without it the worker blocks on `read()` forever. On a timeout we check
/// whether the consumer is gone — if so the `Stop`/drop retired this worker and
/// we exit cleanly; otherwise the endpoint genuinely stalled and we surface a
/// timeout error. Either way a blocked worker now retires within `READ_TIMEOUT`.
fn pump_body<R: Read>(
    mut reader: R,
    decoder: &mut dyn Decoder,
    tx: &async_channel::Sender<ProviderEvent>,
) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                for ev in decoder.feed(&buf[..n]) {
                    if tx.send_blocking(ev).is_err() {
                        return; // consumer gone
                    }
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                if tx.is_closed() {
                    return; // Stop/drop retired this worker — clean shutdown
                }
                let _ =
                    tx.send_blocking(ProviderEvent::Error(format!("stream read timed out: {e}")));
                break;
            }
            Err(e) => {
                let _ = tx.send_blocking(ProviderEvent::Error(format!("read error: {e}")));
                break;
            }
        }
    }
    for ev in decoder.finish() {
        if tx.send_blocking(ev).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Read` that always reports a read timeout — stands in for a
    /// reachable-but-stalled endpoint without opening a socket.
    struct StalledReader;
    impl Read for StalledReader {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "simulated read timeout",
            ))
        }
    }

    /// A decoder that yields nothing — the timeout behaviour under test is in the
    /// read loop, not the decode step.
    struct NoopDecoder;
    impl Decoder for NoopDecoder {
        fn feed(&mut self, _chunk: &[u8]) -> Vec<ProviderEvent> {
            Vec::new()
        }
        fn finish(&mut self) -> Vec<ProviderEvent> {
            Vec::new()
        }
    }

    /// H2: a stalled stream whose consumer has dropped must retire the worker
    /// promptly (clean shutdown), not spin or block forever. Reaching the end of
    /// `pump_body` is the assertion.
    #[test]
    fn pump_retires_when_consumer_drops() {
        let (tx, rx) = async_channel::unbounded::<ProviderEvent>();
        drop(rx); // simulate Stop dropping the receiving side
        let mut decoder = NoopDecoder;
        pump_body(StalledReader, &mut decoder, &tx);
        assert!(tx.is_closed());
    }

    /// H2: a stalled stream with a LIVE consumer surfaces a timeout error
    /// (rather than hanging), so the stall is visible instead of silent.
    #[test]
    fn read_timeout_with_live_consumer_surfaces_error() {
        let (tx, rx) = async_channel::unbounded::<ProviderEvent>();
        let mut decoder = NoopDecoder;
        pump_body(StalledReader, &mut decoder, &tx);
        let ev = rx.try_recv().expect("a timeout error event was emitted");
        match ev {
            ProviderEvent::Error(msg) => assert!(
                msg.contains("timed out"),
                "expected a timeout error, got: {msg}"
            ),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// A `Read` that reports clean EOF on the first call — stands in for an
    /// upstream that closed the body without a read timeout.
    struct EofReader;
    impl Read for EofReader {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Ok(0)
        }
    }

    /// A `Read` that reports a non-timeout, non-would-block error (a reset
    /// connection) — must fall through the timeout guard into the generic arm.
    struct ResetReader;
    impl Read for ResetReader {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "simulated connection reset",
            ))
        }
    }

    /// A `Read` that hands back one non-empty chunk once, then EOF.
    struct OneChunkReader(bool);
    impl Read for OneChunkReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.0 {
                self.0 = false;
                buf[0] = b'x';
                Ok(1)
            } else {
                Ok(0)
            }
        }
    }

    /// Yields nothing on `feed`, one distinguishable event on `finish` — proves
    /// the post-loop flush ran after a clean EOF.
    struct FinishFlushDecoder;
    impl Decoder for FinishFlushDecoder {
        fn feed(&mut self, _chunk: &[u8]) -> Vec<ProviderEvent> {
            Vec::new()
        }
        fn finish(&mut self) -> Vec<ProviderEvent> {
            vec![ProviderEvent::Token("flushed".into())]
        }
    }

    /// Yields one event on `feed`; `finish` panics — so a test can prove the
    /// mid-feed consumer-gone break returned *before* `finish` was reached.
    struct FeedThenPanicOnFinishDecoder;
    impl Decoder for FeedThenPanicOnFinishDecoder {
        fn feed(&mut self, _chunk: &[u8]) -> Vec<ProviderEvent> {
            vec![ProviderEvent::Token("fed".into())]
        }
        fn finish(&mut self) -> Vec<ProviderEvent> {
            panic!(
                "finish must not run: the mid-feed send-error break should retire the worker first"
            );
        }
    }

    /// Clean EOF (`Ok(0)`) breaks the read loop, then the post-loop `finish()`
    /// flush must still run and deliver the decoder's trailing events — the path
    /// a normally-closing stream takes, distinct from the timeout arms above.
    #[test]
    fn eof_runs_finish_flush() {
        let (tx, rx) = async_channel::unbounded::<ProviderEvent>();
        let mut decoder = FinishFlushDecoder;
        pump_body(EofReader, &mut decoder, &tx);
        match rx.try_recv().expect("finish() flush event was delivered") {
            ProviderEvent::Token(s) => assert_eq!(s, "flushed"),
            other => panic!("expected the flushed Token, got {other:?}"),
        }
    }

    /// A non-timeout read error (connection reset) is NOT swallowed by the
    /// timeout guard: it surfaces through the generic error arm with the
    /// distinct "read error:" prefix, so a hard transport failure is visible
    /// rather than mislabelled as a stall.
    #[test]
    fn non_timeout_read_error_surfaces_generic_error() {
        let (tx, rx) = async_channel::unbounded::<ProviderEvent>();
        let mut decoder = NoopDecoder;
        pump_body(ResetReader, &mut decoder, &tx);
        match rx.try_recv().expect("a read-error event was emitted") {
            ProviderEvent::Error(msg) => {
                assert!(
                    msg.starts_with("read error:"),
                    "expected the generic read-error arm, got: {msg}"
                );
                assert!(
                    !msg.contains("timed out"),
                    "reset must not be mislabelled as a timeout: {msg}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// The `pump` → `classify_error` contract: a non-success HTTP response must
    /// reach the turn loop as a string the automatic-fallback classifier can key
    /// off, or a capacity/quota failure strands the turn instead of falling over
    /// to a local/fleet model. `pump`'s status branch needs a live `Response`, so
    /// this asserts the extracted `http_status_error` — the exact string `pump`
    /// emits — round-trips through `classify_error` for every class that gates a
    /// fallback. A regression that dropped the status code or body from the format
    /// (leaving fallback silently dead) fails here.
    #[test]
    fn http_status_error_round_trips_through_classify() {
        use crate::{classify_error, ProviderErrorKind};
        use reqwest::StatusCode;

        // 529 (Anthropic's non-standard capacity code, no canonical reason) →
        // Overloaded, purely off the numeric code even with an empty body.
        let e = http_status_error(StatusCode::from_u16(529).unwrap(), "");
        assert!(
            e.contains("529"),
            "status code must survive into the string: {e}"
        );
        assert_eq!(classify_error(&e), ProviderErrorKind::Overloaded);

        // 429 with a rate-limit body (no quota/credit words) → RateLimit, rotating
        // to the next model of any provider.
        let e = http_status_error(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#,
        );
        assert!(e.contains("429"));
        assert_eq!(classify_error(&e), ProviderErrorKind::RateLimit);

        // Quota outranks the capacity classes even at HTTP 429: an
        // `insufficient_quota` body must BAN the provider, not just rotate the
        // model — the body's "quota" wins over the 429 → RateLimit branch.
        let e = http_status_error(
            StatusCode::TOO_MANY_REQUESTS,
            "You exceeded your current quota, please check your plan and billing details.",
        );
        assert_eq!(classify_error(&e), ProviderErrorKind::Quota);

        // Anthropic's depleted-credit path is HTTP 400 (not a capacity status);
        // only the body prose classifies it as Quota so fallback still fires.
        let e = http_status_error(
            StatusCode::BAD_REQUEST,
            "Your credit balance is too low to access the Anthropic API.",
        );
        assert_eq!(classify_error(&e), ProviderErrorKind::Quota);
    }

    /// If the consumer drops mid-stream, a decoder event that fails to send must
    /// retire the worker immediately — returning from the feed loop *before*
    /// `finish()` is ever called (the panicking decoder proves finish never runs).
    #[test]
    fn feed_send_error_retires_before_finish() {
        let (tx, rx) = async_channel::unbounded::<ProviderEvent>();
        drop(rx); // consumer gone before the first chunk is fed
        let mut decoder = FeedThenPanicOnFinishDecoder;
        pump_body(OneChunkReader(true), &mut decoder, &tx);
        assert!(tx.is_closed());
    }

    /// S7a: a non-success body is read only up to `MAX_ERROR_BODY_BYTES`.
    #[test]
    fn s7a_error_body_is_capped() {
        let big = std::io::Cursor::new(vec![b'x'; 1024 * 1024]);
        let body = read_error_body(big);
        assert_eq!(body.len() as u64, MAX_ERROR_BODY_BYTES);
        let small = std::io::Cursor::new(b"{\"error\":{\"type\":\"overloaded_error\"}}".to_vec());
        assert_eq!(
            read_error_body(small),
            "{\"error\":{\"type\":\"overloaded_error\"}}"
        );
    }
}
