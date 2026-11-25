//! Names as bytes, for Windows: WTF-8.
//!
//! A Windows name is a sequence of 16-bit units that need not be valid
//! UTF-16: NTFS allows a surrogate without its partner. WTF-8 writes a
//! well-formed name as plain UTF-8, so it has the same bytes as on Unix, and
//! an unpaired surrogate in the three-byte form UTF-8 would use for it.
//!
//! Bytes that are not WTF-8 are an error, never repaired. That includes a
//! surrogate pair written as two three-byte forms, so every name has exactly
//! one stored form.
//!
//! This is safe, platform-neutral code, compiled and tested everywhere (and
//! under Miri); only the step between units and an `OsString` is in
//! `platform/windows/`. It is irsync's own copy of the conversion;
//! `fileset` has a copy of its own.

/// Bytes that are not WTF-8.
#[derive(Debug, PartialEq, Eq)]
pub struct Invalid {
    /// The byte offset of the first bad sequence.
    pub at: usize,
}

fn is_high(u: u16) -> bool {
    (0xD800..=0xDBFF).contains(&u)
}

fn is_low(u: u16) -> bool {
    (0xDC00..=0xDFFF).contains(&u)
}

/// The WTF-8 bytes of 16-bit units: a surrogate pair becomes its code point,
/// any other unit, an unpaired surrogate included, is written as a code point
/// of its own.
pub fn encode(units: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        let u = units[i];
        let cp = match units.get(i + 1) {
            Some(&next) if is_high(u) && is_low(next) => {
                i += 1;
                0x10000 + ((u32::from(u) - 0xD800) << 10) + (u32::from(next) - 0xDC00)
            }
            _ => u32::from(u),
        };
        i += 1;
        if cp < 0x80 {
            out.push(cp as u8);
        } else if cp < 0x800 {
            out.push(0xC0 | (cp >> 6) as u8);
            out.push(0x80 | (cp & 0x3F) as u8);
        } else if cp < 0x10000 {
            out.push(0xE0 | (cp >> 12) as u8);
            out.push(0x80 | ((cp >> 6) & 0x3F) as u8);
            out.push(0x80 | (cp & 0x3F) as u8);
        } else {
            out.push(0xF0 | (cp >> 18) as u8);
            out.push(0x80 | ((cp >> 12) & 0x3F) as u8);
            out.push(0x80 | ((cp >> 6) & 0x3F) as u8);
            out.push(0x80 | (cp & 0x3F) as u8);
        }
    }
    out
}

/// The 16-bit units that WTF-8 bytes stand for. Anything [`encode`] cannot
/// have written is [`Invalid`]: overlong forms, cut-short sequences, code
/// points above U+10FFFF, stray bytes, and a high surrogate written in three
/// bytes directly followed by a low one (that pair has a four-byte form).
pub fn decode(bytes: &[u8]) -> Result<Vec<u16>, Invalid> {
    let mut out = Vec::with_capacity(bytes.len());
    // Where the last sequence started, if it was a three-byte high surrogate.
    let mut lone_high_at = None;
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let bad = Invalid { at: start };
        let b0 = bytes[i];
        // The length of the sequence and the range its second byte must be in.
        let (len, second) = match b0 {
            0x00..=0x7F => (1, 0x80..=0xBF),
            0xC2..=0xDF => (2, 0x80..=0xBF),
            0xE0 => (3, 0xA0..=0xBF),
            0xE1..=0xEF => (3, 0x80..=0xBF),
            0xF0 => (4, 0x90..=0xBF),
            0xF1..=0xF3 => (4, 0x80..=0xBF),
            0xF4 => (4, 0x80..=0x8F),
            _ => return Err(bad),
        };
        let seq = bytes.get(start..start + len).ok_or(Invalid { at: start })?;
        if len > 1 && !second.contains(&seq[1]) {
            return Err(bad);
        }
        if seq[2.min(len)..].iter().any(|b| !(0x80..=0xBF).contains(b)) {
            return Err(bad);
        }
        let cp = match len {
            1 => u32::from(b0),
            2 => (u32::from(b0 & 0x1F) << 6) | u32::from(seq[1] & 0x3F),
            3 => {
                (u32::from(b0 & 0x0F) << 12)
                    | (u32::from(seq[1] & 0x3F) << 6)
                    | u32::from(seq[2] & 0x3F)
            }
            _ => {
                (u32::from(b0 & 0x07) << 18)
                    | (u32::from(seq[1] & 0x3F) << 12)
                    | (u32::from(seq[2] & 0x3F) << 6)
                    | u32::from(seq[3] & 0x3F)
            }
        };
        i += len;
        if cp >= 0x10000 {
            let c = cp - 0x10000;
            out.push(0xD800 + (c >> 10) as u16);
            out.push(0xDC00 + (c & 0x3FF) as u16);
            lone_high_at = None;
            continue;
        }
        let u = cp as u16;
        if is_low(u) {
            if let Some(at) = lone_high_at {
                return Err(Invalid { at });
            }
        }
        lone_high_at = is_high(u).then_some(start);
        out.push(u);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_vectors() {
        let v: [(&[u16], &[u8]); 6] = [
            (&[0x61], b"a"),
            (&[0xE9], &[0xC3, 0xA9]),
            (&[0xD83D, 0xDE00], &[0xF0, 0x9F, 0x98, 0x80]), // a pair: four bytes
            (&[0xD800], &[0xED, 0xA0, 0x80]),               // lone high
            (&[0xDFFF], &[0xED, 0xBF, 0xBF]),               // lone low
            (&[0xDE00, 0xD83D], &[0xED, 0xB8, 0x80, 0xED, 0xA0, 0xBD]), // low then high: two lone
        ];
        for (units, bytes) in v {
            assert_eq!(encode(units), bytes, "encode {units:04X?}");
            assert_eq!(decode(bytes), Ok(units.to_vec()), "decode {bytes:02X?}");
        }
    }

    #[test]
    fn invalid_bytes_are_rejected() {
        let bad: [&[u8]; 5] = [
            &[0xFF],
            &[0xC0, 0x80],                         // overlong
            &[0xE2, 0x82],                         // cut short
            &[0xF4, 0x90, 0x80, 0x80],             // above U+10FFFF
            &[0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80], // a pair written as two three-byte forms
        ];
        for bytes in bad {
            assert_eq!(decode(bytes), Err(Invalid { at: 0 }), "{bytes:02X?}");
        }
        assert_eq!(decode(b"ab\xFF"), Err(Invalid { at: 2 }));
    }

    /// A fixed-seed xorshift64, so the test needs no dependency.
    struct XorShift(u64);

    impl XorShift {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
    }

    #[test]
    fn random_units_round_trip() {
        let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
        for _ in 0..10_000 {
            let len = (rng.next() % 41) as usize;
            let units: Vec<u16> = (0..len)
                .map(|_| {
                    let r = rng.next();
                    if r.is_multiple_of(4) {
                        0xD800 + ((r >> 8) % 0x800) as u16
                    } else {
                        (r >> 8) as u16
                    }
                })
                .collect();
            assert_eq!(decode(&encode(&units)), Ok(units.clone()), "{units:04X?}");
        }
    }

    #[test]
    fn well_formed_names_are_plain_utf8() {
        for s in ["a", "é", "日本", "😀"] {
            assert_eq!(encode(&s.encode_utf16().collect::<Vec<_>>()), s.as_bytes());
        }
    }
}
