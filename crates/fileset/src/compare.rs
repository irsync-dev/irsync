//! Comparison tiers, field selection and entry diffing.

use crate::escape::escape;
use crate::model::{Entry, Flags, Kind};
use crate::report::{fmt_blake3, fmt_bytes_value, fmt_mode, fmt_mtime, FieldDiff};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Field {
    Blake3,
    Target,
    Rdev,
    Mode,
    Mtime,
    Size,
    Uid,
    Gid,
    Nlink,
    Xattr,
}

impl Field {
    pub const ALL: [Field; 10] = [
        Field::Blake3,
        Field::Target,
        Field::Rdev,
        Field::Mode,
        Field::Mtime,
        Field::Size,
        Field::Uid,
        Field::Gid,
        Field::Nlink,
        Field::Xattr,
    ];

    pub fn parse(s: &str) -> Option<Field> {
        Field::ALL.into_iter().find(|f| f.name() == s)
    }

    pub fn name(self) -> &'static str {
        match self {
            Field::Blake3 => "blake3",
            Field::Target => "target",
            Field::Rdev => "rdev",
            Field::Mode => "mode",
            Field::Mtime => "mtime",
            Field::Size => "size",
            Field::Uid => "uid",
            Field::Gid => "gid",
            Field::Nlink => "nlink",
            Field::Xattr => "xattr",
        }
    }

    /// Position in `ALL`, which lists the variants in declaration order.
    fn idx(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    Relaxed,
    Default,
    Paranoid,
}

/// The fields a tier compares.
pub fn tier(t: Tier) -> &'static [Field] {
    use Field::*;
    match t {
        Tier::Relaxed => &[Blake3, Target, Rdev],
        Tier::Default => &[Blake3, Target, Rdev, Mode, Mtime, Size],
        Tier::Paranoid => &[
            Blake3, Target, Rdev, Mode, Mtime, Size, Uid, Gid, Nlink, Xattr,
        ],
    }
}

/// One flag per `Field`, indexed by its position in `Field::ALL` (declaration order).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection([bool; Field::ALL.len()]);

impl Selection {
    /// Tier fields, minus `ignore`, plus `require`.
    pub fn new(t: Tier, ignore: &[Field], require: &[Field]) -> Selection {
        let mut set = [false; Field::ALL.len()];
        for f in tier(t) {
            set[f.idx()] = true;
        }
        for f in ignore {
            set[f.idx()] = false;
        }
        for f in require {
            set[f.idx()] = true;
        }
        Selection(set)
    }

    pub fn contains(&self, f: Field) -> bool {
        self.0[f.idx()]
    }
}

fn d(name: &str, old: String, new: String) -> FieldDiff {
    FieldDiff {
        name: name.to_string(),
        old,
        new,
    }
}

fn num<T: ToString + PartialEq>(out: &mut Vec<FieldDiff>, name: &str, old: T, new: T) {
    if old != new {
        out.push(d(name, old.to_string(), new.to_string()));
    }
}

/// Compares stored vs observed under `sel`. Kind is always compared; if it
/// differs the result is exactly one diff named "kind". `mtime_exempt` skips mtime
/// (for the root and the directory holding an in-tree store).
pub fn diff(
    mtime_exempt: bool,
    stored: &Entry,
    observed: &Entry,
    sel: &Selection,
) -> Vec<FieldDiff> {
    if stored.kind != observed.kind {
        return vec![d(
            "kind",
            stored.kind.name().into(),
            observed.kind.name().into(),
        )];
    }
    let mut out = Vec::new();
    if stored.flags.contains(Flags::STORE) {
        return out;
    }
    let kind = stored.kind;
    for f in Field::ALL {
        if !sel.contains(f) {
            continue;
        }
        match f {
            Field::Blake3 => {
                if let (Kind::File, Some(h)) = (kind, stored.blake3) {
                    if observed.flags.contains(Flags::UNREADABLE) {
                        out.push(d("blake3", fmt_blake3(&h), "unreadable".into()));
                    } else if let Some(o) = observed.blake3 {
                        if o != h {
                            out.push(d("blake3", fmt_blake3(&h), fmt_blake3(&o)));
                        }
                    }
                    // An observed file without a hash was not sent to be hashed (the
                    // pipeline skipped it); there is nothing to compare, not a difference.
                }
            }
            Field::Target => {
                if kind == Kind::Symlink && stored.target != observed.target {
                    let r = |t: &Option<Vec<u8>>| t.as_deref().map_or_else(|| "-".into(), escape);
                    out.push(d("target", r(&stored.target), r(&observed.target)));
                }
            }
            Field::Rdev => {
                if matches!(kind, Kind::CharDev | Kind::BlockDev) {
                    num(&mut out, "rdev", stored.rdev, observed.rdev);
                }
            }
            Field::Mode => {
                if stored.mode != observed.mode {
                    out.push(d("mode", fmt_mode(stored.mode), fmt_mode(observed.mode)));
                }
            }
            Field::Mtime => {
                if !mtime_exempt && stored.mtime != observed.mtime {
                    out.push(d(
                        "mtime",
                        fmt_mtime(stored.mtime),
                        fmt_mtime(observed.mtime),
                    ));
                }
            }
            Field::Size => {
                if kind != Kind::Dir {
                    num(&mut out, "size", stored.size, observed.size);
                }
            }
            Field::Uid => num(&mut out, "uid", stored.uid, observed.uid),
            Field::Gid => num(&mut out, "gid", stored.gid, observed.gid),
            Field::Nlink => {
                if kind != Kind::Dir {
                    num(&mut out, "nlink", stored.nlink, observed.nlink);
                }
            }
            Field::Xattr => xattr_diffs(stored, observed, &mut out),
        }
    }
    out
}

