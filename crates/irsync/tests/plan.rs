use futures::executor::block_on;
use futures::stream::{self, StreamExt};
use irsync::cli::Append;
use irsync::cli::Preserve;
use irsync::plan::{decide, merge_join, Decision, PlanOpts, Side};
use irsync_proto::chunk::FileDigest;
use irsync_proto::v1::{file_entry::Kind, FileEntry};

fn entry(path: &str, kind: Kind, size: u64, mtime: i64, mode: u32) -> FileEntry {
    FileEntry {
        path: path.as_bytes().to_vec(),
        kind: kind as i32,
        size,
        mtime_sec: mtime,
        mode,
        ..Default::default()
    }
}

fn side(path: &str, kind: Kind, mode: u32) -> Side {
    Side {
        entry: entry(path, kind, 0, 1, mode),
        digest: None,
    }
}

fn reg(path: &str, size: u64, mtime: i64) -> Side {
    Side {
        entry: entry(path, Kind::Regular, size, mtime, 0o644),
        digest: None,
    }
}

fn with_hash(mut s: Side, h: u8) -> Side {
    s.digest = Some(FileDigest {
        size: s.entry.size,
        hash: [h; 32],
        chunks: vec![],
    });
    s
}

fn opts() -> PlanOpts {
    PlanOpts {
        checksum: false,
        update_only: false,
        delete: false,
        preserve: Preserve::default(),
        append: Append::Off,
    }
}

fn append_opts(append: Append) -> PlanOpts {
    PlanOpts { append, ..opts() }
}

#[test]
fn append_transfers_only_the_tail_of_a_grown_file() {
    let o = append_opts(Append::Append);
    let src = reg("a", 200, 2);
    assert_eq!(
        decide(Some(&src), Some(&reg("a", 100, 1)), &o),
        Decision::Append { from: 100 }
    );
    // Not grown: skipped (attributes still applied), as rsync does.
    assert!(matches!(
        decide(Some(&src), Some(&reg("a", 200, 1)), &o),
        Decision::Skip | Decision::MetaOnly
    ));
    assert!(matches!(
        decide(Some(&src), Some(&reg("a", 300, 1)), &o),
        Decision::Skip | Decision::MetaOnly
    ));
    // A missing destination, or content known equal, is unaffected.
    assert_eq!(decide(Some(&src), None, &o), Decision::Whole);
    assert_eq!(
        decide(
            Some(&with_hash(reg("a", 200, 2), 1)),
            Some(&with_hash(reg("a", 200, 1), 1)),
            &o
        ),
        Decision::MetaOnly
    );
}

#[test]
fn append_verify_keeps_the_delta_for_a_grown_file() {
    let o = append_opts(Append::Verify);
    let src = reg("a", 200, 2);
    assert_eq!(
        decide(Some(&src), Some(&reg("a", 100, 1)), &o),
        Decision::Delta
    );
    assert!(matches!(
        decide(Some(&src), Some(&reg("a", 200, 1)), &o),
        Decision::Skip | Decision::MetaOnly
    ));
}

#[test]
fn missing_dst_is_whole() {
    let s = reg("a", 3, 1);
    assert_eq!(decide(Some(&s), None, &opts()), Decision::Whole);
}

#[test]
fn missing_dst_by_kind() {
    let d = side("d", Kind::Directory, 0o755);
    let l = side("l", Kind::Symlink, 0o777);
    let f = side("f", Kind::Fifo, 0o644);
    assert_eq!(decide(Some(&d), None, &opts()), Decision::Mkdir);
    assert_eq!(decide(Some(&l), None, &opts()), Decision::Symlink);
    let mut o = opts();
    assert_eq!(decide(Some(&f), None, &o), Decision::Skip);
    // A FIFO is a special, not a device: --devices alone leaves it out.
    o.preserve.devices = true;
    assert_eq!(decide(Some(&f), None, &o), Decision::Skip);
    o.preserve.specials = true;
    assert_eq!(decide(Some(&f), None, &o), Decision::Special);
    // And --specials alone leaves a device node out.
    let c = side("c", Kind::CharDev, 0o600);
    o.preserve.devices = false;
    assert_eq!(decide(Some(&c), None, &o), Decision::Skip);
    o.preserve.devices = true;
    assert_eq!(decide(Some(&c), None, &o), Decision::Special);
}

