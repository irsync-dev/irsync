//! The judge: is a destination entry a state its source path really held
//! during the round? Pure rules; the caller builds the source states by
//! replaying filegen and passes them in.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use ::filegen::kind::OpKind;

use crate::snapshot::EntryKind;

/// One entry's metadata, and its content hash for a regular file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateEntry {
    pub kind: EntryKind,
    pub size: u64,
    pub mode: u32,
    pub mtime_ns: i128,
    pub hash: Option<blake3::Hash>,
    pub target: Option<Vec<u8>>,
    /// `(uid, gid)`, when known. Compared only when run as root, and only
    /// when both sides know it.
    pub owner: Option<(u32, u32)>,
}

/// A destination entry has the same shape as a source state.
pub type DestEntry = StateEntry;

/// The source path after a complete step; `entry: None` is absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// 0 = round start.
    pub step: u64,
    pub entry: Option<StateEntry>,
}

/// A step that may have been running while irsync read the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InFlight {
    pub step: u64,
    pub op: OpKind,
    pub pre: Option<StateEntry>,
    pub post: Option<StateEntry>,
    /// The destination's bytes are a state this step passes through
    /// ([`in_flight_content_ok`]).
    pub content_ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Which state or step it matched.
    Held(String),
    /// The rule it broke.
    NotHeld(String),
}

/// How a step writes a file's bytes, for a reader watching it happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Write {
    /// The file is emptied before the write (`File::create`).
    pub truncate_first: bool,
    pub offset: u64,
    pub len: u64,
}

/// Is `dest` a state the source path held? Applies the rules in order:
/// absent, directory, symlink, fifo and others, then a regular file against
/// complete states and finally against the steps in flight.
pub fn judge(
    dest: Option<&DestEntry>,
    states: &[State],
    in_flight: &[InFlight],
    is_root: bool,
) -> Verdict {
    // Complete states: each state, and each in-flight step's ends (which
    // are complete states too, whether or not the caller listed them).
    let complete = || {
        states
            .iter()
            .map(|s| (format!("state at step {}", s.step), s.entry.as_ref()))
            .chain(in_flight.iter().flat_map(|f| {
                [
                    (format!("state before step {}", f.step), f.pre.as_ref()),
                    (format!("state after step {}", f.step), f.post.as_ref()),
                ]
            }))
    };
    let owner_ok = |a: &StateEntry, b: &StateEntry| match (a.owner, b.owner) {
        (Some(x), Some(y)) if is_root => x == y,
        _ => true,
    };

    // 1. Absent.
    let Some(d) = dest else {
        return match complete().find(|(_, e)| e.is_none()) {
            Some((which, _)) => Verdict::Held(format!("absent: {which}")),
            None => Verdict::NotHeld("absent, but the source path always existed".into()),
        };
    };

    // 2-4. Anything but a regular file: some complete state of the same
    // kind with the same identity.
    if d.kind != EntryKind::File {
        let same = |s: &StateEntry| {
            s.kind == d.kind
                && owner_ok(s, d)
                && match d.kind {
                    EntryKind::Dir => s.mode == d.mode,
                    EntryKind::Symlink => s.target == d.target,
                    _ => s.mode == d.mode,
                }
        };
        return match complete().find(|(_, e)| e.is_some_and(same)) {
            Some((which, _)) => Verdict::Held(which),
            None => Verdict::NotHeld("no state the source held".into()),
        };
    }

    // 5. A regular file whose size and mtime name a complete state must
    // also have that state's mode and content.
    let mut times_match = false;
    for (which, s) in complete() {
        let Some(s) = s else { continue };
        if s.kind != EntryKind::File || s.size != d.size || s.mtime_ns != d.mtime_ns {
            continue;
        }
        times_match = true;
        if s.mode == d.mode && s.hash == d.hash && owner_ok(s, d) {
            return Verdict::Held(which);
        }
    }
    if times_match {
        return Verdict::NotHeld(
            "content and times disagree: size and mtime match a source state, \
             but mode or content do not"
                .into(),
        );
    }

    // 6. A regular file caught mid-step.
    for f in in_flight {
        if !f.content_ok {
            continue;
        }
        let ends: Vec<&StateEntry> = f.pre.iter().chain(f.post.iter()).collect();
        if ends.is_empty() {
            continue;
        }
        let mode_ok = ends.iter().any(|e| e.mode == d.mode);
        let owner_in = ends.iter().any(|e| owner_ok(e, d));
        if mode_ok && owner_in && mtime_in_window(d.mtime_ns, f) {
            return Verdict::Held(format!("in flight: step {} ({})", f.step, f.op.name()));
        }
    }

    // 7.
    Verdict::NotHeld("no state the source held".into())
}

