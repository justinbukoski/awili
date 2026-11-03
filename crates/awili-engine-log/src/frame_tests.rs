//! S5(c) framing acceptance: torn tails, inflated lengths, and decoder
//! honesty about what it can prove (F0539, F0595, RVS5C-FIX1).

use crate::{frame, LogError};

/// A genuine final torn write that died inside the 8-byte header is a
/// PROVEN torn tail: no corruption of a whole frame leaves fewer than 8
/// bytes. RVS5C-FIX1 (spec-mandated): the fixture tears at 5 bytes — a
/// complete header with a short payload is no longer provable (covered by
/// `complete_header_with_short_payload_is_uncertain_not_torn`).
#[test]
fn genuine_torn_tail_is_proven_not_corrupt() {
    let full = frame::encode(b"record-payload");
    let buf: Vec<u8> = full[..5].to_vec();
    let frame::DecodeResult::TornTail { frames, torn } = frame::decode_all(&buf).unwrap() else {
        panic!("an incomplete header must be a proven torn tail");
    };
    assert!(frames.is_empty());
    assert_eq!(torn, buf.len());
}

/// An INFLATED LENGTH before a valid later frame is mid-log corruption
/// (F0539): the declared frame does not fit, but a frame that actually
/// fits and CRC-checks starts later — the header is corrupt, and the
/// decoder must not call it a torn tail or discard the later frame.
#[test]
fn inflated_middle_length_is_corrupt() {
    let good = frame::encode(b"later-valid-frame");
    let mut buf: Vec<u8> = Vec::with_capacity(8 + 999 + good.len());
    // The inflated frame declares 999 payload bytes but the buffer holds
    // far less; the CRC is whatever the encoder of 999 would not have
    // produced — it is damaged, like a corrupted length field.
    buf.extend_from_slice(&999u32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&good);
    match frame::decode_all(&buf) {
        Err(LogError::Corrupt { seq: 0, .. }) => {}
        other => {
            panic!("inflated length before a valid later frame must be corrupt, got {other:?}")
        }
    }
}

/// RVS5C-FIX1 — SPEC-MANDATED BEHAVIOUR CHANGE, not a weakening (review
/// RVS5C-sp P1 frame.rs:64). The name is kept only for the no-test-deletion
/// gate; the assertion is INVERTED. An inflated length with nothing
/// plausible after it cannot be told apart from a torn write, so it is an
/// UNCERTAIN tail — never the repairable `TornTail`.
#[test]
fn inflated_tail_with_no_later_frame_is_a_torn_tail() {
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(&999u32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    // A few payload bytes that do not form a frame.
    buf.extend_from_slice(&[1u8, 2, 3, 4, 5]);
    match frame::decode_all(&buf) {
        Ok(frame::DecodeResult::UncertainTail { frames, tail }) => {
            assert!(frames.is_empty());
            assert_eq!(tail, buf.len());
        }
        other => panic!("an inflated tail must be uncertain, never repairable: {other:?}"),
    }
}

/// RVS5C-FIX1: a complete header with a short payload is indistinguishable
/// from a corrupted length — UNCERTAIN, not torn — even when the bytes are
/// a real prefix of a real frame. Whole frames before it are returned.
#[test]
fn complete_header_with_short_payload_is_uncertain_not_torn() {
    let first = frame::encode(b"first-frame");
    let full = frame::encode(b"record-payload");
    let buf: Vec<u8> = [&first[..], &full[..8 + 3]].concat();
    match frame::decode_all(&buf) {
        Ok(frame::DecodeResult::UncertainTail { frames, tail }) => {
            assert_eq!(frames, vec![b"first-frame".to_vec()]);
            assert_eq!(tail, 8 + 3);
        }
        other => panic!("header + short payload must be uncertain: {other:?}"),
    }
}

/// RVS5C-FIX1: an inflated length over a COMPLETE final payload is proven
/// corruption — the bytes present CRC-check, so only the length is wrong.
#[test]
fn inflated_length_over_complete_final_payload_is_corrupt_tail() {
    let mut buf = frame::encode(b"record-payload");
    buf[..4].copy_from_slice(&999u32.to_le_bytes());
    match frame::decode_all(&buf) {
        Err(LogError::CorruptTail) => {}
        other => panic!("a corrupted length over a whole payload must be CorruptTail: {other:?}"),
    }
}

/// RVS5C-FIX1: a complete final frame that fails its CRC is not what a
/// torn (prefix) write leaves — proven corruption, never a repairable tail.
#[test]
fn crc_mismatch_on_complete_final_frame_is_corrupt_tail() {
    let mut buf: Vec<u8> = [
        frame::encode(b"first-frame"),
        frame::encode(b"second-frame"),
    ]
    .concat();
    let last = buf.len() - 1;
    buf[last] ^= 0xFF;
    match frame::decode_all(&buf) {
        Err(LogError::CorruptTail) => {}
        other => panic!("a CRC-failing complete final frame must be CorruptTail: {other:?}"),
    }
}

/// A short header after whole frames stays a PROVEN torn tail, and the
/// whole frames before it are returned intact (what repair keeps).
#[test]
fn short_header_after_whole_frames_is_a_proven_torn_tail() {
    let second = frame::encode(b"second-frame");
    let buf: Vec<u8> = [frame::encode(b"first-frame"), second[..5].to_vec()].concat();
    let frame::DecodeResult::TornTail { frames, torn } = frame::decode_all(&buf).unwrap() else {
        panic!("a short header after whole frames must be a proven torn tail");
    };
    assert_eq!(frames, vec![b"first-frame".to_vec()]);
    assert_eq!(torn, 5);
}
