mod common;

use irsync::cli::FilterRule;
use irsync::filter::FilterSet;
use irsync::fs::paths::RelPath;
use irsync::scan::{walk, ScanEvent, ScanItem, ScanSummary, WalkOpts};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;

async fn run(root: &Path, rules: &[FilterRule], follow: bool) -> (Vec<ScanItem>, ScanSummary) {
    let f = Arc::new(FilterSet::from_rules(rules).unwrap());
    let (mut rx, sum) = walk(
        root.to_path_buf(),
        f,
        WalkOpts {
            follow_root_symlink: follow,
            threads: 4,
            max_depth: None,
        },
        None,
    );
    let mut v = Vec::new();
    while let Some(ev) = rx.recv().await {
        // Gaps are checked through the summary here.
        if let ScanEvent::Entry(i) = ev {
            v.push(i);
        }
    }
    (v, sum.await.unwrap())
}

fn names(items: &[ScanItem]) -> Vec<Vec<u8>> {
    items.iter().map(|i| i.path.as_bytes().to_vec()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn walk_emits_sorted_relpaths() {
    let d = tempfile::tempdir().unwrap();
    let mut x: u64 = 12345;
    let mut rnd = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let probe = tempfile::tempdir().unwrap();
    let ok = common::try_create_non_utf8(probe.path(), b"\xff\x80").is_some();
    let hi = |b: u8| if ok { b } else { b'q' };
    let alpha = [b'a', b'b', b'-', b'.', b'_', b'Z', hi(0xff), hi(0x80), b'0'];
    for _ in 0..200 {
        let depth = 1 + rnd() % 3;
        let mut p = d.path().to_path_buf();
        for _ in 0..depth {
            let n = 1 + rnd() % 3;
            let name: Vec<u8> = (0..n).map(|_| alpha[(rnd() % 9) as usize]).collect();
            if name == b"." {
                continue;
            }
            p.push(std::ffi::OsStr::from_bytes(&name));
        }
        if p.is_file() {
            continue;
        }
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        if !p.exists() {
            let _ = std::fs::write(&p, b"x");
        }
    }
    let (items, sum) = run(d.path(), &[], false).await;
    assert!(items.len() > 50);
    assert_eq!(sum.entries as usize, items.len());
    assert!(!sum.incomplete);
    for w in items.windows(2) {
        assert!(w[0].path < w[1].path, "{:?} !< {:?}", w[0].path, w[1].path);
    }
}

#[tokio::test]
async fn walk_excludes_subtree_without_descending() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("skip/inner")).unwrap();
    std::fs::write(d.path().join("skip/inner/f"), b"1").unwrap();
    std::fs::write(d.path().join("keep"), b"1").unwrap();
    let (items, _) = run(d.path(), &[FilterRule::Exclude("skip/".into())], false).await;
    assert_eq!(names(&items), vec![b"".to_vec(), b"keep".to_vec()]);
}

#[tokio::test]
async fn walk_reports_unreadable_dir_as_incomplete() {
    use std::os::unix::fs::PermissionsExt;
    if nix::unistd::geteuid().is_root() {
        eprintln!("skipping: running as root");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("locked")).unwrap();
    std::fs::write(d.path().join("locked/f"), b"1").unwrap();
    std::fs::write(d.path().join("ok"), b"1").unwrap();
    std::fs::set_permissions(
        d.path().join("locked"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    let (items, sum) = run(d.path(), &[], false).await;
    std::fs::set_permissions(
        d.path().join("locked"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(sum.incomplete);
    assert_eq!(sum.errors.len(), 1, "{:?}", sum.errors);
    assert_eq!(sum.errors[0].0, RelPath::new(b"locked").unwrap());
    let n = names(&items);
    assert!(n.contains(&b"ok".to_vec()));
    assert!(n.contains(&b"locked".to_vec()));
}

#[tokio::test]
async fn walk_includes_root_dir_entry() {
    use irsync_proto::v1::file_entry::Kind;
    let d = tempfile::tempdir().unwrap();
    let (items, sum) = run(d.path(), &[], false).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].path, RelPath::root());
    assert_eq!(items[0].entry.kind(), Kind::Directory);
    assert_eq!(sum.entries, 1);
}

#[tokio::test]
async fn root_symlink_followed_only_when_asked_and_not_below() {
    let d = tempfile::tempdir().unwrap();
    let real = d.path().join("real");
    std::fs::create_dir_all(real.join("sub")).unwrap();
    std::fs::write(real.join("sub/f"), b"1").unwrap();
    std::os::unix::fs::symlink(&real, d.path().join("link")).unwrap();
    std::os::unix::fs::symlink(&real, real.join("below")).unwrap();
    let (items, _) = run(&d.path().join("link"), &[], true).await;
    assert_eq!(
        names(&items),
        vec![
            b"".to_vec(),
            b"below".to_vec(),
            b"sub".to_vec(),
            b"sub/f".to_vec()
        ]
    );
}