/// Rule 6's time check. An mtime equal to `pre`'s means no write landed
/// yet. Otherwise it must be at most `post`'s, and at least `pre`'s only
/// when `pre` is not later than `post`: after a `set_mtime` into the
/// future, the next write moves the mtime backwards, so a `pre` later than
/// `post` gives no lower bound. A missing end gives no bound.
fn mtime_in_window(m: i128, f: &InFlight) -> bool {
    let pre = f.pre.as_ref().map(|p| p.mtime_ns);
    let post = f.post.as_ref().map(|p| p.mtime_ns);
    if pre == Some(m) {
        return true;
    }
    let before_post = post.is_none_or(|post| m <= post);
    let after_pre = match (pre, post) {
        (Some(pre), Some(post)) if pre <= post => pre <= m,
        (Some(pre), None) => pre <= m,
        _ => true,
    };
    before_post && after_pre
}

/// How `op` writes the file's bytes, from its logged `details` and the
/// file's size before the step.
///
/// `None` means the step is atomic for a reader: only the old or the new
/// file passes [`in_flight_content_ok`]. It is also `None` when the range
/// cannot be told, so for `append` the caller must pass `old_size` (the
/// file's size before the step): without it, every in-between state of an
/// append would be rejected.
pub fn write_of(
    op: OpKind,
    details: &BTreeMap<String, String>,
    old_size: Option<u64>,
) -> Option<Write> {
    let num = |k: &str| details.get(k)?.parse::<u64>().ok();
    match op {
        // `create_new`, or `File::create` (which empties the file), then
        // the content from the start.
        OpKind::CreateFile | OpKind::Rewrite => Some(Write {
            truncate_first: true,
            offset: 0,
            len: num("size")?,
        }),
        // Written at the old end.
        OpKind::Append => {
            let old = old_size?;
            Some(Write {
                truncate_first: false,
                offset: old,
                len: num("size")?.checked_sub(old)?,
            })
        }
        OpKind::OverwriteRange => Some(Write {
            truncate_first: false,
            offset: num("offset")?,
            len: num("len")?,
        }),
        // Only the emulated punch writes zeros piece by piece; fallocate
        // and `set_len` ("extend") are one call each.
        OpKind::PunchHole
            if details.get("emulated").map(String::as_str) == Some("1")
                && details.get("mode").map(String::as_str) != Some("extend") =>
        {
            Some(Write {
                truncate_first: false,
                offset: num("offset")?,
                len: num("len")?,
            })
        }
        _ => None,
    }
}

const CHUNK: usize = 1 << 20;

