//! POSIX extras end to end: hard links (`-H`), sparse files (`-S`), FIFOs
//! (`-D`) and ownership (`-o`/`-g`) without privilege.

mod common;

use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;
use std::time::Duration;

use common::{archive_opts, assert_trees_equal, sync_trees, Flow};
use irsync::fs::meta::{apply_metadata, lstat_entry, ApplyOpts};
use irsync::fs::paths::RelPath;

const MIB: usize = 1024 * 1024;

async fn within60<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(60), f)
        .await
        .expect("test timed out after 60 s")
}

fn data(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2654435761).wrapping_add(1);
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

fn opts_with(f: impl FnOnce(&mut irsync::cli::Preserve)) -> irsync::cli::SyncOptions {
    let mut o = archive_opts();
    f(&mut o.preserve);
    o
}

fn ino(p: &Path) -> (u64, u64) {
    let m = fs::symlink_metadata(p).unwrap();
    (m.dev(), m.ino())
}

#[tokio::test]
async fn hard_links_recreated_as_links() {
    const SIZE: usize = 2 * MIB + 777;
    for flow in [Flow::Push, Flow::Pull] {
        let opts = opts_with(|p| p.hard_links = true);
        let (out, h) = within60(sync_trees(
            |src| {
                fs::create_dir(src.join("d")).unwrap();
                fs::write(src.join("a.bin"), data(SIZE, 7)).unwrap();
                fs::hard_link(src.join("a.bin"), src.join("d/b.bin")).unwrap();
                fs::hard_link(src.join("a.bin"), src.join("z.bin")).unwrap();
                fs::write(src.join("solo.txt"), b"alone").unwrap();
            },
            |_| {},
            &opts,
            flow,
        ))
        .await;
        assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
        let dst = h.dst(flow);
        assert_trees_equal(h.src(flow), dst, &opts.preserve);
        let a = ino(&dst.join("a.bin"));
        assert_eq!(a, ino(&dst.join("d/b.bin")), "{flow:?}: d/b.bin not linked");
        assert_eq!(a, ino(&dst.join("z.bin")), "{flow:?}: z.bin not linked");
        assert_eq!(fs::metadata(dst.join("a.bin")).unwrap().nlink(), 3);
        assert_ne!(a, ino(&dst.join("solo.txt")));
        // The linked content crossed the wire once.
        let one = (SIZE + 5) as u64;
        assert!(
            out.bytes_transferred >= one && out.bytes_transferred < one + (SIZE as u64) / 2,
            "{flow:?}: transferred {} bytes for one {one}-byte file",
            out.bytes_transferred
        );

        // A second run finds the links in place and changes nothing.
        let again = within60(h.run(&opts, flow)).await.expect("second run");
        assert!(again.errors.is_empty(), "{flow:?}: {:?}", again.errors);
        assert_eq!(again.bytes_transferred, 0, "{flow:?}");
        assert_eq!(a, ino(&dst.join("z.bin")));
    }
}

#[tokio::test]
async fn hard_links_without_h_are_independent_files() {
    // The daemon always groups links when it sends; a client without -H
    // still gets separate files, with the content transferred once.
    let opts = archive_opts();
    assert!(!opts.preserve.hard_links);
    let (out, h) = within60(sync_trees(
        |src| {
            fs::write(src.join("a"), data(300_000, 3)).unwrap();
            fs::hard_link(src.join("a"), src.join("b")).unwrap();
        },
        |_| {},
        &opts,
        Flow::Pull,
    ))
    .await;
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let dst = h.dst(Flow::Pull);
    assert_trees_equal(h.src(Flow::Pull), dst, &opts.preserve);
    assert_ne!(ino(&dst.join("a")), ino(&dst.join("b")));
}

fn write_sparse(p: &Path, size: u64) {
    let mut f = fs::File::create(p).unwrap();
    f.write_all(&data(MIB, 1)).unwrap();
    f.seek(SeekFrom::Start(size - MIB as u64)).unwrap();
    f.write_all(&data(MIB, 2)).unwrap();
    f.set_len(size).unwrap();
}