#[test]
fn equal_quick_check_skips() {
    let (s, d) = (reg("a", 3, 1), reg("a", 3, 1));
    assert_eq!(decide(Some(&s), Some(&d), &opts()), Decision::Skip);
}

#[test]
fn equal_quick_check_but_mode_differs_is_meta_only() {
    let s = reg("a", 3, 1);
    let mut d = reg("a", 3, 1);
    d.entry.mode = 0o600;
    let mut o = opts();
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::Skip);
    o.preserve.perms = true;
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::MetaOnly);
}

#[test]
fn differing_regular_is_delta() {
    let (s, d) = (reg("a", 4, 2), reg("a", 3, 1));
    assert_eq!(decide(Some(&s), Some(&d), &opts()), Decision::Delta);
}

#[test]
fn differing_mtime_but_same_hash_is_meta_only() {
    let s = with_hash(reg("a", 3, 2), 1);
    let d = with_hash(reg("a", 3, 1), 1);
    assert_eq!(decide(Some(&s), Some(&d), &opts()), Decision::MetaOnly);
}

#[test]
fn checksum_requires_hashes() {
    let mut o = opts();
    o.checksum = true;
    let (s, d) = (reg("a", 3, 1), reg("a", 3, 1));
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::Delta);
    let (s, d) = (with_hash(reg("a", 3, 1), 1), with_hash(reg("a", 3, 1), 2));
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::Delta);
    let (s, d) = (with_hash(reg("a", 3, 1), 1), with_hash(reg("a", 3, 1), 1));
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::Skip);
}

#[test]
fn kind_change_is_whole() {
    let s = reg("a", 3, 1);
    let d = side("a", Kind::Directory, 0o755);
    assert_eq!(decide(Some(&s), Some(&d), &opts()), Decision::Whole);
    assert_eq!(decide(Some(&d), Some(&s), &opts()), Decision::Mkdir);
}

#[test]
fn directory_same_kind() {
    let s = side("d", Kind::Directory, 0o755);
    let mut d = s.clone();
    let mut o = opts();
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::Skip);
    d.entry.mode = 0o700;
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::Skip);
    o.preserve.perms = true;
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::MetaOnly);
}

#[test]
fn symlink_same_kind() {
    let mut s = side("l", Kind::Symlink, 0o777);
    s.entry.link_target = b"x".to_vec();
    let mut d = s.clone();
    assert_eq!(decide(Some(&s), Some(&d), &opts()), Decision::Skip);
    d.entry.link_target = b"y".to_vec();
    assert_eq!(decide(Some(&s), Some(&d), &opts()), Decision::Symlink);
}

#[test]
fn special_same_kind() {
    let mut s = side("c", Kind::CharDev, 0o600);
    s.entry.rdev = 5;
    let mut d = s.clone();
    let mut o = opts();
    o.preserve.devices = true;
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::Skip);
    d.entry.rdev = 6;
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::Special);
}

#[test]
fn src_missing_with_delete_is_delete() {
    let d = reg("a", 3, 1);
    let mut o = opts();
    o.delete = true;
    assert_eq!(decide(None, Some(&d), &o), Decision::Delete);
}

#[test]
fn src_missing_without_delete_is_skip() {
    let d = reg("a", 3, 1);
    assert_eq!(decide(None, Some(&d), &opts()), Decision::Skip);
}

#[test]
fn update_only_keeps_newer_dst() {
    let (s, d) = (reg("a", 4, 1), reg("a", 3, 9));
    let mut o = opts();
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::Delta);
    o.update_only = true;
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::Skip);
}

