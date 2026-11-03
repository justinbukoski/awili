//! Microphone capture (slice-3 spec §2). The real implementation uses
//! cpal/CoreAudio and is **macOS-only** — the one platform Awili records on; cpal
//! is a mac-gated dependency. On every other platform a stub with the same API
//! returns [`CaptureError::Unsupported`] from [`MicCapture::start`], so the app's
//! dictation wiring compiles and is type-checked on Linux (the slice DoD) and
//! simply reports "mic unavailable" at runtime there.
//!
//! Whisper wants 16 kHz mono `f32`; microphones deliver 44.1/48 kHz, often
//! multi-channel. The capture downmixes to mono and resamples to
//! [`crate::stt::SAMPLE_RATE`] with a *stateful* [`StreamResampler`] owned by the
//! audio callback ([`CallbackState`]), so variable-sized CoreAudio callbacks form
//! one continuous stream (review SF2). While capturing,
//! [`snapshot`](MicCapture::snapshot) / [`shared_buffer`](MicCapture::shared_buffer)
//! expose the audio so far for a streaming *partial* transcript;
//! [`stop`](MicCapture::stop) tears the stream down **before** the final drain, so
//! the tail of the utterance is never lost to a late callback (review SF1).
//!
//! The audio callback never waits and never allocates (review F0930, F0932 and
//! the RC2-VOICE batch review). Everything it writes to is allocated before the
//! stream starts: its scratch buffers, sized for one [`CHUNK_FRAMES`] step, and a
//! fixed-size ring of [`RING_CAPACITY`] samples ([`sample_pipe`]). A separate
//! drain thread ([`DrainThread`]) moves the ring's samples into the growing
//! [`SharedSamples`] buffer that snapshots and the conversation drain read; only
//! that thread ever takes the buffer's lock or grows it.
//!
//! Overflow policy: the ring fills only if the drain thread falls
//! [`RING_CAPACITY`] samples (ten seconds) behind, i.e. a reader holds the shared
//! buffer's lock that long. Then the callback drops the **newest** samples that do
//! not fit (the callback cannot discard the oldest without racing the drain
//! thread, and what is queued stays whole and in order), counts them, and the
//! drain thread logs the loss to stderr — never the audio thread.
//!
//! [`StreamResampler`]: crate::audio::StreamResampler

use std::io::Write as _;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::audio::{downmix_convert_into, StreamResampler};
use crate::stt::SAMPLE_RATE;

/// The captured-audio buffer shared between the drain thread and readers.
pub type SharedSamples = Arc<Mutex<Vec<f32>>>;

/// How many samples the callback's ring holds: ten seconds at 16 kHz (640 KB),
/// allocated once before the stream starts. The drain thread empties it every
/// [`DRAIN_INTERVAL`], so it fills only if a reader holds the shared buffer's
/// lock for ten seconds; past that the newest samples are dropped and counted.
pub const RING_CAPACITY: usize = SAMPLE_RATE as usize * 10;

/// How often the drain thread moves the ring's samples into the shared buffer.
pub const DRAIN_INTERVAL: Duration = Duration::from_millis(10);

/// Input frames the callback converts and resamples per step. A larger CoreAudio
/// callback is processed in steps of this size, so the callback's scratch
/// buffers never outgrow the capacity they were given before the stream started.
pub const CHUNK_FRAMES: usize = 4096;

/// Lock the shared buffer, recovering it if a panicking holder poisoned it: the
/// samples are plain `f32`s, valid whatever the panic interrupted.
fn lock_samples(shared: &SharedSamples) -> MutexGuard<'_, Vec<f32>> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The fixed-size single-producer, single-consumer ring between the audio
/// callback ([`SampleSink`]) and the drain thread ([`SampleDrain`]). Samples are
/// kept as `f32` bits in atomics, so the two sides share it without a lock.
/// `written` and `read` count every sample ever written and read; at 16 kHz a
/// 64-bit counter does not wrap in any real session.
struct Ring {
    slots: Box<[AtomicU32]>,
    /// Moved only by the callback, after the slots are written.
    written: AtomicUsize,
    /// Moved only by the drain, after the slots are read.
    read: AtomicUsize,
    /// Samples the callback dropped because the ring was full.
    dropped: AtomicU64,
}

