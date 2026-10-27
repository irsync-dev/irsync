//! Pure planning logic: per-path decisions and the sorted merge-join of two listings.

use futures::stream::{self, Stream, StreamExt};
use irsync_proto::chunk::FileDigest;
use irsync_proto::v1::{file_entry::Kind, FileEntry};

use crate::cli::{Append, Preserve};
use crate::fs::paths::RelPath;

/// One path's view on one side: its listing entry and, when known, its digest.
#[derive(Debug, Clone)]
pub struct Side {
    pub entry: FileEntry,
    pub digest: Option<FileDigest>,
}

/// What to do for one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Skip,
    MetaOnly,
    Mkdir,
    Symlink,
    Special,
    Whole,
    Delta,
    /// `--append`: the destination holds the first `from` bytes, trusted;
    /// only the tail is transferred.
    Append {
        from: u64,
    },
    LocalCopy {
        from: RelPath,
    },
    /// Link to the earlier listed regular file `target` (`-H`), once every
    /// regular file is placed.
    Hardlink {
        target: RelPath,
    },
    Delete,
}

#[derive(Debug, Clone)]
pub struct PlanOpts {
    pub checksum: bool,
    pub update_only: bool,
    pub delete: bool,
    pub rename_detection: bool,
    pub preserve: Preserve,
    pub append: Append,
}

/// Finds a destination file already holding content with the given whole-file hash.
pub trait HashLookup {
    fn path_with_whole_hash(&self, h: &[u8; 32]) -> Option<RelPath>;
}

/// A lookup that never hits.
pub struct NoLookup;

impl HashLookup for NoLookup {
    fn path_with_whole_hash(&self, _h: &[u8; 32]) -> Option<RelPath> {
        None
    }
}

fn kind_of(e: &FileEntry) -> Option<Kind> {
    let k = Kind::try_from(e.kind).ok();
    if k.is_none() {
        tracing::warn!(kind = e.kind, "unknown entry kind; skipping");
    }
    k
}

fn mtime(e: &FileEntry) -> (i64, u32) {
    (e.mtime_sec, e.mtime_nsec)
}

/// Whether the metadata selected by `p` differs between the two entries.
/// Whether the attributes `p` preserves differ between `src` and `dst`.
pub fn meta_differs(src: &FileEntry, dst: &FileEntry, p: &Preserve) -> bool {
    (p.perms && src.mode & 0o7777 != dst.mode & 0o7777)
        || (p.times && mtime(src) != mtime(dst))
        || (p.owner && src.uid != dst.uid)
        || (p.group && src.gid != dst.gid)
}

fn skip_or_meta(src: &FileEntry, dst: &FileEntry, p: &Preserve) -> Decision {
    if meta_differs(src, dst, p) {
        Decision::MetaOnly
    } else {
        Decision::Skip
    }
}

/// Whether a device node or special file of `kind` is transferred at all:
/// rsync's `--devices` covers the device nodes, `--specials` the rest.
fn wants_special(kind: Kind, p: &Preserve) -> bool {
    match kind {
        Kind::CharDev | Kind::BlockDev => p.devices,
        Kind::Fifo | Kind::Socket => p.specials,
        _ => false,
    }
}

/// Decision for a source entry that has to be created (no usable destination entry).
fn create(src: &Side, opts: &PlanOpts, lookup: Option<&dyn HashLookup>) -> Decision {
    match kind_of(&src.entry) {
        Some(Kind::Directory) => Decision::Mkdir,
        Some(Kind::Symlink) => Decision::Symlink,
        Some(k @ (Kind::CharDev | Kind::BlockDev | Kind::Fifo | Kind::Socket)) => {
            if wants_special(k, &opts.preserve) {
                Decision::Special
            } else {
                Decision::Skip
            }
        }
        Some(Kind::Hardlink) => match RelPath::new(&src.entry.link_target) {
            Ok(target) if !target.as_bytes().is_empty() => Decision::Hardlink { target },
            _ => {
                tracing::warn!(
                    target = %String::from_utf8_lossy(&src.entry.link_target),
                    "hard link with an invalid target; skipping"
                );
                Decision::Skip
            }
        },
        Some(Kind::Regular) => {
            if opts.rename_detection {
                if let (Some(d), Some(lookup)) = (&src.digest, lookup) {
                    if let Some(from) = lookup.path_with_whole_hash(&d.hash) {
                        return Decision::LocalCopy { from };
                    }
                }
            }
            Decision::Whole
        }
        None => Decision::Skip,
    }
}