#[test]
fn merge_join_aligns_sorted_streams() {
    let src = stream::iter(vec![reg("a", 1, 1), reg("b", 1, 1), reg("d", 1, 1)]);
    let dst = stream::iter(vec![reg("b", 1, 1), reg("c", 1, 1), reg("d", 1, 1)]);
    let out: Vec<_> = block_on(merge_join(src, dst).collect());
    let shape: Vec<(Vec<u8>, bool, bool)> = out
        .iter()
        .map(|(p, s, d)| (p.as_bytes().to_vec(), s.is_some(), d.is_some()))
        .collect();
    assert_eq!(
        shape,
        vec![
            (b"a".to_vec(), true, false),
            (b"b".to_vec(), true, true),
            (b"c".to_vec(), false, true),
            (b"d".to_vec(), true, true),
        ]
    );
}

#[test]
fn merge_join_skips_invalid_paths_and_handles_empty() {
    let src = stream::iter(vec![reg("../x", 1, 1), reg("a", 1, 1)]);
    let dst = stream::iter(Vec::<Side>::new());
    let out: Vec<_> = block_on(merge_join(src, dst).collect());
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0.as_bytes(), b"a");
}

fn with_nsec(mut s: Side, nsec: u32) -> Side {
    s.entry.mtime_nsec = nsec;
    s
}

#[test]
fn nsec_only_difference_is_not_quick_check_equal() {
    let s = with_nsec(reg("a", 3, 1), 5);
    let d = with_nsec(reg("a", 3, 1), 6);
    assert_eq!(decide(Some(&s), Some(&d), &opts()), Decision::Delta);
    let (s, d) = (with_hash(s, 1), with_hash(d, 1));
    assert_eq!(decide(Some(&s), Some(&d), &opts()), Decision::MetaOnly);
}

#[test]
fn update_only_nsec_newer_dst_skips() {
    let s = with_nsec(reg("a", 4, 1), 5);
    let d = with_nsec(reg("a", 3, 1), 6);
    let mut o = opts();
    o.update_only = true;
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::Skip);
}

#[test]
fn update_only_equal_mtime_proceeds() {
    let (s, d) = (reg("a", 4, 1), reg("a", 3, 1));
    let mut o = opts();
    o.update_only = true;
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::Delta);
}

#[test]
fn mode_mask_ignores_type_bits() {
    let mut s = reg("a", 3, 1);
    s.entry.mode = 0o100644;
    let d = reg("a", 3, 1);
    let mut o = opts();
    o.preserve.perms = true;
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::Skip);
}

#[test]
fn times_owner_group_each_trigger_meta_only() {
    // Directory: mtime is metadata only (no quick check involved).
    let s = side("d", Kind::Directory, 0o755);
    let mut d = s.clone();
    d.entry.mtime_sec = 99;
    let mut o = opts();
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::Skip);
    o.preserve.times = true;
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::MetaOnly);

    let (s, mut d) = (reg("a", 3, 1), reg("a", 3, 1));
    d.entry.uid = 5;
    let mut o = opts();
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::Skip);
    o.preserve.owner = true;
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::MetaOnly);

    let (s, mut d) = (reg("a", 3, 1), reg("a", 3, 1));
    d.entry.gid = 5;
    let mut o = opts();
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::Skip);
    o.preserve.group = true;
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::MetaOnly);
}

#[test]
fn a_file_over_a_directory_is_whole() {
    let s = with_hash(reg("a", 3, 1), 7);
    let d = side("a", Kind::Directory, 0o755);
    assert_eq!(decide(Some(&s), Some(&d), &opts()), Decision::Whole);
}

fn collect_keys(src: Vec<Side>, dst: Vec<Side>) -> Vec<(Vec<u8>, bool, bool)> {
    block_on(
        merge_join(stream::iter(src), stream::iter(dst))
            .map(|(p, s, d)| (p.as_bytes().to_vec(), s.is_some(), d.is_some()))
            .collect(),
    )
}

#[test]
fn merge_join_both_empty() {
    assert!(collect_keys(vec![], vec![]).is_empty());
}

#[test]
fn merge_join_one_side_empty() {
    let out = collect_keys(vec![reg("a", 1, 1), reg("b", 1, 1)], vec![]);
    assert_eq!(
        out,
        vec![(b"a".to_vec(), true, false), (b"b".to_vec(), true, false)]
    );
    let out = collect_keys(vec![], vec![reg("a", 1, 1)]);
    assert_eq!(out, vec![(b"a".to_vec(), false, true)]);
}