impl Ring {
    fn queued(&self) -> usize {
        let read = self.read.load(Ordering::Acquire);
        self.written.load(Ordering::Acquire).wrapping_sub(read)
    }
}

/// Allocate the ring (at least one sample) and split it into the callback's
/// [`SampleSink`] and the drain thread's [`SampleDrain`], which feeds `shared`.
pub fn sample_pipe(shared: SharedSamples, capacity: usize) -> (SampleSink, SampleDrain) {
    let ring = Arc::new(Ring {
        slots: (0..capacity.max(1)).map(|_| AtomicU32::new(0)).collect(),
        written: AtomicUsize::new(0),
        read: AtomicUsize::new(0),
        dropped: AtomicU64::new(0),
    });
    let drain = SampleDrain {
        ring: ring.clone(),
        shared,
        reported_dropped: 0,
    };
    (SampleSink { ring }, drain)
}

/// The audio callback's side of the ring (review F0930, F0932).
///
/// [`push`](Self::push) runs on the real-time audio thread: it never takes a lock
/// and never allocates. Samples that do not fit are dropped (the newest ones) and
/// counted for the drain thread to report.
pub struct SampleSink {
    ring: Arc<Ring>,
}

impl SampleSink {
    /// Queue `samples` for the drain thread and return how many were queued; the
    /// rest did not fit and were dropped and counted.
    pub fn push(&mut self, samples: &[f32]) -> usize {
        let ring = &*self.ring;
        let capacity = ring.slots.len();
        let written = ring.written.load(Ordering::Relaxed);
        // Acquire: the slots the drain has released are free to overwrite.
        let free = capacity - written.wrapping_sub(ring.read.load(Ordering::Acquire));
        let queued = samples.len().min(free);
        for (i, &sample) in samples[..queued].iter().enumerate() {
            ring.slots[written.wrapping_add(i) % capacity]
                .store(sample.to_bits(), Ordering::Relaxed);
        }
        // Release: the drain sees these slots written before it sees the count.
        ring.written
            .store(written.wrapping_add(queued), Ordering::Release);
        let lost = samples.len() - queued;
        if lost > 0 {
            ring.dropped.fetch_add(lost as u64, Ordering::Relaxed);
        }
        queued
    }

    /// Samples waiting in the ring for the drain thread.
    pub fn queued(&self) -> usize {
        self.ring.queued()
    }

    /// The ring's fixed size, in samples.
    pub fn capacity(&self) -> usize {
        self.ring.slots.len()
    }
}

/// What one [`SampleDrain::drain`] pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DrainReport {
    /// Samples moved from the ring into the shared buffer.
    pub moved: usize,
    /// Samples the callback dropped (ring full) since the previous pass.
    pub newly_dropped: u64,
}

/// The drain thread's side of the ring: moves queued samples, in order, into the
/// growing shared buffer. It may wait for the buffer's lock and may allocate; it
/// never runs on the audio thread.
pub struct SampleDrain {
    ring: Arc<Ring>,
    shared: SharedSamples,
    reported_dropped: u64,
}

impl SampleDrain {
    /// Move everything queued in the ring into the shared buffer.
    pub fn drain(&mut self) -> DrainReport {
        let ring = &*self.ring;
        let capacity = ring.slots.len();
        let read = ring.read.load(Ordering::Relaxed);
        // Acquire: the callback's slot writes are visible before its count.
        let ready = ring.written.load(Ordering::Acquire).wrapping_sub(read);
        if ready > 0 {
            lock_samples(&self.shared).extend((0..ready).map(|i| {
                f32::from_bits(ring.slots[read.wrapping_add(i) % capacity].load(Ordering::Relaxed))
            }));
            // Release: the slots are read before the callback may reuse them.
            ring.read.store(read.wrapping_add(ready), Ordering::Release);
        }
        let dropped = ring.dropped.load(Ordering::Relaxed);
        let newly_dropped = dropped - self.reported_dropped;
        self.reported_dropped = dropped;
        DrainReport {
            moved: ready,
            newly_dropped,
        }
    }

    /// Samples the callback has dropped since capture began.
    pub fn dropped(&self) -> u64 {
        self.ring.dropped.load(Ordering::Relaxed)
    }
}

