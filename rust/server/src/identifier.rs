//! Port of packages/schema/src/identifier.ts.
//!
//! Identifiers are 26 chars: 12 hex chars encoding a 48-bit monotonic
//! (timestamp * 4096 + counter) value — bitwise-negated for descending order —
//! followed by 14 random chars from a 62-char alphabet.

use rand::RngCore;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const LENGTH: usize = 26;
const CHARS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

static STATE: Mutex<(u64, u64)> = Mutex::new((0, 0));

pub fn descending() -> String {
    create(true, now_millis())
}

pub fn ascending() -> String {
    create(false, now_millis())
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_millis() as u64
}

pub fn create(descending: bool, timestamp: u64) -> String {
    let counter = {
        let mut state = STATE.lock().expect("identifier state poisoned");
        if timestamp != state.0 {
            *state = (timestamp, 0);
        }
        state.1 += 1;
        state.1
    };

    let current = (timestamp as u128) * 0x1000 + counter as u128;
    // The TS source negates an arbitrary-precision BigInt and masks 48 bits,
    // which is the two's complement of the low 48 bits.
    let masked = (current & 0xFFFF_FFFF_FFFF) as u64;
    let value = if descending {
        (!masked) & 0xFFFF_FFFF_FFFF
    } else {
        masked
    };

    let mut out = String::with_capacity(LENGTH);
    for index in 0..6 {
        let byte = (value >> (40 - 8 * index)) & 0xFF;
        out.push_str(&format!("{byte:02x}"));
    }

    let mut bytes = [0u8; LENGTH - 12];
    rand::thread_rng().fill_bytes(&mut bytes);
    for byte in bytes {
        out.push(CHARS[(byte % 62) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference vectors generated from packages/schema/src/identifier.ts with a
    // fresh counter (first call for each timestamp yields counter = 1).
    #[test]
    fn matches_typescript_reference_vectors() {
        assert_eq!(&create(true, 1_751_659_200_000)[..12], "828f9c1ffffe");
        assert_eq!(&create(false, 1_751_659_200_001)[..12], "7d7063e01001");
        assert_eq!(&create(true, 9_999_999_999_999)[..12], "7b18d6000ffe");
    }

    // The 48-bit value wraps every ~2.2 years (matching the TS BigInt byte
    // extraction), so ordering guarantees only hold between nearby timestamps.
    #[test]
    fn descending_ids_sort_newest_first() {
        let older = create(true, 1_783_197_020_000);
        let newer = create(true, 1_783_197_021_000);
        assert!(newer < older);
    }
}
