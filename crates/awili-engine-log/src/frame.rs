//! Record framing: `[len: u32 LE][crc32: u32 LE][payload: len bytes]`.
//!
//! Tail classification (S5(c) F0539, RVS5C-FIX1). The length field sits
//! OUTSIDE the payload CRC, so when a complete header declares a frame that
//! runs past EOF the decoder cannot tell a torn write from a corrupted
//! length. Only an INCOMPLETE header (fewer than 8 bytes) cannot be made by
//! corrupting a whole frame, so it is the only provable torn write:
//!
//! - `TornTail` — fewer than 8 bytes after the last whole frame. The ONLY
//!   shape the grant store's repair may truncate.
//! - `UncertainTail` — a complete header whose frame runs past EOF, with no
//!   CRC-valid frame after it and no proof the length alone is wrong.
//!   Read-only callers may report it; nothing may truncate it, open over
//!   it, append behind it, or migrate past it.
//! - `Err(Corrupt)` — a CRC mismatch with readable bytes after it, or an
//!   inflated length before a plausible later frame (mid-log corruption).
//! - `Err(CorruptTail)` — proven tail corruption: the final frame is
//!   complete but fails its CRC, or the bytes after an inflated header
//!   CRC-check as a complete payload (only the length is wrong).

use crate::LogError;

pub fn encode(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Outcome of decoding a whole log buffer.
#[derive(Debug)]
pub enum DecodeResult {
    /// Every byte decoded; `frames` holds the payloads.
    Clean(Vec<Vec<u8>>),
    /// PROVABLE torn write: the log ends with `torn` (< 8) bytes of an
    /// incomplete header. The grant store may repair by truncating to
    /// `buf.len() - torn`.
    TornTail { frames: Vec<Vec<u8>>, torn: usize },
    /// UNPROVABLE tail (RVS5C-FIX1): the last `tail` bytes start with a
    /// complete header whose declared frame runs past EOF. It may be a torn
    /// write or a corrupted length; it is never repaired, opened over,
    /// appended behind, or migrated past.
    UncertainTail { frames: Vec<Vec<u8>>, tail: usize },
}

/// Decode all whole frames from `buf` and classify the tail (module docs).
pub fn decode_all(buf: &[u8]) -> Result<DecodeResult, LogError> {
    let mut frames = Vec::new();
    let mut pos = 0usize;
    while pos < buf.len() {
        let remaining = buf.len() - pos;
        if remaining < 8 {
            // Incomplete header: no corruption of a whole frame leaves fewer
            // than 8 bytes — the one provable torn write.
            return Ok(DecodeResult::TornTail {
                frames,
                torn: remaining,
            });
        }
        let len = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
        let crc = u32::from_le_bytes(buf[pos + 4..pos + 8].try_into().unwrap());
        if remaining - 8 < len {
            if inflated_with_plausible_later_frame(buf, pos) {
                // A frame that fits and CRC-checks starts later: the header
                // is corrupt and would hide it (F0539).
                return Err(LogError::Corrupt {
                    seq: frames.len() as u64,
                    reason: "inflated length before a plausible later frame".into(),
                });
            }
            if crc32fast::hash(&buf[pos + 8..]) == crc {
                // The bytes present ARE the complete payload: only the
                // length is wrong. Proven corruption, not a torn write.
                return Err(LogError::CorruptTail);
            }
            // Torn write or corrupted length — the decoder cannot tell.
            return Ok(DecodeResult::UncertainTail {
                frames,
                tail: remaining,
            });
        }
        let payload = &buf[pos + 8..pos + 8 + len];
        if crc32fast::hash(payload) != crc {
            if pos + 8 + len == buf.len() {
                // A complete final frame that fails its CRC is not what a
                // torn (prefix) write leaves: proven tail corruption.
                return Err(LogError::CorruptTail);
            }
            return Err(LogError::Corrupt {
                seq: frames.len() as u64,
                reason: "crc mismatch mid-log".into(),
            });
        }
        frames.push(payload.to_vec());
        pos += 8 + len;
    }
    Ok(DecodeResult::Clean(frames))
}

/// Does a plausible complete frame start somewhere after `pos`? A
/// plausible frame declares a length that fits in the buffer AND its
/// payload CRC matches. This is the probe that turns an "inflated
/// length" into proven corruption (F0539): the inflated header cannot
/// itself be a torn write when a well-formed frame follows it.
fn inflated_with_plausible_later_frame(buf: &[u8], pos: usize) -> bool {
    let end = buf.len();
    let mut start = pos + 8;
    while start + 8 <= end {
        let decl = u32::from_le_bytes(buf[start..start + 4].try_into().unwrap()) as usize;
        let want = start + 8 + decl;
        if want <= end {
            let payload = &buf[start + 8..start + 8 + decl];
            let want_crc = u32::from_le_bytes(buf[start + 4..start + 8].try_into().unwrap());
            if crc32fast::hash(payload) == want_crc {
                return true;
            }
        }
        start += 1;
    }
    false
}
