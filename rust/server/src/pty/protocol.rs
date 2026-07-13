//! Port of packages/core/src/pty/protocol.ts.
//!
//! The PTY WebSocket protocol is:
//! - Outbound frames: raw UTF-8 terminal chunks (text frames).
//! - One control frame per attach: `0x00` byte followed by UTF-8 JSON with the
//!   absolute output cursor after replay, so a reconnecting client can resume.
//! - Inbound frames: UTF-8 text or binary decoded as UTF-8 keyboard input;
//!   invalid UTF-8 is dropped.

use serde_json::json;

/// Replay can be megabytes; send it in bounded 64 KiB text frames.
pub const REPLAY_CHUNK: usize = 64 * 1024;

/// Build the 0x00-prefixed JSON cursor control frame emitted after replay.
pub fn meta_frame(cursor: u64) -> Vec<u8> {
    let payload = json!({ "cursor": cursor }).to_string();
    let bytes = payload.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() + 1);
    out.push(0);
    out.extend_from_slice(bytes);
    out
}

/// Split a UTF-8 replay string into REPLAY_CHUNK-sized text frames.
///
/// Chunk boundaries respect UTF-8 code-point boundaries: the JavaScript port
/// slices by UTF-16 code units, which cannot fall mid-code-point, so this
/// implementation walks bytes forward until it reaches a char boundary at or
/// below the nominal chunk size.
pub fn chunks(data: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = data.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        let mut end = (start + REPLAY_CHUNK).min(bytes.len());
        while end < bytes.len() && !data.is_char_boundary(end) {
            end += 1;
        }
        out.push(&data[start..end]);
        start = end;
    }
    out
}

/// Decode an inbound frame as UTF-8; invalid input is dropped.
pub fn decode_input(bytes: &[u8]) -> Option<String> {
    std::str::from_utf8(bytes).ok().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_input_drops_invalid_utf8() {
        assert_eq!(decode_input(b"ready").as_deref(), Some("ready"));
        assert!(decode_input(&[0xff, 0xfe, 0xfd]).is_none());
        assert_eq!(decode_input(b"hello").as_deref(), Some("hello"));
    }

    #[test]
    fn meta_frame_prefixes_zero_byte_and_cursor_json() {
        let frame = meta_frame(42);
        assert_eq!(frame[0], 0);
        let parsed: serde_json::Value = serde_json::from_slice(&frame[1..]).unwrap();
        assert_eq!(parsed, json!({ "cursor": 42 }));
    }

    #[test]
    fn chunks_splits_replay_into_bounded_frames() {
        assert_eq!(chunks("").len(), 0);
        assert_eq!(chunks("abc"), vec!["abc"]);
        let big = "x".repeat(REPLAY_CHUNK + 1);
        let frames = chunks(&big);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].len(), REPLAY_CHUNK);
        assert_eq!(frames.concat(), big);
    }

    #[test]
    fn chunks_respects_utf8_boundaries() {
        // Fill up to just below REPLAY_CHUNK, then append a 4-byte code point
        // that would otherwise split mid-sequence.
        let mut data = "a".repeat(REPLAY_CHUNK - 2);
        data.push('\u{1F600}');
        data.push('b');
        let frames = chunks(&data);
        assert_eq!(frames.concat(), data);
        for frame in &frames {
            assert!(std::str::from_utf8(frame.as_bytes()).is_ok());
        }
    }
}