/// The plain log line for audio lost to a full ring.
pub fn dropped_audio_message(samples: u64) -> String {
    format!(
        "awili: the microphone recording fell behind; {:.2} seconds of audio ({samples} samples) were lost",
        samples as f64 / f64::from(SAMPLE_RATE)
    )
}

/// One drain pass that also logs any newly dropped audio. Runs on the drain
/// thread or the thread stopping the capture, never the audio thread.
fn drain_and_log(drain: &mut SampleDrain) -> DrainReport {
    let report = drain.drain();
    if report.newly_dropped > 0 {
        // A closed stderr must not end the capture: the log line is best-effort.
        let _ = writeln!(
            std::io::stderr(),
            "{}",
            dropped_audio_message(report.newly_dropped)
        );
    }
    report
}

/// The thread that empties the ring into the shared buffer every `interval`
/// while the capture runs. Dropping it (or [`finish`](Self::finish)) ends it.
pub struct DrainThread {
    stop: mpsc::Sender<()>,
    handle: JoinHandle<SampleDrain>,
}

impl DrainThread {
    pub fn spawn(mut drain: SampleDrain, interval: Duration) -> std::io::Result<Self> {
        let (stop, stop_rx) = mpsc::channel::<()>();
        let handle = std::thread::Builder::new()
            .name("awili-mic-drain".to_string())
            .spawn(move || {
                // A stop message or a dropped sender ends the loop.
                while let Err(RecvTimeoutError::Timeout) = stop_rx.recv_timeout(interval) {
                    drain_and_log(&mut drain);
                }
                drain
            })?;
        Ok(Self { stop, handle })
    }

    /// Stop the thread and hand back its drain for the final pass (`None` only if
    /// the thread panicked).
    pub fn finish(self) -> Option<SampleDrain> {
        let _ = self.stop.send(());
        self.handle.join().ok()
    }
}

/// What [`stop_capture`] needs from a live input stream. Dropping the stream must
/// end its callback for good — and drop the callback's [`SampleSink`] — as cpal's
/// CoreAudio stream does.
pub trait CaptureStream {
    fn pause(&self);
}

/// Tear a capture down and return the full utterance (16 kHz mono `f32`).
///
/// Ordering matters (review SF1): the stream is paused and **dropped** first. A
/// cpal CoreAudio stream drop disposes the AudioUnit synchronously, so once the
/// drop returns no callback can run and whatever the last callback pushed is in
/// the ring. Only then is the drain thread stopped and the ring drained one last
/// time, and only then is the buffer taken, so the final frames of the utterance
/// are always included.
pub fn stop_capture<S: CaptureStream>(
    stream: S,
    drain: DrainThread,
    samples: &SharedSamples,
) -> Vec<f32> {
    stream.pause();
    drop(stream);
    if let Some(mut drain) = drain.finish() {
        drain_and_log(&mut drain);
    }
    let utterance = std::mem::take(&mut *lock_samples(samples));
    utterance
}

/// Per-stream state owned by the audio callback: the cross-callback resampler
/// phase, scratch buffers sized before the stream starts, and the ring's
/// [`SampleSink`]. [`ingest`](Self::ingest) does no allocation (review SF2 and
/// the RC2-VOICE batch review).
pub struct CallbackState {
    resampler: StreamResampler,
    channels: usize,
    /// Input frames per step: [`CHUNK_FRAMES`], fewer for a rate below 16 kHz so
    /// the resampled step stays near [`CHUNK_FRAMES`] samples.
    step_frames: usize,
    /// Downmixed (and sample-format-converted) mono scratch for one step.
    mono: Vec<f32>,
    /// Resampled 16 kHz scratch for one step.
    out: Vec<f32>,
    sink: SampleSink,
}

impl CallbackState {
    pub fn new(channels: usize, in_rate: u32, sink: SampleSink) -> Self {
        let in_rate = in_rate.max(1);
        let step_frames = if in_rate >= SAMPLE_RATE {
            CHUNK_FRAMES
        } else {
            (CHUNK_FRAMES * in_rate as usize / SAMPLE_RATE as usize).max(1)
        };
        // A step of n input frames yields at most ceil(n * 16000 / in_rate) + 1
        // output samples (the resampler's output points that fall in it); one
        // more absorbs floating-point rounding.
        let out_capacity =
            (step_frames as u64 * u64::from(SAMPLE_RATE)).div_ceil(u64::from(in_rate)) as usize + 2;
        Self {
            resampler: StreamResampler::new(in_rate, SAMPLE_RATE),
            channels,
            step_frames,
            mono: Vec::with_capacity(step_frames),
            out: Vec::with_capacity(out_capacity),
            sink,
        }
    }