pub fn decide(
    src: Option<&Side>,
    dst: Option<&Side>,
    opts: &PlanOpts,
    lookup: &dyn HashLookup,
) -> Decision {
    let (src, dst) = match (src, dst) {
        (None, None) => return Decision::Skip,
        (None, Some(_)) => {
            return if opts.delete {
                Decision::Delete
            } else {
                Decision::Skip
            }
        }
        (Some(s), None) => return create(s, opts, Some(lookup)),
        (Some(s), Some(d)) => (s, d),
    };
    let (se, de) = (&src.entry, &dst.entry);
    let (sk, dk) = (kind_of(se), kind_of(de));
    if sk != dk {
        return create(src, opts, None);
    }
    let p = &opts.preserve;
    match sk {
        Some(Kind::Directory) => skip_or_meta(se, de, p),
        Some(Kind::Symlink) => {
            if se.link_target == de.link_target && !meta_differs(se, de, p) {
                Decision::Skip
            } else {
                Decision::Symlink
            }
        }
        Some(k @ (Kind::CharDev | Kind::BlockDev | Kind::Fifo | Kind::Socket)) => {
            if !wants_special(k, p) {
                Decision::Skip
            } else if se.rdev != de.rdev {
                Decision::Special
            } else {
                // Same node: mode, times and ownership are fixed in place.
                skip_or_meta(se, de, p)
            }
        }
        Some(Kind::Regular) => {
            if opts.update_only && mtime(de) > mtime(se) {
                return Decision::Skip;
            }
            let hashes_equal = match (&src.digest, &dst.digest) {
                (Some(a), Some(b)) => Some(a.hash == b.hash),
                _ => None,
            };
            let quick_equal = se.size == de.size
                && mtime(se) == mtime(de)
                && (!opts.checksum || hashes_equal == Some(true));
            if quick_equal {
                skip_or_meta(se, de, p)
            } else if hashes_equal == Some(true) {
                Decision::MetaOnly
            } else {
                match opts.append {
                    Append::Off => Decision::Delta,
                    // rsync: a destination not shorter than the source is skipped.
                    _ if se.size <= de.size => skip_or_meta(se, de, p),
                    Append::Append => Decision::Append { from: de.size },
                    Append::Verify => Decision::Delta,
                }
            }
        }
        // Only ever in the sender's listing; whether the destination already
        // shares the target's inode is for the receiver's planner to see.
        Some(Kind::Hardlink) => create(src, opts, None),
        None => Decision::Skip,
    }
}

type Item = (RelPath, Side);

/// Next side whose path validates; invalid ones are skipped with a warning.
async fn next_valid<S: Stream<Item = Side> + Unpin>(s: &mut S, which: &str) -> Option<Item> {
    while let Some(side) = s.next().await {
        match RelPath::new(&side.entry.path) {
            Ok(p) => return Some((p, side)),
            Err(_) => tracing::warn!(
                side = which,
                path = %String::from_utf8_lossy(&side.entry.path),
                "skipping entry with invalid path"
            ),
        }
    }
    None
}

struct MergeState<A, B> {
    src: A,
    dst: B,
    sh: Option<Item>,
    dh: Option<Item>,
    primed: bool,
    last: Option<RelPath>,
}