/// Does `dest` hold bytes the file passes through while the step runs?
///
/// With a write `w`: the base B is `old`, or empty if `w.truncate_first` or
/// there is no old file. `dest` passes if it is B with `new`'s bytes
/// `[offset, offset + k)` written over it, for some `k` in `0..=len`. With
/// no write, `dest` must equal `old` or `new`. Files are compared in 1 MiB
/// chunks, never loaded whole.
pub fn in_flight_content_ok(
    dest: &Path,
    old: Option<&Path>,
    new: &Path,
    w: Option<&Write>,
) -> io::Result<bool> {
    let d = Src::open(dest)?;
    let n = Src::open(new)?;
    let o = old.map(Src::open).transpose()?;
    let Some(w) = w else {
        return Ok(same(&d, &n)? || o.as_ref().map_or(Ok(false), |o| same(&d, o))?);
    };
    let base = if w.truncate_first { None } else { o.as_ref() };
    let bl = base.map_or(0, |b| b.len);
    let (off, dl) = (w.offset, d.len);

    // p: how far dest already agrees with the new bytes from `offset`. If
    // dest is B with the first k bytes written, it is also B with the first
    // p written: between k and p, dest equals both B and the new bytes.
    let p = {
        let span = w
            .len
            .min(dl.saturating_sub(off))
            .min(n.len.saturating_sub(off));
        common_prefix(&d, off, Some(&n), off, span)?
    };
    // Writing k > 0 bytes makes the file max(B, offset + k) long.
    let k = match dl.cmp(&bl) {
        std::cmp::Ordering::Less => return Ok(false),
        std::cmp::Ordering::Equal => p,
        std::cmp::Ordering::Greater => match dl.checked_sub(off) {
            Some(k) if k > 0 && k <= p => k,
            _ => return Ok(false),
        },
    };
    // Outside [offset, offset + k), dest must be B (zeros past its end).
    let around = [(0, off.min(dl)), ((off + k).min(dl), dl)];
    for (start, end) in around {
        if !matches_base(&d, base, bl, start, end)? {
            return Ok(false);
        }
    }
    Ok(true)
}

struct Src {
    f: std::fs::File,
    len: u64,
}

impl Src {
    fn open(p: &Path) -> io::Result<Src> {
        let f = std::fs::File::open(p)?;
        let len = f.metadata()?.len();
        Ok(Src { f, len })
    }
}

fn same(a: &Src, b: &Src) -> io::Result<bool> {
    Ok(a.len == b.len && common_prefix(a, 0, Some(b), 0, a.len)? == a.len)
}

/// `dest[start, end)` equals the base there, reading zeros past its end.
fn matches_base(d: &Src, base: Option<&Src>, bl: u64, start: u64, end: u64) -> io::Result<bool> {
    let mid = end.min(bl).max(start);
    let in_base = mid - start;
    if in_base > 0 && common_prefix(d, start, base, start, in_base)? != in_base {
        return Ok(false);
    }
    let tail = end.saturating_sub(mid);
    Ok(tail == 0 || common_prefix(d, mid, None, 0, tail)? == tail)
}

