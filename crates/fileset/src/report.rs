//! Formatting of report lines and summaries.

use crate::escape::escape;
use crate::model::{Entry, Kind, Timespec};
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

/// The `CHG` line. The path is escaped here; diff values are written as given, so a
/// caller puts escaped text into `FieldDiff` (as `compare::diff` does).
pub fn line_chg(path: &[u8], diffs: &[FieldDiff]) -> String {
    let mut s = format!("CHG {}", escape(path));
    for d in diffs {
        s.push_str(&format!(" {}={}→{}", d.name, d.old, d.new));
    }
    s
}

/// `<path> kind= size= mtime= mode=`, then `blake3=` when the entry has a checksum: what
/// every line that shows an entry in full has in common.
fn entry_fields(path: &[u8], e: &Entry) -> String {
    let mut s = format!(
        "{} kind={} size={} mtime={} mode={}",
        escape(path),
        e.kind.name(),
        e.size,
        fmt_mtime(e.mtime),
        fmt_mode(e.mode)
    );
    if let Some(h) = &e.blake3 {
        s.push_str(" blake3=");
        s.push_str(&fmt_blake3(h));
    }
    s
}

/// One entry in full: `<prefix> <path> kind= size= mtime= mode=`, then `blake3=` when the
/// entry has a checksum.
pub fn line_entry(prefix: &str, path: &[u8], e: &Entry) -> String {
    format!("{prefix} {}", entry_fields(path, e))
}

/// The line `fileset list` prints for an entry: the fields of [`line_entry`] without a
/// prefix, then `target=` for a symlink with a recorded target. With `verbose`, also
/// `uid=`, `gid=`, `nlink=`, `rdev=` for a device, and one `xattr:<name>=<value>` per
/// extended attribute in the order stored (values cut as in `CHG` lines).
pub fn line_listed(path: &[u8], e: &Entry, verbose: bool) -> String {
    let mut s = entry_fields(path, e);
    if let Some(t) = &e.target {
        s.push_str(" target=");
        s.push_str(&escape(t));
    }
    if verbose {
        s.push_str(&format!(" uid={} gid={} nlink={}", e.uid, e.gid, e.nlink));
        if matches!(e.kind, Kind::CharDev | Kind::BlockDev) {
            s.push_str(&format!(" rdev={}", e.rdev));
        }
        for (name, value) in &e.xattrs {
            s.push_str(&format!(
                " xattr:{}={}",
                escape(name),
                fmt_bytes_value(value)
            ));
        }
    }
    s
}

