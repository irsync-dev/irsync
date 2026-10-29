//! Lossless-ish escaping of byte strings for human-readable report lines.

use std::fmt::Write;

/// Escape arbitrary bytes for display. Valid printable UTF-8 (and space)
/// passes through; `\` becomes `\\`; control bytes, DEL, and invalid UTF-8
/// bytes become lowercase `\xNN`; C1 controls and the bidi formatting
/// characters, which could make a line read differently from what it says,
/// become `\u{...}`.
pub fn escape(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut rest = bytes;
    while !rest.is_empty() {
        let (valid, bad_len) = match std::str::from_utf8(rest) {
            Ok(s) => (s, 0),
            Err(e) => {
                let (v, _) = rest.split_at(e.valid_up_to());
                (
                    std::str::from_utf8(v).unwrap_or(""),
                    e.error_len().unwrap_or(rest.len() - e.valid_up_to()),
                )
            }
        };
        for c in valid.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                    let _ = write!(out, "\\x{:02x}", c as u32);
                }
                c if alters_reading(c) => {
                    let _ = write!(out, "\\u{{{:x}}}", c as u32);
                }
                c => out.push(c),
            }
        }
        let skip = valid.len();
        for b in &rest[skip..skip + bad_len] {
            let _ = write!(out, "\\x{b:02x}");
        }
        rest = &rest[skip + bad_len..];
    }
    out
}

/// C1 controls, the bidi marks, and the bidi embedding, override and isolate controls.
fn alters_reading(c: char) -> bool {
    matches!(
        c as u32,
        0x80..=0x9f | 0x061c | 0x200e | 0x200f | 0x202a..=0x202e | 0x2066..=0x2069
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_plain_passthrough() {
        assert_eq!(escape(b"a b/c.txt"), "a b/c.txt");
    }
    #[test]
    fn escape_non_utf8() {
        assert_eq!(escape(b"bad\xff\n"), "bad\\xff\\x0a");
    }
    #[test]
    fn escape_backslash() {
        assert_eq!(escape(b"a\\b"), "a\\\\b");
    }
    #[test]
    fn escape_utf8_multibyte_passthrough() {
        assert_eq!(escape("ü.txt".as_bytes()), "ü.txt");
    }
    #[test]
    fn escape_del_and_truncated_sequence() {
        assert_eq!(escape(b"a\x7fb"), "a\\x7fb");
        assert_eq!(escape(b"a\xc3"), "a\\xc3");
    }
    #[test]
    fn escape_c1_controls_and_bidi_formatting() {
        // C1 controls (U+0080..=U+009F) and the bidi overrides, isolates and marks
        // could make a report line read differently from what it says.
        assert_eq!(escape("a\u{85}b".as_bytes()), "a\\u{85}b");
        assert_eq!(escape("x\u{202e}txt.exe".as_bytes()), "x\\u{202e}txt.exe");
        assert_eq!(escape("\u{2066}\u{2069}".as_bytes()), "\\u{2066}\\u{2069}");
        assert_eq!(escape("\u{200f}\u{061c}".as_bytes()), "\\u{200f}\\u{61c}");
        assert_eq!(escape("é→ü".as_bytes()), "é→ü");
    }
}
