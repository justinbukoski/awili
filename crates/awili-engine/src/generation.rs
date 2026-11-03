//! Connection generations (unit 4b, design r9 §1/§7 commit 1).
//!
//! One MCP origin connects, dies, and reconnects; each attempt is a
//! *generation*. The generation is a correlation value, not a capability:
//! admission always compares against the runtime's own live `Ready`
//! generation, so knowing a number confers nothing (design r9 §1).
//!
//! PROCESS uniqueness (unit 5 design r8 §2, r7 verdict ruling 41): every
//! production MCP runtime draws from the ONE process-long allocator,
//! [`GenerationAllocator::process`]. Per-runtime `new()` allocators each
//! started at 1, so a respawned worker's first connection reused `g1` and
//! no cross-worker comparison could fence a retained pre-respawn value.
//! `new()` remains ONLY for isolated unit tests that never cross a
//! runtime boundary.

/// One connection attempt of one origin, minted by [`GenerationAllocator`].
///
/// `Copy` on purpose: it is carried inside lifecycle facts, tokens, and
/// host bookkeeping as a plain correlation value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnectionGeneration(u64);

impl ConnectionGeneration {
    /// Raw value for display/trace only — never an authority input.
    pub fn value(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for ConnectionGeneration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "g{}", self.0)
    }
}

/// The runtime's single monotonic generation source.
///
/// Owned by the MCP runtime loop; one per runtime. Not `Clone`: two
/// allocators could mint colliding generations, and scoped uniqueness is
/// the property the lifecycle machine leans on.
#[derive(Debug)]
pub struct GenerationAllocator {
    next: u64,
}

impl GenerationAllocator {
    /// Isolated allocator for unit tests ONLY — production runtimes use
    /// [`GenerationAllocator::process`] (unit 5 ruling 41: separate
    /// allocators reuse values, so nothing minted here may ever be
    /// compared across a runtime boundary).
    pub fn new() -> Self {
        Self { next: 1 }
    }

    /// Mint the next generation. Monotonic, never reused, never zero.
    /// Not an iterator: minting is infallible and never ends, so `Option` would lie.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> ConnectionGeneration {
        let g = ConnectionGeneration(self.next);
        self.next += 1;
        g
    }
}

/// The PROCESS-long allocator (unit 5 r8 §2): one `AtomicU64` shared by
/// every MCP runtime in the process, monotonic, never reset — a respawned
/// worker can never re-mint a predecessor's generation, which is what
/// makes the seed candidate's generation check a sound respawn fence.
#[derive(Debug)]
pub struct ProcessGenerations(());

/// The one process instance.
pub fn process() -> &'static ProcessGenerations {
    static P: ProcessGenerations = ProcessGenerations(());
    &P
}

static PROCESS_NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl ProcessGenerations {
    /// Mint the next process-unique generation. Never wraps silently
    /// (ruling 48): exhausting u64 is unreachable in practice, and if it
    /// ever happened the process panics rather than reusing a value.
    pub fn next(&self) -> ConnectionGeneration {
        let v = PROCESS_NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert!(v != u64::MAX, "process generation space exhausted");
        ConnectionGeneration(v)
    }
}

impl Default for GenerationAllocator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unit 5 r8 §2: racing runtimes drawing from the process allocator
    /// never collide — a fetch-add proof over a spawn storm.
    #[test]
    fn process_allocator_never_collides_across_threads() {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    (0..250)
                        .map(|_| process().next().value())
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut all: Vec<u64> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        let n = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), n, "every minted generation is unique");
    }

    #[test]
    fn generations_are_monotonic_and_never_reused() {
        let mut a = GenerationAllocator::new();
        let g1 = a.next();
        let g2 = a.next();
        let g3 = a.next();
        assert!(g1 < g2 && g2 < g3);
        assert_ne!(g1, g2);
        assert_eq!(g1.value(), 1);
        assert_eq!(g3.value(), 3);
    }
}