#[test]
fn merge_join_drops_duplicate_dst_key() {
    let out = collect_keys(vec![reg("a", 1, 1)], vec![reg("a", 1, 1), reg("a", 1, 1)]);
    assert_eq!(out, vec![(b"a".to_vec(), true, true)]);
}

#[test]
fn merge_join_drops_duplicate_src_key() {
    let out = collect_keys(vec![reg("a", 1, 1), reg("a", 1, 1)], vec![reg("a", 1, 1)]);
    assert_eq!(out, vec![(b"a".to_vec(), true, true)]);
}

#[test]
fn merge_join_drops_duplicates_on_a_one_sided_key() {
    // A duplicate source key with no destination counterpart (after Less).
    let out = collect_keys(
        vec![reg("a", 1, 1), reg("a", 1, 1), reg("c", 1, 1)],
        vec![reg("b", 1, 1)],
    );
    assert_eq!(
        out,
        vec![
            (b"a".to_vec(), true, false),
            (b"b".to_vec(), false, true),
            (b"c".to_vec(), true, false),
        ]
    );
    // A duplicate destination key with no source counterpart (after Greater).
    let out = collect_keys(
        vec![reg("c", 1, 1)],
        vec![reg("a", 1, 1), reg("b", 1, 1), reg("b", 1, 1)],
    );
    assert_eq!(
        out,
        vec![
            (b"a".to_vec(), false, true),
            (b"b".to_vec(), false, true),
            (b"c".to_vec(), true, false),
        ]
    );
    // Both sides list the key twice: one pair, never a stray one-sided `a`.
    let out = collect_keys(
        vec![reg("a", 1, 1), reg("a", 1, 1)],
        vec![reg("a", 1, 1), reg("a", 1, 1)],
    );
    assert_eq!(out, vec![(b"a".to_vec(), true, true)]);
}

#[test]
fn merge_join_out_of_order_drops_the_key_that_goes_back() {
    // `a` after `b` on the source: the dropped item is the late source `a`,
    // which must not reappear as a source-only key (nor turn the paired
    // destination `a` into a deletion).
    let out = collect_keys(
        vec![reg("b", 1, 1), reg("a", 1, 1)],
        vec![reg("a", 1, 1), reg("b", 1, 1)],
    );
    assert_eq!(
        out,
        vec![(b"a".to_vec(), false, true), (b"b".to_vec(), true, true)]
    );
}

#[test]
fn special_mode_mask_ignores_type_bits() {
    // A listing carries the full `st_mode`; only the permission bits count.
    let mut s = side("p", Kind::Fifo, 0o010640);
    s.entry.rdev = 0;
    let mut d = side("p", Kind::Fifo, 0o640);
    let mut o = opts();
    o.preserve.specials = true;
    o.preserve.perms = true;
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::Skip);
    d.entry.mode = 0o010600;
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::MetaOnly);
    // The same for a device node.
    let s = side("c", Kind::CharDev, 0o020600);
    let d = side("c", Kind::CharDev, 0o600);
    o.preserve.devices = true;
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::Skip);
}

#[test]
fn special_metadata_only_difference_is_meta_only() {
    let mut s = side("p", Kind::Fifo, 0o640);
    s.entry.mtime_sec = 100;
    let mut d = s.clone();
    let mut o = opts();
    o.preserve.specials = true;
    o.preserve.times = true;
    o.preserve.perms = true;
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::Skip);
    // Only the mtime differs: fix it in place, do not recreate.
    d.entry.mtime_sec = 200;
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::MetaOnly);
    // Only the mode differs: the same.
    d.entry.mtime_sec = 100;
    d.entry.mode = 0o600;
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::MetaOnly);
    // Without -t a differing mtime is not a difference.
    d.entry.mode = 0o640;
    d.entry.mtime_sec = 200;
    o.preserve.times = false;
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::Skip);
    // A different device number needs a new node.
    d.entry.rdev = 9;
    assert_eq!(decide(Some(&s), Some(&d), &o), Decision::Special);
}