#[tokio::test]
async fn sparse_file_stays_sparse() {
    const SIZE: u64 = 64 * MIB as u64;
    for flow in [Flow::Push, Flow::Pull] {
        let opts = opts_with(|p| p.sparse = true);
        let (out, h) = within60(sync_trees(
            |src| {
                write_sparse(&src.join("sparse.img"), SIZE);
                // Ends in a zero run: the length must still come out right.
                let mut f = fs::File::create(src.join("tail.img")).unwrap();
                f.write_all(&data(MIB, 9)).unwrap();
                f.set_len(8 * MIB as u64).unwrap();
            },
            |_| {},
            &opts,
            flow,
        ))
        .await;
        assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
        assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
        let m = fs::metadata(h.dst(flow).join("sparse.img")).unwrap();
        assert_eq!(m.len(), SIZE);
        assert!(
            m.blocks() * 512 < SIZE / 2,
            "{flow:?}: dst is not sparse: {} bytes allocated",
            m.blocks() * 512
        );
        let t = fs::metadata(h.dst(flow).join("tail.img")).unwrap();
        assert_eq!(t.len(), 8 * MIB as u64);
        assert!(
            t.blocks() * 512 < 4 * MIB as u64,
            "{flow:?}: tail not sparse"
        );
    }
}

#[tokio::test]
async fn fifo_recreated() {
    for flow in [Flow::Push, Flow::Pull] {
        let opts = archive_opts();
        let (out, h) = within60(sync_trees(
            |src| {
                nix::unistd::mkfifo(
                    &src.join("pipe"),
                    nix::sys::stat::Mode::from_bits_truncate(0o640),
                )
                .unwrap();
            },
            |_| {},
            &opts,
            flow,
        ))
        .await;
        assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
        let m = fs::symlink_metadata(h.dst(flow).join("pipe")).unwrap();
        assert!(m.file_type().is_fifo(), "{flow:?}: not a FIFO");
        assert_eq!(m.mode() & 0o7777, 0o640);
    }
}

#[tokio::test]
async fn sockets_skipped_with_warning() {
    for flow in [Flow::Push, Flow::Pull] {
        let opts = archive_opts();
        let (out, h) = within60(sync_trees(
            |src| {
                let _l = std::os::unix::net::UnixListener::bind(src.join("sock")).unwrap();
                fs::write(src.join("f"), b"x").unwrap();
            },
            |_| {},
            &opts,
            flow,
        ))
        .await;
        assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
        assert!(h.dst(flow).join("f").exists());
        assert!(fs::symlink_metadata(h.dst(flow).join("sock")).is_err());
    }
}

fn foreign_ids() -> (u32, u32) {
    let uid = nix::unistd::geteuid().as_raw();
    let mut mine: Vec<u32> = nix::unistd::getgroups()
        .unwrap_or_default()
        .into_iter()
        .map(|g| g.as_raw())
        .collect();
    mine.push(nix::unistd::getegid().as_raw());
    let gid = (40_000..).find(|g| !mine.contains(g)).unwrap();
    (uid.wrapping_add(4242), gid)
}

#[test]
fn owner_skipped_without_root() {
    if nix::unistd::geteuid().is_root() {
        eprintln!("skipped: running as root");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("f"), b"x").unwrap();
    let rel = RelPath::new(b"f").unwrap();
    let before = lstat_entry(dir.path(), &rel).unwrap();
    let (uid, gid) = foreign_ids();
    let mut e = before.clone();
    e.uid = uid;
    e.gid = gid;
    let opts = ApplyOpts {
        perms: true,
        times: true,
        owner: true,
        group: true,
    };
    apply_metadata(&dir.path().join("f"), &e, &opts).expect("ownership is skipped, not an error");
    let after = lstat_entry(dir.path(), &rel).unwrap();
    assert_eq!(after.uid, before.uid);
    assert_eq!(after.gid, before.gid);
}

