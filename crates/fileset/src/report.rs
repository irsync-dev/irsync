//! Formatting of report lines and summaries.

use crate::escape::escape;
use crate::model::{Kind, Timespec};
use std::fmt;

/// One changed field of a `CHG` line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldDiff {
    pub name: String,
    pub old: String,
    pub new: String,
}

pub fn fmt_mode(m: u32) -> String {
    format!("{m:04o}")
}

pub fn fmt_mtime(t: Timespec) -> String {
    match jiff::Timestamp::new(t.sec, 0) {
        Ok(ts) => format!("{}.{:09}Z", ts.strftime("%Y-%m-%dT%H:%M:%S"), t.nsec),
        Err(_) => format!("{}.{:09}", t.sec, t.nsec),
    }
}

pub fn fmt_blake3(h: &[u8; 32]) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

const MAX_VALUE_BYTES: usize = 64;

pub fn fmt_bytes_value(v: &[u8]) -> String {
    if v.len() <= MAX_VALUE_BYTES {
        return escape(v);
    }
    let mut cut = MAX_VALUE_BYTES;
    // A sequence cut short by the limit (not genuinely invalid) is dropped whole.
    if let Err(e) = std::str::from_utf8(&v[..cut]) {
        if e.error_len().is_none() {
            cut = e.valid_up_to();
        }
    }
    format!("{}…", escape(&v[..cut]))
}

pub fn line_add(path: &[u8], kind: Kind) -> String {
    format!("ADD {} kind={}", escape(path), kind.name())
}

pub fn line_del(path: &[u8]) -> String {
    format!("DEL {}", escape(path))
}

pub fn line_chg(path: &[u8], diffs: &[FieldDiff]) -> String {
    let mut s = format!("CHG {}", escape(path));
    for d in diffs {
        s.push_str(&format!(" {}={}→{}", d.name, d.old, d.new));
    }
    s
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckSummary {
    pub added: u64,
    pub deleted: u64,
    pub changed: u64,
    pub unchanged: u64,
    pub warnings: u64,
}

impl fmt::Display for CheckSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "added={} deleted={} changed={} unchanged={} warnings={}",
            self.added, self.deleted, self.changed, self.unchanged, self.warnings
        )
    }
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateSummary {
    pub entries: u64,
    pub hashed: u64,
    pub reused: u64,
    pub warnings: u64,
}

impl fmt::Display for UpdateSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "entries={} hashed={} reused={} warnings={}",
            self.entries, self.hashed, self.reused, self.warnings
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, Timespec};

    #[test]
    fn mode_is_four_octal_digits() {
        assert_eq!(fmt_mode(0o644), "0644");
        assert_eq!(fmt_mode(0o4755), "4755");
    }
    #[test]
    fn mtime_rfc3339_nanos() {
        assert_eq!(fmt_mtime(Timespec { sec: 0, nsec: 5 }), "1970-01-01T00:00:00.000000005Z");
    }
    #[test]
    fn blake3_hex() {
        let mut h = [0u8; 32];
        h[0] = 0xab;
        h[31] = 0x01;
        let s = fmt_blake3(&h);
        assert_eq!(s.len(), 64);
        assert!(s.starts_with("ab") && s.ends_with("01"));
    }
    #[test]
    fn bytes_value_truncates() {
        assert_eq!(fmt_bytes_value(b"hi\n"), "hi\\x0a");
        let long = vec![b'a'; 65];
        assert_eq!(fmt_bytes_value(&long), format!("{}…", "a".repeat(64)));
        let exact = vec![b'a'; 64];
        assert_eq!(fmt_bytes_value(&exact), "a".repeat(64));
    }
    #[test]
    fn bytes_value_does_not_split_utf8() {
        let mut v = vec![b'a'; 63];
        v.extend_from_slice("ü".as_bytes());
        assert_eq!(fmt_bytes_value(&v), format!("{}…", "a".repeat(63)));
    }
    #[test]
    fn chg_line_format() {
        let d = vec![FieldDiff { name: "mode".into(), old: "0644".into(), new: "0600".into() }];
        assert_eq!(line_chg(b"a/b", &d), "CHG a/b mode=0644→0600");
    }
    #[test]
    fn add_line_has_kind() {
        assert_eq!(line_add(b"x", Kind::File), "ADD x kind=file");
    }
    #[test]
    fn del_line() {
        assert_eq!(line_del(b"x"), "DEL x");
    }
    #[test]
    fn check_summary_display() {
        let s = CheckSummary { added: 1, deleted: 2, changed: 3, unchanged: 4, warnings: 5 };
        assert_eq!(s.to_string(), "added=1 deleted=2 changed=3 unchanged=4 warnings=5");
    }
    #[test]
    fn update_summary_display() {
        let s = UpdateSummary { entries: 1, hashed: 2, reused: 3, warnings: 4 };
        assert_eq!(s.to_string(), "entries=1 hashed=2 reused=3 warnings=4");
    }
}