/// Merge-walks two name-sorted xattr lists, one diff per differing name.
fn xattr_diffs(stored: &Entry, observed: &Entry, out: &mut Vec<FieldDiff>) {
    use std::cmp::Ordering::{Equal, Greater, Less};
    let (a, b) = (&stored.xattrs, &observed.xattrs);
    let sorted = |v: &[(Vec<u8>, Vec<u8>)]| v.windows(2).all(|w| w[0].0 < w[1].0);
    debug_assert!(sorted(a) && sorted(b), "xattr lists are sorted by name");
    let (mut i, mut j) = (0, 0);
    let mut emit = |name: &[u8], old: Option<&[u8]>, new: Option<&[u8]>| {
        let r = |v: Option<&[u8]>| v.map_or_else(|| "-".into(), fmt_bytes_value);
        out.push(d(&format!("xattr:{}", escape(name)), r(old), r(new)));
    };
    while i < a.len() || j < b.len() {
        let ord = match (a.get(i), b.get(j)) {
            (Some((na, _)), Some((nb, _))) => na.cmp(nb),
            (Some(_), None) => Less,
            _ => Greater,
        };
        match ord {
            Less => {
                emit(&a[i].0, Some(&a[i].1), None);
                i += 1;
            }
            Greater => {
                emit(&b[j].0, None, Some(&b[j].1));
                j += 1;
            }
            Equal => {
                if a[i].1 != b[j].1 {
                    emit(&a[i].0, Some(&a[i].1), Some(&b[j].1));
                }
                i += 1;
                j += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Timespec;

    fn ent(kind: Kind) -> Entry {
        Entry {
            kind,
            mode: 0o644,
            uid: 1000,
            gid: 1000,
            size: 10,
            nlink: 1,
            ino: 1,
            dev: 1,
            rdev: 0,
            mtime: Timespec { sec: 100, nsec: 5 },
            ctime: Timespec { sec: 100, nsec: 5 },
            blake3: None,
            target: None,
            xattrs: vec![],
            flags: Flags::default(),
            generation: 0,
        }
    }

    fn sel(t: Tier) -> Selection {
        Selection::new(t, &[], &[])
    }

    fn names(v: &[FieldDiff]) -> Vec<&str> {
        v.iter().map(|x| x.name.as_str()).collect()
    }

    #[test]
    fn tiers_contain_spec_fields() {
        use Field::*;
        assert_eq!(tier(Tier::Relaxed), &[Blake3, Target, Rdev]);
        assert_eq!(
            tier(Tier::Default),
            &[Blake3, Target, Rdev, Mode, Mtime, Size]
        );
        assert_eq!(
            tier(Tier::Paranoid),
            &[Blake3, Target, Rdev, Mode, Mtime, Size, Uid, Gid, Nlink, Xattr]
        );
    }

    #[test]
    fn selection_ignore_and_require() {
        let s = Selection::new(Tier::Default, &[Field::Mtime], &[Field::Uid]);
        assert!(!s.contains(Field::Mtime));
        assert!(s.contains(Field::Uid));
        assert!(s.contains(Field::Mode));
        assert!(!s.contains(Field::Gid));
    }

    #[test]
    fn field_parse_round_trip() {
        for f in Field::ALL {
            assert_eq!(Field::parse(f.name()), Some(f));
        }
        assert_eq!(Field::parse("bogus"), None);
    }

    #[test]
    fn diff_equal_entries_is_empty() {
        let e = ent(Kind::File);
        assert!(diff(false, &e, &e.clone(), &sel(Tier::Paranoid)).is_empty());
    }

    #[test]
    fn diff_kind_change_reports_only_kind() {
        let a = ent(Kind::File);
        let mut b = ent(Kind::Dir);
        b.mode = 0o755;
        let v = diff(false, &a, &b, &sel(Tier::Paranoid));
        assert_eq!(
            v,
            vec![FieldDiff {
                name: "kind".into(),
                old: "file".into(),
                new: "dir".into()
            }]
        );
    }

    #[test]
    fn diff_mode_and_mtime_in_default() {
        let a = ent(Kind::File);
        let mut b = a.clone();
        b.mode = 0o600;
        b.mtime = Timespec { sec: 200, nsec: 0 };
        let v = diff(false, &a, &b, &sel(Tier::Default));
        assert_eq!(names(&v), ["mode", "mtime"]);
        assert_eq!(v[0].old, fmt_mode(0o644));
        assert_eq!(v[0].new, fmt_mode(0o600));
        assert_eq!(v[1].old, fmt_mtime(a.mtime));
        assert_eq!(v[1].new, fmt_mtime(b.mtime));
    }

    #[test]
    fn diff_blake3_unreadable_observed() {
        let mut a = ent(Kind::File);
        a.blake3 = Some([1; 32]);
        let mut b = a.clone();
        b.blake3 = None;
        b.flags.insert(Flags::UNREADABLE);
        let v = diff(false, &a, &b, &sel(Tier::Default));
        assert_eq!(
            v,
            vec![FieldDiff {
                name: "blake3".into(),
                old: fmt_blake3(&[1; 32]),
                new: "unreadable".into()
            }]
        );
    }

    #[test]
    fn diff_blake3_changed() {
        let mut a = ent(Kind::File);
        a.blake3 = Some([1; 32]);
        let mut b = a.clone();
        b.blake3 = Some([2; 32]);
        let v = diff(false, &a, &b, &sel(Tier::Relaxed));
        assert_eq!(v[0].new, fmt_blake3(&[2; 32]));
    }

    #[test]
    fn diff_skips_blake3_when_stored_has_none() {
        let a = ent(Kind::File);
        let mut b = a.clone();
        b.blake3 = Some([2; 32]);
        assert!(diff(false, &a, &b, &sel(Tier::Default)).is_empty());
    }

    #[test]
    fn diff_dir_ignores_size_and_nlink_even_paranoid() {
        let a = ent(Kind::Dir);
        let mut b = a.clone();
        b.size = 4096;
        b.nlink = 5;
        assert!(diff(false, &a, &b, &sel(Tier::Paranoid)).is_empty());
    }

    #[test]
    fn diff_mtime_exempt_ignores_mtime() {
        let a = ent(Kind::Dir);
        let mut b = a.clone();
        b.mtime = Timespec { sec: 999, nsec: 0 };
        assert!(diff(true, &a, &b, &sel(Tier::Default)).is_empty());
        assert_eq!(names(&diff(false, &a, &b, &sel(Tier::Default))), ["mtime"]);
    }

    #[test]
    fn diff_store_flag_only_kind() {
        let mut a = ent(Kind::Dir);
        a.flags.insert(Flags::STORE);
        let mut b = ent(Kind::Dir);
        b.mode = 0o700;
        b.uid = 5;
        b.mtime = Timespec { sec: 1, nsec: 1 };
        assert!(diff(false, &a, &b, &sel(Tier::Paranoid)).is_empty());
    }

    #[test]
    fn diff_xattr_added_and_changed_in_paranoid() {
        let mut a = ent(Kind::File);
        a.xattrs = vec![(b"user.b".to_vec(), b"x".to_vec())];
        let mut b = a.clone();
        b.xattrs = vec![
            (b"user.a".to_vec(), b"1".to_vec()),
            (b"user.b".to_vec(), b"y".to_vec()),
        ];
        let v = diff(false, &a, &b, &sel(Tier::Paranoid));
        assert_eq!(
            v,
            vec![
                FieldDiff {
                    name: "xattr:user.a".into(),
                    old: "-".into(),
                    new: fmt_bytes_value(b"1")
                },
                FieldDiff {
                    name: "xattr:user.b".into(),
                    old: fmt_bytes_value(b"x"),
                    new: fmt_bytes_value(b"y")
                },
            ]
        );
    }

    #[test]
    fn diff_xattr_removed_in_paranoid() {
        let mut a = ent(Kind::File);
        a.xattrs = vec![
            (b"user.a".to_vec(), b"1".to_vec()),
            (b"user.b".to_vec(), b"2".to_vec()),
        ];
        let mut b = a.clone();
        b.xattrs.remove(0);
        let v = diff(false, &a, &b, &sel(Tier::Paranoid));
        assert_eq!(
            v,
            vec![FieldDiff {
                name: "xattr:user.a".into(),
                old: fmt_bytes_value(b"1"),
                new: "-".into()
            }]
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "sorted by name")]
    fn diff_xattr_requires_sorted_lists() {
        let mut a = ent(Kind::File);
        a.xattrs = vec![
            (b"user.b".to_vec(), b"2".to_vec()),
            (b"user.a".to_vec(), b"1".to_vec()),
        ];
        diff(false, &a, &a.clone(), &sel(Tier::Paranoid));
    }

    #[test]
    fn diff_target_only_for_symlinks() {
        let mut a = ent(Kind::Symlink);
        a.target = Some(b"old".to_vec());
        let mut b = a.clone();
        b.target = Some(b"new".to_vec());
        let v = diff(false, &a, &b, &sel(Tier::Relaxed));
        assert_eq!(
            v,
            vec![FieldDiff {
                name: "target".into(),
                old: "old".into(),
                new: "new".into()
            }]
        );
        // A link whose target could not be read shows as "-".
        b.target = None;
        assert_eq!(diff(false, &a, &b, &sel(Tier::Relaxed))[0].new, "-");
        // The field belongs to symlinks: a file's target is never looked at.
        let mut c = ent(Kind::File);
        c.target = Some(b"old".to_vec());
        let mut e = c.clone();
        e.target = None;
        assert!(diff(false, &c, &e, &sel(Tier::Paranoid)).is_empty());
    }

    #[test]
    fn diff_rdev_only_for_devices() {
        for kind in [Kind::CharDev, Kind::BlockDev] {
            let mut a = ent(kind);
            a.rdev = 1;
            let mut b = a.clone();
            b.rdev = 2;
            assert_eq!(names(&diff(false, &a, &b, &sel(Tier::Relaxed))), ["rdev"]);
        }
        let mut a = ent(Kind::Fifo);
        a.rdev = 1;
        let mut b = a.clone();
        b.rdev = 2;
        assert!(diff(false, &a, &b, &sel(Tier::Paranoid)).is_empty());
    }

    #[test]
    fn diff_size_for_every_kind_but_dir() {
        let a = ent(Kind::Symlink);
        let mut b = a.clone();
        b.size = 11;
        let v = diff(false, &a, &b, &sel(Tier::Default));
        assert_eq!(names(&v), ["size"]);
        assert_eq!((v[0].old.as_str(), v[0].new.as_str()), ("10", "11"));
        assert!(diff(false, &a, &b, &sel(Tier::Relaxed)).is_empty());
    }

    #[test]
    fn diff_nlink_and_gid_only_in_paranoid() {
        let a = ent(Kind::File);
        let mut b = a.clone();
        b.nlink = 2;
        b.gid = 7;
        assert!(diff(false, &a, &b, &sel(Tier::Default)).is_empty());
        let v = diff(false, &a, &b, &sel(Tier::Paranoid));
        assert_eq!(names(&v), ["gid", "nlink"]);
        assert_eq!((v[0].old.as_str(), v[0].new.as_str()), ("1000", "7"));
        assert_eq!((v[1].old.as_str(), v[1].new.as_str()), ("1", "2"));
    }

    #[test]
    fn diff_xattr_ignored_in_default() {
        let a = ent(Kind::File);
        let mut b = a.clone();
        b.xattrs = vec![(b"user.a".to_vec(), b"1".to_vec())];
        assert!(diff(false, &a, &b, &sel(Tier::Default)).is_empty());
    }

    #[test]
    fn diff_uid_only_with_require() {
        let a = ent(Kind::File);
        let mut b = a.clone();
        b.uid = 0;
        assert!(diff(false, &a, &b, &sel(Tier::Default)).is_empty());
        let s = Selection::new(Tier::Default, &[], &[Field::Uid]);
        let v = diff(false, &a, &b, &s);
        assert_eq!(
            v,
            vec![FieldDiff {
                name: "uid".into(),
                old: "1000".into(),
                new: "0".into()
            }]
        );
    }
}