#[test]
fn device_needs_privilege() {
    if nix::unistd::geteuid().is_root() {
        eprintln!("skipped: running as root");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut e = irsync_proto::v1::FileEntry {
        kind: irsync_proto::v1::file_entry::Kind::CharDev as i32,
        mode: 0o20644,
        rdev: nix::sys::stat::makedev(1, 3),
        ..Default::default()
    };
    let err = irsync::fs::meta::create_special(&dir.path().join("null"), &e).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(nix::libc::EPERM), "{err}");
    e.kind = irsync_proto::v1::file_entry::Kind::BlockDev as i32;
    let err = irsync::fs::meta::create_special(&dir.path().join("blk"), &e).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(nix::libc::EPERM), "{err}");
}

#[tokio::test]
async fn ownership_sync_without_root_succeeds() {
    // -a includes -o/-g: as a normal user they are skipped, not errors.
    let opts = archive_opts();
    assert!(opts.preserve.owner && opts.preserve.group);
    let (out, h) = within60(sync_trees(
        |src| fs::write(src.join("f"), b"owned").unwrap(),
        |_| {},
        &opts,
        Flow::Push,
    ))
    .await;
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let m = fs::metadata(h.dst(Flow::Push).join("f")).unwrap();
    assert_eq!(m.uid(), nix::unistd::geteuid().as_raw());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hard_links_survive_a_dropped_connection() {
    // The links are stored with the plan, so the resumed connection places them.
    let opts = opts_with(|p| p.hard_links = true);
    for flow in [Flow::Push, Flow::Pull] {
        let h = common::Harness::new().await;
        let src = h.src(flow);
        fs::create_dir(src.join("links")).unwrap();
        for i in 0..10 {
            let f = src.join(format!("f{i:02}.bin"));
            fs::write(&f, data(2 * MIB, i + 1)).unwrap();
            fs::hard_link(&f, src.join(format!("links/l{i:02}"))).unwrap();
        }
        h.daemon.fault.drop_after_bytes(8 * MIB as u64);
        let ropts = irsync::session::reconnect::ReconnectOpts {
            backoff_min: Duration::from_millis(100),
            ..Default::default()
        };
        let (out, reconnects) = within60(h.run_reconnect(&opts, flow, ropts, h.local_side())).await;
        let out = out.unwrap_or_else(|e| panic!("{flow:?}: {e:#}"));
        assert!(h.daemon.fault.fired(), "{flow:?}: the fault never fired");
        assert_eq!(reconnects, 1, "{flow:?}");
        assert!(out.resumed, "{flow:?}");
        assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
        let dst = h.dst(flow);
        assert_trees_equal(h.src(flow), dst, &opts.preserve);
        for i in 0..10 {
            assert_eq!(
                ino(&dst.join(format!("f{i:02}.bin"))),
                ino(&dst.join(format!("links/l{i:02}"))),
                "{flow:?}: links/l{i:02} not linked"
            );
        }
    }
}

fn set_mtime(p: &Path, sec: i64, nsec: u32) {
    let t = nix::sys::time::TimeSpec::new(sec, nsec as _);
    nix::sys::stat::utimensat(
        nix::fcntl::AT_FDCWD,
        p,
        &t,
        &t,
        nix::sys::stat::UtimensatFlags::NoFollowSymlink,
    )
    .unwrap();
}

fn mtime_of(p: &Path) -> (i64, i64) {
    let m = fs::symlink_metadata(p).unwrap();
    (m.mtime(), m.mtime_nsec())
}

/// A FIFO's modification time is applied when it is created and again when
/// only the time changes on the source (irsync-stress found it never was).
#[tokio::test]
async fn fifo_mtime_is_applied_and_updated() {
    for flow in [Flow::Push, Flow::Pull] {
        let opts = archive_opts();
        let (out, h) = within60(sync_trees(
            |src| {
                nix::unistd::mkfifo(
                    &src.join("pipe"),
                    nix::sys::stat::Mode::from_bits_truncate(0o640),
                )
                .unwrap();
                set_mtime(&src.join("pipe"), 1_000_000_000, 123_000_000);
            },
            |_| {},
            &opts,
            flow,
        ))
        .await;
        assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
        let dst = h.dst(flow).join("pipe");
        assert!(fs::symlink_metadata(&dst).unwrap().file_type().is_fifo());
        assert_eq!(
            mtime_of(&dst),
            (1_000_000_000, 123_000_000),
            "{flow:?}: created"
        );

        set_mtime(&h.src(flow).join("pipe"), 1_100_000_000, 456_000_000);
        let again = within60(h.run(&opts, flow)).await.expect("second run");
        assert!(again.errors.is_empty(), "{flow:?}: {:?}", again.errors);
        assert_eq!(
            mtime_of(&dst),
            (1_100_000_000, 456_000_000),
            "{flow:?}: updated"
        );
        assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
    }
}

fn long(c: char) -> String {
    std::iter::repeat_n(c, 255).collect()
}

/// Every entry kind with a 255-byte name (NAME_MAX): irsync's temporary
/// names must fit next to them (irsync-stress found they did not).
#[tokio::test]
async fn name_max_entries_transfer() {
    for flow in [Flow::Push, Flow::Pull] {
        let opts = opts_with(|p| p.hard_links = true);
        let (out, h) = within60(sync_trees(
            |src| {
                fs::create_dir(src.join(long('d'))).unwrap();
                fs::write(src.join(long('d')).join(long('f')), data(3 * MIB, 3)).unwrap();
                fs::write(src.join(long('g')), data(70_000, 4)).unwrap();
                fs::write(src.join("a"), data(50_000, 5)).unwrap();
                fs::hard_link(src.join("a"), src.join(long('h'))).unwrap();
                std::os::unix::fs::symlink("a", src.join(long('l'))).unwrap();
                nix::unistd::mkfifo(
                    &src.join(long('p')),
                    nix::sys::stat::Mode::from_bits_truncate(0o640),
                )
                .unwrap();
            },
            |_| {},
            &opts,
            flow,
        ))
        .await;
        assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
        let (src, dst) = (h.src(flow), h.dst(flow));
        assert_trees_equal(src, dst, &opts.preserve);
        assert_eq!(
            ino(&dst.join("a")),
            ino(&dst.join(long('h'))),
            "{flow:?}: not linked"
        );

        // Change the long-named files so they are staged through `.part` again,
        // and repoint the long-named symlink so it goes through `.tmp` again.
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(src.join(long('d')).join(long('f')))
            .unwrap();
        f.write_all(&data(100_000, 6)).unwrap();
        fs::write(src.join(long('g')), data(80_000, 7)).unwrap();
        fs::remove_file(src.join(long('l'))).unwrap();
        std::os::unix::fs::symlink(long('g'), src.join(long('l'))).unwrap();
        let again = within60(h.run(&opts, flow)).await.expect("second run");
        assert!(again.errors.is_empty(), "{flow:?}: {:?}", again.errors);
        assert_trees_equal(src, dst, &opts.preserve);
        // No temporary file of ours is left anywhere in the destination.
        fn leftovers(d: &Path, out: &mut Vec<std::path::PathBuf>) {
            for e in fs::read_dir(d).unwrap() {
                let e = e.unwrap();
                let name = e.file_name();
                if name.as_encoded_bytes().starts_with(b".irsync.") {
                    out.push(e.path());
                }
                if e.file_type().unwrap().is_dir() {
                    leftovers(&e.path(), out);
                }
            }
        }
        let mut left = Vec::new();
        leftovers(dst, &mut left);
        assert!(left.is_empty(), "{flow:?}: {left:?}");
    }
}
