//! Escaping of byte strings for the operation log and state file.

use std::fmt::Write;

/// Escape arbitrary bytes for the log. Valid printable UTF-8 passes through;
/// `\` becomes `\\`, `"` becomes `\"`; control bytes, DEL and invalid UTF-8
/// bytes become lowercase `\xNN`.
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
                '"' => out.push_str("\\\""),
                c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                    let _ = write!(out, "\\x{:02x}", c as u32);
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

/// Invert [`escape`]. Errors on a dangling or unknown escape or bad hex.
pub fn unescape(s: &str) -> Result<Vec<u8>, String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        match bytes.get(i + 1) {
            Some(b'\\') => out.push(b'\\'),
            Some(b'"') => out.push(b'"'),
            Some(b'x') => {
                let hex = bytes
                    .get(i + 2..i + 4)
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
                    .ok_or_else(|| format!("bad \\x escape at byte {i} in {s:?}"))?;
                out.push(u8::from_str_radix(hex, 16).map_err(|e| e.to_string())?);
                i += 2;
            }
            Some(c) => return Err(format!("unknown escape \\{} at byte {i}", *c as char)),
            None => return Err(format!("dangling backslash at end of {s:?}")),
        }
        i += 2;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_round_trips() {
        for raw in [
            &b"a b/c.txt"[..],
            b"bad\xff\n",
            b"q\"uote\\",
            "ü.txt".as_bytes(),
            b"",
        ] {
            assert_eq!(unescape(&escape(raw)).unwrap(), raw);
        }
        assert_eq!(escape(b"q\"\n"), "q\\\"\\x0a");
        assert!(unescape("\\x4").is_err());
    }

    #[test]
    fn unescape_rejects_malformed() {
        for bad in ["\\", "\\q", "\\xzz", "\\x"] {
            assert!(unescape(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn escape_del_and_truncated_sequence() {
        assert_eq!(escape(b"a\x7fb"), "a\\x7fb");
        assert_eq!(escape(b"a\xc3"), "a\\xc3");
    }
}