    /// One callback's work, step by step: convert+downmix into `mono`, resample
    /// into `out`, then queue it in the ring.
    pub fn ingest<T: Copy>(&mut self, data: &[T], conv: impl Fn(T) -> f32) {
        if self.channels == 0 {
            return;
        }
        for step in data.chunks(self.step_frames * self.channels) {
            self.mono.clear();
            downmix_convert_into(step, self.channels, &conv, &mut self.mono);
            self.out.clear();
            self.resampler.process(&self.mono, &mut self.out);
            self.sink.push(&self.out);
        }
    }

    /// The scratch buffers' capacities (mono, resampled): fixed for the stream.
    pub fn scratch_capacity(&self) -> (usize, usize) {
        (self.mono.capacity(), self.out.capacity())
    }

    /// The ring's side of this callback.
    pub fn sink(&self) -> &SampleSink {
        &self.sink
    }
}

/// Why capture could not start.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("microphone capture is only available on macOS")]
    Unsupported,
    #[error("no default input device (grant microphone permission?)")]
    NoDevice,
    #[error("querying input config: {0}")]
    Config(String),
    #[error("building input stream: {0}")]
    Stream(String),
    #[error("starting input stream: {0}")]
    Play(String),
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    /// An in-progress microphone capture. Holds the live cpal stream (kept alive
    /// for the capture's duration), the drain thread, and the shared, resampled
    /// 16 kHz mono buffer the drain thread fills.
    pub struct MicCapture {
        // The stream must stay alive to keep recording; `stop` tears it down
        // explicitly before the final drain.
        stream: cpal::Stream,
        drain: DrainThread,
        samples: SharedSamples,
    }

    impl CaptureStream for cpal::Stream {
        fn pause(&self) {
            let _ = StreamTrait::pause(self);
        }
    }

    impl MicCapture {
        /// Open the default input device and begin capturing into an internal
        /// buffer.
        pub fn start() -> Result<Self, CaptureError> {
            let host = cpal::default_host();
            let device = host.default_input_device().ok_or(CaptureError::NoDevice)?;
            let config = device
                .default_input_config()
                .map_err(|e| CaptureError::Config(e.to_string()))?;
            let in_rate = config.sample_rate().0;
            let channels = config.channels() as usize;

            let samples: SharedSamples = Arc::new(Mutex::new(Vec::new()));
            // Everything the callback writes to is allocated here, before the
            // stream exists: the ring now, the scratch buffers in
            // `CallbackState::new`.
            let (sink, drain) = sample_pipe(samples.clone(), RING_CAPACITY);
            let err_fn = |e| eprintln!("awili: audio input stream error: {e}");

            let stream = match config.sample_format() {
                cpal::SampleFormat::F32 => {
                    let mut st = CallbackState::new(channels, in_rate, sink);
                    device.build_input_stream(
                        &config.into(),
                        move |data: &[f32], _| st.ingest(data, |s| s),
                        err_fn,
                        None,
                    )
                }
                cpal::SampleFormat::I16 => {
                    let mut st = CallbackState::new(channels, in_rate, sink);
                    device.build_input_stream(
                        &config.into(),
                        move |data: &[i16], _| st.ingest(data, |s| s as f32 / i16::MAX as f32),
                        err_fn,
                        None,
                    )
                }
                cpal::SampleFormat::U16 => {
                    let mut st = CallbackState::new(channels, in_rate, sink);
                    device.build_input_stream(
                        &config.into(),
                        move |data: &[u16], _| {
                            st.ingest(data, |s| (s as f32 / u16::MAX as f32) * 2.0 - 1.0)
                        },
                        err_fn,
                        None,
                    )
                }
                other => {
                    return Err(CaptureError::Stream(format!(
                        "unsupported sample format {other:?}"
                    )))
                }
            }
            .map_err(|e| CaptureError::Stream(e.to_string()))?;

            let drain = DrainThread::spawn(drain, DRAIN_INTERVAL)
                .map_err(|e| CaptureError::Stream(format!("starting the sample drain: {e}")))?;
            stream
                .play()
                .map_err(|e| CaptureError::Play(e.to_string()))?;
            Ok(Self {
                stream,
                drain,
                samples,
            })
        }

        /// A copy of the audio captured so far — fed to whisper for a partial
        /// transcript while the user is still dictating.
        pub fn snapshot(&self) -> Vec<f32> {
            lock_samples(&self.samples).clone()
        }

        /// The shared buffer handle, so a worker thread can poll snapshots for
        /// partial transcripts without holding the (non-`Send`) cpal stream.
        pub fn shared_buffer(&self) -> SharedSamples {
            self.samples.clone()
        }

        /// Stop capturing and return the full utterance (16 kHz mono `f32`):
        /// the stream is dropped before the final drain ([`stop_capture`], review
        /// SF1), so the final frames of the utterance are always included.
        pub fn stop(self) -> Vec<f32> {
            let Self {
                stream,
                drain,
                samples,
            } = self;
            stop_capture(stream, drain, &samples)
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    /// Non-macOS stub: the same API, but [`start`](MicCapture::start) always fails
    /// with [`CaptureError::Unsupported`]. Lets the app's dictation code compile and
    /// type-check on Linux without pulling in cpal.
    pub struct MicCapture {
        samples: SharedSamples,
    }

    impl MicCapture {
        pub fn start() -> Result<Self, CaptureError> {
            Err(CaptureError::Unsupported)
        }
        pub fn snapshot(&self) -> Vec<f32> {
            lock_samples(&self.samples).clone()
        }
        pub fn shared_buffer(&self) -> SharedSamples {
            self.samples.clone()
        }
        pub fn stop(self) -> Vec<f32> {
            // Mirrors the macOS ordering: there is no stream here, so draining is
            // trivially safe.
            std::mem::take(&mut *lock_samples(&self.samples))
        }
    }
}

pub use platform::MicCapture;

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    /// Counts heap allocations per thread, so a test can prove the audio
    /// callback's path allocates nothing (the RC2-VOICE batch review, P2).
    struct CountingAlloc;

    thread_local! {
        static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    }

    fn count_allocation() {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
    }

    // SAFETY: every call is forwarded unchanged to the system allocator; the
    // counter is a const-initialised thread-local `Cell`, which never allocates.
    unsafe impl GlobalAlloc for CountingAlloc {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            count_allocation();
            // SAFETY: the caller's contract is passed straight through.
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            count_allocation();
            // SAFETY: the caller's contract is passed straight through.
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            count_allocation();
            // SAFETY: the caller's contract is passed straight through.
            unsafe { System.realloc(ptr, layout, new_size) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: the caller's contract is passed straight through.
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static COUNTING_ALLOC: CountingAlloc = CountingAlloc;

    /// Heap allocations (and reallocations) made on this thread while `f` ran.
    fn allocations_during(f: impl FnOnce()) -> usize {
        let before = ALLOCATIONS.with(Cell::get);
        f();
        ALLOCATIONS.with(Cell::get) - before
    }

    fn shared() -> SharedSamples {
        Arc::new(Mutex::new(Vec::new()))
    }

    /// F0930 / F0932: a reader holding the buffer lock (a long snapshot clone,
    /// the conversation drain) must never make the audio callback wait.
    #[test]
    fn rc2_voice_callback_never_waits_on_a_held_buffer_lock() {
        let buffer = shared();
        let held = lock_samples(&buffer);
        let (sink, mut drain) = sample_pipe(buffer.clone(), 16);
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut sink = sink;
            sink.push(&[0.1, 0.2]);
            done_tx.send(sink.queued()).unwrap();
            sink
        });
        let queued = done_rx.recv_timeout(Duration::from_secs(2));
        drop(held);
        let sink = worker.join().unwrap();
        assert_eq!(
            queued,
            Ok(2),
            "the push must return at once and keep its samples in the ring"
        );
        drop(sink);
        drain.drain();
        assert_eq!(*lock_samples(&buffer), vec![0.1, 0.2]);
    }

    /// Hold the buffer lock on another thread, as a reader would, until the
    /// returned sender fires (or two seconds pass, so a regression to a
    /// blocking push fails its assertions instead of hanging the test run).
    fn hold_lock_elsewhere(
        buffer: &SharedSamples,
    ) -> (mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let holder = buffer.clone();
        let handle = std::thread::spawn(move || {
            let _guard = lock_samples(&holder);
            ready_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(2));
        });
        ready_rx.recv().unwrap();
        (release_tx, handle)
    }

    /// Samples queued while a reader held the lock reach the buffer in order,
    /// including across the ring's wrap-around.
    #[test]
    fn rc2_voice_deferred_samples_keep_their_order() {
        let buffer = shared();
        let (mut sink, mut drain) = sample_pipe(buffer.clone(), 4);
        sink.push(&[1.0]);
        drain.drain();
        let (release, holder) = hold_lock_elsewhere(&buffer);
        sink.push(&[2.0, 3.0]);
        assert_eq!(sink.queued(), 2);
        release.send(()).unwrap();
        holder.join().unwrap();
        sink.push(&[4.0]);
        drain.drain();
        assert_eq!(sink.queued(), 0);
        sink.push(&[5.0, 6.0, 7.0]);
        drain.drain();
        assert_eq!(
            *lock_samples(&buffer),
            vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]
        );
    }

    /// Samples still in the ring when the callback's sink is dropped (with the
    /// stream, in `stop`) are not lost: the drain after the drop moves them.
    #[test]
    fn rc2_voice_dropping_the_sink_hands_over_the_tail() {
        let buffer = shared();
        let (mut sink, mut drain) = sample_pipe(buffer.clone(), 16);
        let (release, holder) = hold_lock_elsewhere(&buffer);
        sink.push(&[5.0, 6.0]);
        assert_eq!(sink.queued(), 2);
        drop(sink);
        release.send(()).unwrap();
        holder.join().unwrap();
        assert_eq!(drain.drain().moved, 2);
        assert_eq!(*lock_samples(&buffer), vec![5.0, 6.0]);
    }

    /// A reader that panicked while holding the lock must not stop capture.
    #[test]
    fn rc2_voice_poisoned_buffer_keeps_recording() {
        let buffer = shared();
        let poisoner = buffer.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("reader panicked while holding the buffer");
        })
        .join();
        assert!(buffer.is_poisoned());
        let (mut sink, mut drain) = sample_pipe(buffer.clone(), 16);
        sink.push(&[7.0]);
        drain.drain();
        assert_eq!(*lock_samples(&buffer), vec![7.0]);
    }

    /// RC2-VOICE batch review P2: the callback path allocates nothing, even for
    /// a callback far larger than its scratch step and far past the ring's
    /// capacity. What fits is queued; the newest samples beyond it are dropped.
    #[test]
    fn rc2_voice_callback_never_allocates_past_the_ring() {
        let buffer = shared();
        let (sink, mut drain) = sample_pipe(buffer.clone(), 1000);
        let mut state = CallbackState::new(2, 48_000, sink);
        // Three seconds of stereo 48 kHz in one callback (both channels carry
        // the frame number), then a small one: 48 086 resampled samples against
        // a 1000-sample ring and a 4096-frame step.
        let frames = 48_000 * 3;
        let callback: Vec<f32> = (0..frames * 2).map(|i| (i / 2) as f32).collect();
        let mono: Vec<f32> = (0..frames).map(|i| i as f32).collect();
        let mut expected = Vec::new();
        let mut reference = StreamResampler::new(48_000, SAMPLE_RATE);
        reference.process(&mono, &mut expected);
        reference.process(&mono[..256], &mut expected);
        let scratch_before = state.scratch_capacity();

        let allocations = allocations_during(|| {
            state.ingest(&callback, |s| s);
            state.ingest(&callback[..512], |s| s);
        });

        assert_eq!(allocations, 0, "the callback path must not allocate");
        assert_eq!(
            state.scratch_capacity(),
            scratch_before,
            "the scratch buffers never grow"
        );
        assert_eq!(state.sink().queued(), 1000, "the ring is full, not grown");
        let report = drain.drain();
        assert_eq!(report.moved, 1000);
        assert_eq!(
            report.newly_dropped,
            (expected.len() - 1000) as u64,
            "every sample that did not fit is counted"
        );
        assert_eq!(
            *lock_samples(&buffer),
            expected[..1000],
            "the oldest samples are kept, in order; the newest were dropped"
        );
    }

    /// The overflow policy: a full ring drops the newest samples, the drain
    /// reports each loss once, and capture carries on once there is room.
    #[test]
    fn rc2_voice_a_full_ring_drops_the_newest_and_counts_them() {
        let buffer = shared();
        let (mut sink, mut drain) = sample_pipe(buffer.clone(), 4);
        assert_eq!(sink.capacity(), 4);
        assert_eq!(sink.push(&[1.0, 2.0, 3.0]), 3);
        assert_eq!(sink.push(&[4.0, 5.0, 6.0]), 1, "one slot was left");
        assert_eq!(sink.queued(), 4);
        assert_eq!(
            drain.drain(),
            DrainReport {
                moved: 4,
                newly_dropped: 2
            }
        );
        assert_eq!(
            drain.drain(),
            DrainReport::default(),
            "a loss is reported once"
        );
        assert_eq!(drain.dropped(), 2);
        assert_eq!(sink.push(&[7.0, 8.0, 9.0]), 3, "room again after a drain");
        drain.drain();
        assert_eq!(
            *lock_samples(&buffer),
            vec![1.0, 2.0, 3.0, 4.0, 7.0, 8.0, 9.0]
        );
        let message = dropped_audio_message(8_000);
        assert!(
            message.contains("0.50 seconds of audio") && message.contains("lost"),
            "{message}"
        );
    }

    /// A stand-in for the cpal stream: it owns the callback's sink, records
    /// the teardown order, and — like a callback that was running when the
    /// stream was torn down — pushes one last batch while it is dropped.
    struct MockStream {
        sink: Option<SampleSink>,
        last_callback: Vec<f32>,
        log: Arc<Mutex<Vec<&'static str>>>,
    }

    impl CaptureStream for MockStream {
        fn pause(&self) {
            self.log.lock().unwrap().push("pause");
        }
    }

    impl Drop for MockStream {
        fn drop(&mut self) {
            if let Some(mut sink) = self.sink.take() {
                sink.push(&self.last_callback);
            }
            self.log.lock().unwrap().push("stream dropped");
        }
    }

    /// RC2-VOICE batch review P3 (review SF1): `stop` drains the ring only
    /// after the stream is dropped, so samples still queued and the last
    /// callback's push both reach the utterance.
    #[test]
    fn rc2_voice_stop_drains_after_the_stream_is_dropped() {
        let buffer = shared();
        let (mut sink, drain) = sample_pipe(buffer.clone(), 64);
        // A drain thread that would not wake on its own for an hour: only the
        // final pass in `stop_capture` can move the samples.
        let drain = DrainThread::spawn(drain, Duration::from_secs(3600)).unwrap();
        sink.push(&[1.0, 2.0, 3.0]);
        let log = Arc::new(Mutex::new(Vec::new()));
        let stream = MockStream {
            sink: Some(sink),
            last_callback: vec![4.0, 5.0],
            log: log.clone(),
        };

        let utterance = stop_capture(stream, drain, &buffer);

        assert_eq!(
            utterance,
            vec![1.0, 2.0, 3.0, 4.0, 5.0],
            "queued samples and the last callback's are in the utterance"
        );
        assert_eq!(*log.lock().unwrap(), vec!["pause", "stream dropped"]);
        assert!(lock_samples(&buffer).is_empty(), "the utterance was taken");
    }

    /// While capture runs, the drain thread keeps the shared buffer current for
    /// snapshots and the conversation drain.
    #[test]
    fn rc2_voice_drain_thread_keeps_the_buffer_current() {
        let buffer = shared();
        let (mut sink, drain) = sample_pipe(buffer.clone(), 64);
        let drain = DrainThread::spawn(drain, Duration::from_millis(1)).unwrap();
        sink.push(&[1.0, 2.0]);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while lock_samples(&buffer).len() < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(*lock_samples(&buffer), vec![1.0, 2.0]);
        assert!(drain.finish().is_some());
    }
}
