//! base64url (no padding) codec matching Buffer#toString("base64url") and
//! effect Encoding.encodeBase64Url, used for opaque pagination cursors.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub fn encode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let buffer = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let value = u32::from_be_bytes([0, buffer[0], buffer[1], buffer[2]]);
        let chars = [
            ALPHABET[((value >> 18) & 63) as usize],
            ALPHABET[((value >> 12) & 63) as usize],
            ALPHABET[((value >> 6) & 63) as usize],
            ALPHABET[(value & 63) as usize],
        ];
        let keep = match chunk.len() {
            1 => 2,
            2 => 3,
            _ => 4,
        };
        for ch in &chars[..keep] {
            out.push(*ch as char);
        }
    }
    out
}

pub fn decode(input: &str) -> Option<Vec<u8>> {
    let index_of = |ch: u8| ALPHABET.iter().position(|item| *item == ch);
    let chars: Vec<u8> = input.bytes().collect();
    let mut bytes: Vec<u8> = vec![];
    for chunk in chars.chunks(4) {
        let mut value: u32 = 0;
        for (position, ch) in chunk.iter().enumerate() {
            value |= (index_of(*ch)? as u32) << (18 - 6 * position);
        }
        let keep = chunk.len().checked_sub(1)?;
        let raw = value.to_be_bytes();
        bytes.extend_from_slice(&raw[1..1 + keep]);
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_json_cursors() {
        let raw = r#"{"directory":"/workspace/packages/opencode","anchor":{"id":"ses_x","time":1783203848860,"direction":"previous"}}"#;
        let encoded = encode(raw);
        assert!(!encoded.contains('='));
        assert_eq!(decode(&encoded).unwrap(), raw.as_bytes());
    }

    #[test]
    fn matches_buffer_base64url_reference() {
        // Buffer.from('{"anchor":1}').toString("base64url")
        assert_eq!(encode("{\"anchor\":1}"), "eyJhbmNob3IiOjF9");
    }
}
