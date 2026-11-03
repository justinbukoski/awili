// Row 25 fixture: a struct literal for AcceptedRoots must not compile —
// every field is private, and ValidatedRoots has no public constructor
// either.

use awili_seam::AcceptedRoots;

fn main() {
    let _ = AcceptedRoots {
        origin: awili_engine::ToolOrigin::new("mcp:forged"),
        epoch: 999,
    };
}