/// The `SKP` line of a built-in exclude the walk skipped.
pub fn line_skp(path: &[u8]) -> String {
    format!("SKP {} reason=builtin-exclude", escape(path))
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
    use crate::model::{Entry, Flags, Kind, Timespec};

    fn entry(kind: Kind, size: u64, mtime: Timespec, mode: u32, blake3: Option<[u8; 32]>) -> Entry {
        Entry {
            kind,
            mode,
            uid: 0,
            gid: 0,
            size,
            nlink: 0,
            ino: 0,
            dev: 0,
            rdev: 0,
            mtime,
            ctime: Timespec { sec: 0, nsec: 0 },
            blake3,
            target: None,
            xattrs: vec![],
            flags: Flags::default(),
            generation: 0,
        }
    }

    /// 2026-01-31T12:00:00.000000005Z
    const NOON: Timespec = Timespec {
        sec: 1769860800,
        nsec: 5,
    };

    #[test]
    fn listed_line_is_the_entry_line_without_a_prefix() {
        let file = entry(Kind::File, 12, NOON, 0o644, Some([0x11; 32]));
        assert_eq!(
            line_listed(b"d/a.txt", &file, false),
            format!(
                "d/a.txt kind=file size=12 mtime=2026-01-31T12:00:00.000000005Z mode=0644 blake3={}",
                "11".repeat(32)
            )
        );
        assert_eq!(
            format!("EQL {}", line_listed(b"d/a.txt", &file, false)),
            line_entry("EQL", b"d/a.txt", &file)
        );
        let dir = entry(Kind::Dir, 4096, NOON, 0o755, None);
        assert_eq!(
            line_listed(b"d", &dir, false),
            "d kind=dir size=4096 mtime=2026-01-31T12:00:00.000000005Z mode=0755"
        );
        let mut link = entry(Kind::Symlink, 4, NOON, 0o777, None);
        assert!(!line_listed(b"l", &link, false).contains("target="));
        link.target = Some(b"a b\n".to_vec());
        let line = line_listed(b"l", &link, false);
        assert!(line.ends_with(" mode=0777 target=a b\\x0a"), "{line}");
    }

    #[test]
    fn listed_line_verbose_adds_everything_recorded() {
        let mut file = entry(Kind::File, 12, NOON, 0o644, Some([0x11; 32]));
        file.uid = 1000;
        file.gid = 100;
        file.nlink = 2;
        file.rdev = 7;
        file.xattrs = vec![
            (b"user.a".to_vec(), b"1".to_vec()),
            (b"user.b".to_vec(), vec![b'x'; 70]),
        ];
        let line = line_listed(b"a", &file, true);
        assert!(
            line.ends_with(&format!(
                " uid=1000 gid=100 nlink=2 xattr:user.a=1 xattr:user.b={}…",
                "x".repeat(64)
            )),
            "{line}"
        );
        // A device number means something only for a device.
        assert!(!line.contains("rdev="), "{line}");
        assert!(line.starts_with(&line_listed(b"a", &file, false)));
        let mut dev = entry(Kind::CharDev, 0, NOON, 0o600, None);
        dev.nlink = 1;
        dev.rdev = 259;
        let line = line_listed(b"dev", &dev, true);
        assert!(line.contains(" nlink=1 rdev=259"), "{line}");
        // The target comes before the verbose fields.
        let mut link = entry(Kind::Symlink, 1, NOON, 0o777, None);
        link.target = Some(b"t".to_vec());
        let line = line_listed(b"l", &link, true);
        assert!(line.contains(" mode=0777 target=t uid="), "{line}");
    }

    #[test]
    fn entry_line_with_checksum() {
        let mut h = [0u8; 32];
        h[0] = 0xab;
        let e = entry(Kind::File, 13, Timespec { sec: 0, nsec: 5 }, 0o644, Some(h));
        assert_eq!(
            line_entry("EQL", b"a/b", &e),
            format!(
                "EQL a/b kind=file size=13 mtime=1970-01-01T00:00:00.000000005Z mode=0644 blake3=ab{}",
                "00".repeat(31)
            )
        );
    }
    #[test]
    fn entry_line_without_checksum_has_no_blake3_field() {
        let e = entry(Kind::Dir, 96, Timespec { sec: 0, nsec: 0 }, 0o755, None);
        assert_eq!(
            line_entry("ADD", b"d", &e),
            "ADD d kind=dir size=96 mtime=1970-01-01T00:00:00.000000000Z mode=0755"
        );
    }
    #[test]
    fn entry_and_skip_lines_escape_the_path() {
        let e = entry(Kind::File, 0, Timespec { sec: 0, nsec: 0 }, 0o600, None);
        let l = line_entry("CHG", b"a\nb\xff", &e);
        assert!(l.starts_with("CHG a\\x0ab\\xff kind=file "), "{l}");
        assert!(!l.contains('\n'));
        assert_eq!(line_skp(b".Trashes"), "SKP .Trashes reason=builtin-exclude");
        assert_eq!(line_skp(b"a\nb"), "SKP a\\x0ab reason=builtin-exclude");
    }

    #[test]
    fn mode_is_four_octal_digits() {
        assert_eq!(fmt_mode(0o644), "0644");
        assert_eq!(fmt_mode(0o4755), "4755");
    }
    #[test]
    fn mtime_rfc3339_nanos() {
        assert_eq!(
            fmt_mtime(Timespec { sec: 0, nsec: 5 }),
            "1970-01-01T00:00:00.000000005Z"
        );
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
        let d = vec![FieldDiff {
            name: "mode".into(),
            old: "0644".into(),
            new: "0600".into(),
        }];
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
        let s = CheckSummary {
            added: 1,
            deleted: 2,
            changed: 3,
            unchanged: 4,
            warnings: 5,
        };
        assert_eq!(
            s.to_string(),
            "added=1 deleted=2 changed=3 unchanged=4 warnings=5"
        );
    }
    #[test]
    fn update_summary_display() {
        let s = UpdateSummary {
            entries: 1,
            hashed: 2,
            reused: 3,
            warnings: 4,
        };
        assert_eq!(s.to_string(), "entries=1 hashed=2 reused=3 warnings=4");
    }
}