/// Merges two listings sorted by `RelPath` into aligned `(path, src, dst)` triples.
pub fn merge_join(
    src: impl Stream<Item = Side>,
    dst: impl Stream<Item = Side>,
) -> impl Stream<Item = (RelPath, Option<Side>, Option<Side>)> {
    let state = MergeState {
        src: Box::pin(src),
        dst: Box::pin(dst),
        sh: None,
        dh: None,
        primed: false,
        last: None,
    };
    stream::unfold(state, |mut st| async move {
        if !st.primed {
            st.sh = next_valid(&mut st.src, "src").await;
            st.dh = next_valid(&mut st.dst, "dst").await;
            st.primed = true;
        }
        loop {
            let out = match (st.sh.take(), st.dh.take()) {
                (None, None) => return None,
                (Some((p, s)), None) => {
                    st.sh = next_valid(&mut st.src, "src").await;
                    (p, Some(s), None)
                }
                (None, Some((p, d))) => {
                    st.dh = next_valid(&mut st.dst, "dst").await;
                    (p, None, Some(d))
                }
                (Some((sp, s)), Some((dp, d))) => match sp.cmp(&dp) {
                    std::cmp::Ordering::Less => {
                        st.dh = Some((dp, d));
                        st.sh = next_valid(&mut st.src, "src").await;
                        (sp, Some(s), None)
                    }
                    std::cmp::Ordering::Greater => {
                        st.sh = Some((sp, s));
                        st.dh = next_valid(&mut st.dst, "dst").await;
                        (dp, None, Some(d))
                    }
                    std::cmp::Ordering::Equal => {
                        st.sh = next_valid(&mut st.src, "src").await;
                        st.dh = next_valid(&mut st.dst, "dst").await;
                        (sp, Some(s), Some(d))
                    }
                },
            };
            if st.last.as_ref().is_some_and(|last| out.0 <= *last) {
                // Duplicate or out-of-order key: a listing bug. Drop it rather than
                // let it pair against the wrong counterpart (e.g. spurious Delete).
                tracing::error!(
                    path = ?out.0,
                    previous = ?st.last,
                    "merge_join input duplicate or out of order; dropping item"
                );
                continue;
            }
            st.last = Some(out.0.clone());
            return Some((out, st));
        }
    })
}

/// The `-i` code of a deletion (11 characters).
pub const DELETING: &str = "*deleting  ";

/// rsync's `-i` code for one planned item: `YXcstpoguax` (11 characters).
///
/// `Y` is `dir` (`>` received by this host, `<` sent from it) for a data
/// transfer, `c` for a local creation (directory, symlink, special), `h` for
/// a hard link and `.` when only attributes change. `X` is the file type.
/// A new item has `+` in every attribute position; otherwise a letter marks
/// each attribute that differs between `src` and `dst` (`c` content, `s`
/// size, `t` mtime, `p` permissions, `o` owner, `g` group; `u a x` stay
/// dots as irsync has no atimes or ACLs). `c` is only shown when size and
/// mtime match but the content (checksum) differs. A letter for `t p o g`
/// only shows when `preserve` asks for that attribute. A deletion is
/// [`DELETING`].
pub fn itemize(
    decision: &Decision,
    src: Option<&Side>,
    dst: Option<&Side>,
    dir: char,
    preserve: &Preserve,
) -> String {
    if *decision == Decision::Delete {
        return DELETING.to_string();
    }
    let kind = src.and_then(|s| kind_of(&s.entry));
    let letter = match kind {
        Some(Kind::Directory) => 'd',
        Some(Kind::Symlink) => 'L',
        Some(Kind::CharDev | Kind::BlockDev) => 'D',
        Some(Kind::Fifo | Kind::Socket) => 'S',
        _ => 'f',
    };
    let update = match decision {
        Decision::Whole
        | Decision::Delta
        | Decision::Append { .. }
        | Decision::LocalCopy { .. } => dir,
        Decision::Hardlink { .. } => 'h',
        Decision::Mkdir | Decision::Symlink | Decision::Special => 'c',
        Decision::MetaOnly | Decision::Skip | Decision::Delete => '.',
    };
    let attrs = match (src, dst) {
        (Some(s), Some(d)) if !matches!(decision, Decision::Hardlink { .. }) => {
            let (se, de) = (&s.entry, &d.entry);
            let regular = kind == Some(Kind::Regular);
            let flag = |on: bool, ch: char| if on { ch } else { '.' };
            [
                flag(
                    regular
                        && *decision == Decision::Delta
                        && se.size == de.size
                        && mtime(se) == mtime(de),
                    'c',
                ),
                flag(regular && se.size != de.size, 's'),
                flag(preserve.times && mtime(se) != mtime(de), 't'),
                flag(preserve.perms && se.mode & 0o7777 != de.mode & 0o7777, 'p'),
                flag(preserve.owner && se.uid != de.uid, 'o'),
                flag(preserve.group && se.gid != de.gid, 'g'),
                '.',
                '.',
                '.',
            ]
            .iter()
            .collect::<String>()
        }
        _ => "+++++++++".to_string(),
    };
    format!("{update}{letter}{attrs}")
}