/// How many leading bytes of `a[a_off..]` and `b[b_off..]` agree, up to
/// `len`; `b: None` reads as zeros.
fn common_prefix(a: &Src, a_off: u64, b: Option<&Src>, b_off: u64, len: u64) -> io::Result<u64> {
    use std::os::unix::fs::FileExt;
    let cap = usize::try_from(len).unwrap_or(CHUNK).min(CHUNK);
    let mut ba = vec![0u8; cap];
    let mut bb = vec![0u8; cap];
    let mut done = 0u64;
    while done < len {
        let n = usize::try_from(len - done).unwrap_or(CHUNK).min(CHUNK);
        a.f.read_exact_at(&mut ba[..n], a_off + done)?;
        if let Some(b) = b {
            b.f.read_exact_at(&mut bb[..n], b_off + done)?;
        }
        if let Some(i) = ba[..n].iter().zip(&bb[..n]).position(|(x, y)| x != y) {
            return Ok(done + i as u64);
        }
        done += n as u64;
    }
    Ok(len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn hash(b: &[u8]) -> Option<blake3::Hash> {
        Some(blake3::hash(b))
    }

    fn file(bytes: &[u8], mode: u32, mtime_ns: i128) -> StateEntry {
        StateEntry {
            kind: EntryKind::File,
            size: bytes.len() as u64,
            mode,
            mtime_ns,
            hash: hash(bytes),
            target: None,
            owner: None,
        }
    }

    fn dir(mode: u32) -> StateEntry {
        StateEntry {
            kind: EntryKind::Dir,
            size: 0,
            mode,
            mtime_ns: 1,
            hash: None,
            target: None,
            owner: None,
        }
    }

    fn state(step: u64, entry: Option<StateEntry>) -> State {
        State { step, entry }
    }

    fn held(v: &Verdict) -> bool {
        matches!(v, Verdict::Held(_))
    }

    /// Writes `old`, `new` and each candidate destination into a temp dir
    /// and asks [`in_flight_content_ok`] about each candidate.
    struct Files {
        tmp: TempDir,
    }

    impl Files {
        fn new(old: Option<&[u8]>, new: &[u8]) -> Files {
            let tmp = TempDir::new().unwrap();
            if let Some(old) = old {
                fs::write(tmp.path().join("old"), old).unwrap();
            }
            fs::write(tmp.path().join("new"), new).unwrap();
            Files { tmp }
        }

        fn ok(&self, dest: &[u8], w: Option<&Write>) -> bool {
            let d = self.tmp.path().join("dest");
            fs::write(&d, dest).unwrap();
            let old = self.tmp.path().join("old");
            let old = old.exists().then_some(old);
            in_flight_content_ok(&d, old.as_deref(), &self.tmp.path().join("new"), w).unwrap()
        }
    }

    fn details(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn rewrite_prefix_is_held() {
        let f = Files::new(Some(b"AAAA"), b"BBBBBB");
        let w = write_of(OpKind::Rewrite, &details(&[("size", "6")]), Some(4)).unwrap();
        assert_eq!(
            w,
            Write {
                truncate_first: true,
                offset: 0,
                len: 6
            }
        );
        assert!(f.ok(b"BBB", Some(&w)));
        assert!(f.ok(b"", Some(&w)));
        assert!(f.ok(b"BBBBBB", Some(&w)));
        assert!(!f.ok(b"BBBA", Some(&w)));
        assert!(!f.ok(b"AAAA", Some(&w)));
    }

    #[test]
    fn append_partial_is_held() {
        let f = Files::new(Some(b"AAAA"), b"AAAABBBB");
        let w = write_of(OpKind::Append, &details(&[("size", "8")]), Some(4)).unwrap();
        assert_eq!(
            w,
            Write {
                truncate_first: false,
                offset: 4,
                len: 4
            }
        );
        assert!(f.ok(b"AAAABB", Some(&w)));
        assert!(f.ok(b"AAAA", Some(&w)));
        assert!(f.ok(b"AAAABBBB", Some(&w)));
        assert!(!f.ok(b"AAABBB", Some(&w)));
        assert!(!f.ok(b"AAA", Some(&w)));
    }

    #[test]
    fn create_partial_is_held() {
        let f = Files::new(None, b"CCCC");
        let w = write_of(OpKind::CreateFile, &details(&[("size", "4")]), None).unwrap();
        assert!(f.ok(b"", Some(&w)));
        assert!(f.ok(b"CC", Some(&w)));
        assert!(f.ok(b"CCCC", Some(&w)));
        assert!(!f.ok(b"CD", Some(&w)));
    }

    #[test]
    fn overwrite_front_part_is_held() {
        let f = Files::new(Some(b"AAAAAAAA"), b"AABBBBAA");
        let w = write_of(
            OpKind::OverwriteRange,
            &details(&[("offset", "2"), ("len", "4"), ("content", "random")]),
            Some(8),
        )
        .unwrap();
        assert_eq!(
            w,
            Write {
                truncate_first: false,
                offset: 2,
                len: 4
            }
        );
        assert!(f.ok(b"AABBAAAA", Some(&w)));
        assert!(f.ok(b"AAAAAAAA", Some(&w)));
        assert!(f.ok(b"AABBBBAA", Some(&w)));
        // The back part written first: not how the write goes.
        assert!(!f.ok(b"AAAABBAA", Some(&w)));
        // Right bytes, wrong length.
        assert!(!f.ok(b"AABBAA", Some(&w)));
    }

    #[test]
    fn overwrite_where_old_and_new_agree() {
        // The range's first byte is already 'B' in the old file, so a
        // longest match runs past the real write point; still held.
        let f = Files::new(Some(b"AABAAAAA"), b"AABBBBAA");
        let w = Write {
            truncate_first: false,
            offset: 2,
            len: 4,
        };
        assert!(f.ok(b"AABAAAAA", Some(&w)));
        assert!(f.ok(b"AABBAAAA", Some(&w)));
        assert!(!f.ok(b"AABABAAA", Some(&w)));
    }

    #[test]
    fn content_spans_many_chunks() {
        let mib = 1usize << 20;
        let old = vec![b'A'; 3 * mib];
        let new = vec![b'B'; 3 * mib + 10];
        let f = Files::new(Some(&old), &new);
        let w = Write {
            truncate_first: true,
            offset: 0,
            len: new.len() as u64,
        };
        let mut d = vec![b'B'; 2 * mib + 7];
        assert!(f.ok(&d, Some(&w)));
        d[mib + 3] = b'A';
        assert!(!f.ok(&d, Some(&w)));
        let w = Write {
            truncate_first: false,
            offset: mib as u64,
            len: (2 * mib) as u64,
        };
        let mut d = old.clone();
        d[mib..mib + mib / 2].fill(b'B');
        assert!(f.ok(&d, Some(&w)));
        d[2 * mib + 5] = b'B';
        assert!(!f.ok(&d, Some(&w)));
    }

    #[test]
    fn atomic_step_is_old_or_new() {
        let f = Files::new(Some(b"AAAA"), b"AA");
        assert!(f.ok(b"AAAA", None));
        assert!(f.ok(b"AA", None));
        assert!(!f.ok(b"AAA", None));
        let f = Files::new(None, b"AA");
        assert!(f.ok(b"AA", None));
        assert!(!f.ok(b"", None));
    }

    #[test]
    fn punch_hole_zero_writes_are_sequential() {
        let f = Files::new(Some(b"AAAAAAAA"), b"AA\0\0\0\0AA");
        let d = details(&[("offset", "2"), ("len", "4"), ("emulated", "1")]);
        let w = write_of(OpKind::PunchHole, &d, Some(8)).unwrap();
        assert!(f.ok(b"AA\0\0AAAA", Some(&w)));
        assert!(!f.ok(b"AAAA\0\0AA", Some(&w)));
    }

    #[test]
    fn write_of_atomic_steps_is_none() {
        let none = details(&[]);
        assert_eq!(
            write_of(OpKind::Truncate, &details(&[("size", "3")]), Some(8)),
            None
        );
        assert_eq!(
            write_of(
                OpKind::PunchHole,
                &details(&[("offset", "2"), ("len", "4")]),
                Some(8)
            ),
            None
        );
        assert_eq!(
            write_of(
                OpKind::PunchHole,
                &details(&[
                    ("offset", "8"),
                    ("len", "4"),
                    ("mode", "extend"),
                    ("size", "12")
                ]),
                Some(8)
            ),
            None
        );
        for op in [
            OpKind::Chmod,
            OpKind::SetMtime,
            OpKind::Rename,
            OpKind::RenameOver,
            OpKind::Chown,
            OpKind::SetXattr,
            OpKind::Hardlink,
        ] {
            assert_eq!(write_of(op, &none, Some(8)), None, "{op:?}");
        }
        // Append with no known old size cannot place its range.
        assert_eq!(
            write_of(OpKind::Append, &details(&[("size", "8")]), None),
            None
        );
    }

    #[test]
    fn two_steps_never_mix() {
        let s = [
            file(b"AAAA", 0o644, 100),
            file(b"BBBB", 0o644, 200),
            file(b"CCCC", 0o644, 300),
        ];
        let states = [
            state(0, Some(s[0].clone())),
            state(1, Some(s[1].clone())),
            state(2, Some(s[2].clone())),
        ];
        let w = write_of(OpKind::Rewrite, &details(&[("size", "4")]), Some(4)).unwrap();
        let d = b"BBCC";
        let step1 = Files::new(Some(b"AAAA"), b"BBBB").ok(d, Some(&w));
        let step2 = Files::new(Some(b"BBBB"), b"CCCC").ok(d, Some(&w));
        assert!(!step1 && !step2);
        let in_flight = [
            InFlight {
                step: 1,
                op: OpKind::Rewrite,
                pre: Some(s[0].clone()),
                post: Some(s[1].clone()),
                content_ok: step1,
            },
            InFlight {
                step: 2,
                op: OpKind::Rewrite,
                pre: Some(s[1].clone()),
                post: Some(s[2].clone()),
                content_ok: step2,
            },
        ];
        let dest = file(d, 0o644, 250);
        let v = judge(Some(&dest), &states, &in_flight, false);
        assert!(!held(&v), "{v:?}");
    }

    #[test]
    fn times_say_current_bytes_old_fails() {
        let s1 = file(b"BBBB", 0o644, 200);
        let s2 = file(b"CCCC", 0o644, 300);
        let states = [state(1, Some(s1.clone())), state(2, Some(s2.clone()))];
        let mut dest = s2.clone();
        dest.hash = s1.hash;
        let v = judge(Some(&dest), &states, &[], false);
        match v {
            Verdict::NotHeld(why) => assert!(why.contains("content and times disagree"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn complete_state_is_held() {
        let s1 = file(b"BBBB", 0o644, 200);
        let s2 = file(b"CCCC", 0o600, 300);
        let states = [state(1, Some(s1)), state(2, Some(s2.clone()))];
        assert!(held(&judge(Some(&s2), &states, &[], false)));
        // Same content and times, a mode never held.
        let mut d = s2.clone();
        d.mode = 0o640;
        assert!(!held(&judge(Some(&d), &states, &[], false)));
    }

    #[test]
    fn in_flight_inside_window_is_held() {
        let pre = file(b"AAAA", 0o644, 100);
        let post = file(b"BBBBBB", 0o600, 200);
        let states = [state(0, Some(pre.clone())), state(1, Some(post.clone()))];
        let fl = |content_ok| InFlight {
            step: 1,
            op: OpKind::Rewrite,
            pre: Some(pre.clone()),
            post: Some(post.clone()),
            content_ok,
        };
        let dest = file(b"BBB", 0o644, 150);
        assert!(held(&judge(Some(&dest), &states, &[fl(true)], false)));
        assert!(!held(&judge(Some(&dest), &states, &[fl(false)], false)));
        let mut odd = dest.clone();
        odd.mode = 0o640;
        assert!(!held(&judge(Some(&odd), &states, &[fl(true)], false)));
        // Bounds are inclusive.
        let mut edge = dest.clone();
        edge.mtime_ns = 100;
        assert!(held(&judge(Some(&edge), &states, &[fl(true)], false)));
        // No pre: no lower bound.
        let created = InFlight {
            step: 1,
            op: OpKind::CreateFile,
            pre: None,
            post: Some(post.clone()),
            content_ok: true,
        };
        let early = file(b"BB", 0o600, 5);
        assert!(held(&judge(Some(&early), &states, &[created], false)));
    }

    #[test]
    fn future_pre_mtime_does_not_reject_in_between() {
        let sec: i128 = 1_000_000_000;
        let now = 1_800_000_000 * sec;
        let pre = file(b"AAAA", 0o644, now + 365 * 24 * 3600 * sec);
        let post = file(b"BBBBBB", 0o644, now);
        let states = [state(0, Some(pre.clone())), state(1, Some(post.clone()))];
        let fl = InFlight {
            step: 1,
            op: OpKind::Rewrite,
            pre: Some(pre.clone()),
            post: Some(post),
            content_ok: true,
        };
        let fl = std::slice::from_ref(&fl);
        let dest = file(b"BBB", 0o644, now - sec);
        assert!(held(&judge(Some(&dest), &states, fl, false)));
        let dest = file(b"BBB", 0o644, now + sec);
        assert!(!held(&judge(Some(&dest), &states, fl, false)));
        // Still at pre's mtime: no write landed yet.
        let dest = file(b"AAAA", 0o644, pre.mtime_ns);
        assert!(held(&judge(Some(&dest), &states, fl, false)));
    }

    #[test]
    fn content_and_times_rule_comes_before_in_flight() {
        let pre = file(b"AAAAAA", 0o644, 100);
        let post = file(b"BBBBBB", 0o644, 200);
        let states = [state(0, Some(pre.clone())), state(1, Some(post.clone()))];
        let fl = InFlight {
            step: 1,
            op: OpKind::OverwriteRange,
            pre: Some(pre),
            post: Some(post.clone()),
            content_ok: true,
        };
        let mut dest = post.clone();
        dest.hash = hash(b"BBBAAA");
        match judge(Some(&dest), &states, &[fl], false) {
            Verdict::NotHeld(why) => assert!(why.contains("content and times disagree"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn mtime_outside_window_fails() {
        let pre = file(b"AAAA", 0o644, 100);
        let post = file(b"BBBBBB", 0o644, 200);
        let states = [state(0, Some(pre.clone())), state(1, Some(post.clone()))];
        let fl = InFlight {
            step: 1,
            op: OpKind::Rewrite,
            pre: Some(pre),
            post: Some(post),
            content_ok: true,
        };
        let dest = file(b"BBB", 0o644, 201);
        assert!(!held(&judge(
            Some(&dest),
            &states,
            std::slice::from_ref(&fl),
            false
        )));
        let dest = file(b"BBB", 0o644, 99);
        assert!(!held(&judge(Some(&dest), &states, &[fl], false)));
    }

    #[test]
    fn absent_held_when_created_mid_round() {
        let states = [state(0, None), state(3, Some(file(b"x", 0o644, 1)))];
        assert!(held(&judge(None, &states, &[], false)));
        // Never absent: not held.
        let states = [state(0, Some(file(b"x", 0o644, 1)))];
        assert!(!held(&judge(None, &states, &[], false)));
        // Absent only as an in-flight step's end.
        let fl = InFlight {
            step: 1,
            op: OpKind::DeleteFile,
            pre: Some(file(b"x", 0o644, 1)),
            post: None,
            content_ok: false,
        };
        assert!(held(&judge(None, &states, &[fl], false)));
    }

    #[test]
    fn dir_mode_must_have_been_held() {
        let states = [state(0, Some(dir(0o755))), state(2, Some(dir(0o700)))];
        assert!(!held(&judge(Some(&dir(0o711)), &states, &[], false)));
        let mut d = dir(0o700);
        d.mtime_ns = 999; // mtime is ignored
        assert!(held(&judge(Some(&d), &states, &[], false)));
        // A file where only directories were held.
        let f = file(b"", 0o700, 1);
        assert!(!held(&judge(Some(&f), &states, &[], false)));
    }

    #[test]
    fn symlink_target_must_match() {
        let link = |t: &[u8]| StateEntry {
            kind: EntryKind::Symlink,
            size: t.len() as u64,
            mode: 0o777,
            mtime_ns: 1,
            hash: None,
            target: Some(t.to_vec()),
            owner: None,
        };
        let states = [state(0, Some(link(b"a"))), state(1, Some(link(b"bb")))];
        assert!(held(&judge(Some(&link(b"bb")), &states, &[], false)));
        assert!(!held(&judge(Some(&link(b"c")), &states, &[], false)));
    }

    #[test]
    fn fifo_kind_and_mode_must_match() {
        let fifo = |mode| StateEntry {
            kind: EntryKind::Fifo,
            size: 0,
            mode,
            mtime_ns: 1,
            hash: None,
            target: None,
            owner: None,
        };
        let states = [state(0, Some(fifo(0o644)))];
        assert!(held(&judge(Some(&fifo(0o644)), &states, &[], false)));
        assert!(!held(&judge(Some(&fifo(0o600)), &states, &[], false)));
        let mut other = fifo(0o644);
        other.kind = EntryKind::Other;
        assert!(!held(&judge(Some(&other), &states, &[], false)));
    }

    #[test]
    fn owner_checked_only_as_root() {
        let mut s = file(b"x", 0o644, 1);
        s.owner = Some((1000, 1000));
        let states = [state(0, Some(s.clone()))];
        let mut d = s.clone();
        d.owner = Some((0, 0));
        assert!(held(&judge(Some(&d), &states, &[], false)));
        assert!(!held(&judge(Some(&d), &states, &[], true)));
        assert!(held(&judge(Some(&s), &states, &[], true)));
        let mut dd = dir(0o755);
        dd.owner = Some((1, 1));
        let mut sd = dir(0o755);
        sd.owner = Some((2, 2));
        assert!(!held(&judge(Some(&dd), &[state(0, Some(sd))], &[], true)));
    }

    #[test]
    fn nothing_matches_says_so() {
        let states = [state(0, Some(file(b"AAAA", 0o644, 100)))];
        let dest = file(b"ZZ", 0o644, 50);
        assert_eq!(
            judge(Some(&dest), &states, &[], false),
            Verdict::NotHeld("no state the source held".into())
        );
    }
}
