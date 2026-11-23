//! filegen on Windows: what only Windows has (the read-only attribute,
//! junctions, symlink kinds, 100 ns times, sparse files, edge names), and
//! whole runs there.
#![cfg(windows)]

mod common;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

use common::snapshot;
use filegen::caps::{self, Caps};
use filegen::chain::Chain;
use filegen::config::{self, default_weights};
use filegen::kind::OpKind;
use filegen::log::{parse_step_line, StepLine};
use filegen::model::NodeKind;
use filegen::ops::{dispatch, OpCtx, OpOutcome};
use filegen::platform::{self, testutil, FileKind, Platform};
use filegen::walk::from_disk;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_filegen"))
}

/// A temp dir holding an empty `root` and an empty output dir `out`.
fn dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let t = tempfile::TempDir::new().unwrap();
    let (root, out) = (t.path().join("root"), t.path().join("out"));
    fs::create_dir(&root).unwrap();
    fs::create_dir(&out).unwrap();
    (t, root, out)
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// `filegen run -q --seed <seed> --steps <steps> --out-dir <out> <extra> <root>`.
fn run(root: &Path, out: &Path, seed: u64, steps: u64, extra: &[&str]) -> Output {
    bin()
        .args(["run", "-q", "--seed", &seed.to_string()])
        .args(["--steps", &steps.to_string(), "--out-dir"])
        .arg(out)
        .args(["--set", "max_file_size=16K", "--set", "max_total_size=4M"])
        .args(extra)
        .arg(root)
        .output()
        .unwrap()
}

fn assert_ok(o: &Output) {
    assert_eq!(o.status.code(), Some(0), "{}", stderr(o));
}

/// The step lines of a log, parsed.
fn steps(log: &Path) -> Vec<StepLine> {
    fs::read_to_string(log)
        .unwrap()
        .lines()
        .filter_map(parse_step_line)
        .collect()
}

/// A config file in `dir` whose chain draws only the kinds in `kinds`, with
/// their weights, from every kind.
fn weights_config(dir: &Path, kinds: &[(OpKind, f64)]) -> PathBuf {
    let mut yaml = String::from("weights:\n");
    for from in OpKind::ALL {
        yaml.push_str(&format!("  {}:\n", from.name()));
        for to in OpKind::ALL {
            let w = kinds
                .iter()
                .find(|(k, _)| *k == to)
                .map_or(0.0, |(_, w)| *w);
            yaml.push_str(&format!("    {}: {w}\n", to.name()));
        }
    }
    let path = dir.join("weights.yaml");
    fs::write(&path, yaml).unwrap();
    path
}

/// Sets or clears the read-only attribute of `path`.
fn set_readonly(path: &Path, on: bool) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_readonly(on);
    fs::set_permissions(path, perms).unwrap();
}

/// The POSIX-only kinds, as the startup notice names them.
const POSIX_ONLY: &str = "mkfifo, chmod, set_xattr, chown, mknod";

#[test]
fn a_default_run_finishes_and_resumes() {
    // The default config writes some 2 MB a step, so a few hundred steps
    // keep this to seconds in a debug build.
    let (_t, root, out) = dirs();
    let o = bin()
        .args(["run", "--seed", "1", "--steps", "600", "--out-dir"])
        .arg(&out)
        .arg(&root)
        .output()
        .unwrap();
    assert_ok(&o);
    let notice = format!("filegen: not available here, weight set to 0: {POSIX_ONLY}");
    let count = |o: &Output| stderr(o).lines().filter(|l| *l == notice).count();
    assert_eq!(count(&o), 1, "{}", stderr(&o));
    let state = out.join("filegen-1.state.yaml");
    let o = bin()
        .args(["run", "--steps", "200", "--resume"])
        .arg(&state)
        .arg(&root)
        .output()
        .unwrap();
    assert_ok(&o);
    assert_eq!(count(&o), 1, "{}", stderr(&o));
    assert_eq!(filegen::state::read(&state).unwrap().steps_done, 800);
    let log = fs::read_to_string(out.join("filegen-1.log")).unwrap();
    let runs: Vec<&str> = log.lines().filter(|l| l.starts_with("# run ")).collect();
    assert_eq!(runs.len(), 2);
    for r in runs {
        assert!(r.contains(" platform=windows "), "{r}");
    }
    assert_eq!(steps(&out.join("filegen-1.log")).len(), 800);
}

/// A step line without what the clock decides: the time, the durations
/// and the mtimes of the post-step stats.
fn without_timing(line: &str) -> String {
    line.split(' ')
        .filter(|w| {
            !["ts=", "dur_us=", "gen_us="]
                .iter()
                .any(|k| w.starts_with(k))
        })
        .map(
            |w| match w.starts_with("post=") || w.starts_with("post_to=") {
                true => w.rsplit_once(':').map_or(w, |(keep, _)| keep),
                false => w,
            },
        )
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn the_same_seed_gives_the_same_run() {
    // Both runs at one root path: absolute symlink targets name it.
    let (t, root, out) = dirs();
    let mut logs = Vec::new();
    let mut trees = Vec::new();
    for i in 0..2 {
        let out = out.join(i.to_string());
        assert_ok(&run(&root, &out, 2, 2000, &[]));
        let log = fs::read_to_string(out.join("filegen-2.log")).unwrap();
        let lines: Vec<String> = log
            .lines()
            .filter(|l| l.starts_with("step="))
            .map(without_timing)
            .collect();
        assert_eq!(lines.len(), 2000);
        logs.push(lines);
        trees.push(snapshot(&root));
        fs::rename(&root, t.path().join(format!("done{i}"))).unwrap();
        fs::create_dir(&root).unwrap();
    }
    assert_eq!(logs[0], logs[1]);
    assert_eq!(trees[0], trees[1]);
}

#[test]
fn punch_hole_steps_make_real_holes() {
    let (t, root, out) = dirs();
    let cfg = weights_config(
        t.path(),
        &[(OpKind::CreateFile, 1.0), (OpKind::PunchHole, 3.0)],
    );
    let o = run(&root, &out, 4, 300, &["--config", cfg.to_str().unwrap()]);
    assert_ok(&o);
    let punched: Vec<StepLine> = steps(&out.join("filegen-4.log"))
        .into_iter()
        .filter(|l| l.op == OpKind::PunchHole && !l.details.contains_key("mode"))
        .collect();
    assert!(punched.len() > 20, "only {} holes punched", punched.len());
    for l in &punched {
        assert!(!l.details.contains_key("emulated"), "{l:?}");
        assert_eq!(l.bytes, 0, "{l:?}");
    }
}

#[test]
fn hard_links_share_an_identity() {
    let t = tempfile::TempDir::new().unwrap();
    let (f, g) = (t.path().join("f"), t.path().join("g"));
    fs::write(&f, b"x").unwrap();
    fs::hard_link(&f, &g).unwrap();
    let (a, b) = (platform::lstat(&f).unwrap(), platform::lstat(&g).unwrap());
    assert_eq!((a.ino, a.dev, a.nlink), (b.ino, b.dev, 2));
    assert_eq!(b.nlink, 2);
}

#[test]
fn set_mtime_does_not_follow_and_rounds_to_100ns() {
    let t = tempfile::TempDir::new().unwrap();
    let f = t.path().join("f");
    fs::write(&f, b"x").unwrap();
    platform::set_mtime(&f, 1_700_000_000, 123_456_700).unwrap();
    assert_eq!(
        platform::lstat(&f).unwrap().mtime_ns,
        1_700_000_000_123_456_700
    );
    // Stored in steps of 100 ns: the rest is dropped.
    platform::set_mtime(&f, 1_700_000_000, 123_456_789).unwrap();
    assert_eq!(
        platform::lstat(&f).unwrap().mtime_ns,
        1_700_000_000_123_456_700
    );
    if !testutil::symlinks_available(t.path()) {
        return;
    }
    let link = t.path().join("l");
    testutil::symlink(Path::new("f"), &link).unwrap();
    platform::set_mtime(&link, 1_000_000_000, 500).unwrap();
    assert_eq!(
        platform::lstat(&link).unwrap().mtime_ns,
        1_000_000_000_000_000_500
    );
    assert_eq!(
        platform::lstat(&f).unwrap().mtime_ns,
        1_700_000_000_123_456_700
    );
}

#[test]
fn distant_mtimes_work() {
    // A thousand years back reaches before 1970: drawn times are clamped
    // to the epoch, never to before 1601, which Windows cannot store.
    let (t, root, out) = dirs();
    let cfg = weights_config(
        t.path(),
        &[
            (OpKind::CreateFile, 1.0),
            (OpKind::Mkdir, 1.0),
            (OpKind::SetMtime, 4.0),
        ],
    );
    let o = run(
        &root,
        &out,
        5,
        500,
        &[
            "--config",
            cfg.to_str().unwrap(),
            "--set",
            "mtime.distant_years=1000",
            "--set",
            "mtime.distant=1",
        ],
    );
    assert_ok(&o);
    let mut clamped = 0;
    let set: Vec<StepLine> = steps(&out.join("filegen-5.log"))
        .into_iter()
        .filter(|l| l.op == OpKind::SetMtime)
        .collect();
    assert!(set.len() > 200, "only {} set_mtime steps", set.len());
    for l in &set {
        let logged: jiff::Timestamp = l.details["mtime"].parse().unwrap();
        let logged = logged.as_nanosecond();
        // The stat right after the step: the mtime on disk.
        let post = l.post.unwrap().unwrap();
        assert_eq!(post.mtime_ns, logged, "{l:?}");
        assert!(logged >= 0 && logged % 100 == 0, "{l:?}");
        clamped += usize::from(logged < 1_000_000_000);
    }
    assert!(clamped > 0, "no time was drawn before 1970");
}

/// Whether `name` holds an unpaired surrogate.
fn has_surrogate(name: &OsString) -> bool {
    filegen::wtf8::decode(&platform::os_to_bytes(name))
        .unwrap()
        .iter()
        .any(|u| (0xD800..=0xDFFF).contains(u))
}

/// Every name in the tree below `root`, read below its canonical path.
fn names_below(root: &Path) -> Vec<OsString> {
    let mut out = Vec::new();
    let mut stack = vec![fs::canonicalize(root).unwrap()];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).unwrap() {
            let e = e.unwrap();
            if platform::lstat(&e.path()).unwrap().kind == FileKind::Dir {
                stack.push(e.path());
            }
            out.push(e.file_name());
        }
    }
    out
}

#[test]
fn edge_names_exist_on_disk() {
    let (_t, root, out) = dirs();
    assert_ok(&run(&root, &out, 6, 300, &["--set", "names.edge_rate=1"]));
    let names = names_below(&root);
    assert!(names.iter().any(has_surrogate), "{names:?}");
    let devices = ["CON", "PRN", "AUX", "NUL", "COM1", "LPT1"];
    assert!(
        names.iter().any(|n| {
            let n = n.to_string_lossy();
            devices.contains(&n.split('.').next().unwrap())
        }),
        "{names:?}"
    );
    // A second start walks them and goes on.
    let o = bin()
        .args(["run", "-q", "--steps", "100", "--resume"])
        .arg(out.join("filegen-6.state.yaml"))
        .arg(&root)
        .output()
        .unwrap();
    assert_ok(&o);
    let state = filegen::state::read(&out.join("filegen-6.state.yaml")).unwrap();
    assert_eq!(state.steps_done, 400);
    let walked = from_disk(&fs::canonicalize(&root).unwrap()).unwrap();
    assert_eq!(state.tree.entries, walked.entries() as u64);
}

#[test]
fn symlinks_of_the_three_kinds() {
    let (t, root, out) = dirs();
    if !testutil::symlinks_available(t.path()) {
        return;
    }
    let cfg = weights_config(
        t.path(),
        &[
            (OpKind::CreateFile, 1.0),
            (OpKind::Mkdir, 1.0),
            (OpKind::Symlink, 2.0),
            (OpKind::RepointSymlink, 1.0),
            (OpKind::DeleteSymlink, 0.5),
        ],
    );
    assert_ok(&run(
        &root,
        &out,
        7,
        300,
        &["--config", cfg.to_str().unwrap()],
    ));
    let canon = fs::canonicalize(&root).unwrap();
    // The target each link should hold now, and the kinds seen.
    let mut links: BTreeMap<Vec<u8>, (String, String)> = BTreeMap::new();
    let mut kinds = BTreeMap::new();
    for l in steps(&out.join("filegen-7.log")) {
        match l.op {
            OpKind::Symlink | OpKind::RepointSymlink => {
                let target = l.details["target"].clone();
                let kind = l.details["target_kind"].clone();
                *kinds
                    .entry((kind.clone(), l.details["absolute"].clone()))
                    .or_insert(0) += 1;
                assert!(!target.contains('/'), "{target}");
                links.insert(l.path, (target, kind));
            }
            OpKind::DeleteSymlink => {
                links.remove(&l.path);
            }
            _ => {}
        }
    }
    for kind in ["file", "dir", "dangling"] {
        for absolute in ["0", "1"] {
            assert!(
                kinds.contains_key(&(kind.to_string(), absolute.to_string())),
                "no {kind} target with absolute={absolute}: {kinds:?}"
            );
        }
    }
    assert!(!links.is_empty());
    for (path, (target, kind)) in &links {
        let link = platform::rel_to_path(&canon, path).unwrap();
        let back = platform::read_link(&link).unwrap();
        assert_eq!(String::from_utf8_lossy(&back), *target, "{link:?}");
        // The link's own kind matches its target: a directory symlink for
        // a directory, a file symlink otherwise.
        let followed = fs::metadata(&link);
        match kind.as_str() {
            "file" => assert!(followed.unwrap().is_file(), "{link:?}"),
            "dir" => assert!(followed.unwrap().is_dir(), "{link:?}"),
            _ => assert!(followed.is_err(), "{link:?}"),
        }
        assert_eq!(platform::lstat(&link).unwrap().kind, FileKind::Symlink);
    }
    // A target of more than one component is joined by `\`.
    assert!(links.values().any(|(t, _)| t.contains('\\')));
}

#[test]
fn without_the_symlink_capability_the_kinds_are_off() {
    let t = tempfile::TempDir::new().unwrap();
    let root = fs::canonicalize(t.path()).unwrap();
    let off = caps::disabled(
        Platform::Windows,
        Caps {
            symlinks: false,
            sparse: true,
        },
    );
    let chain = Chain::from_weights(&caps::without(&default_weights(), &off)).unwrap();
    let cfg = config::load(None, None, &[("max_file_size".into(), "16K".into())]).unwrap();
    let mut model = from_disk(&root).unwrap();
    let mut rng = ChaCha8Rng::seed_from_u64(8);
    let (mut last, mut anchor) = (OpKind::Mkdir, model.root());
    let mut done = 0;
    for _ in 0..2000 {
        let kind = chain.next(&mut rng, last, 0.0);
        assert!(!off.contains(&kind), "{kind:?} drawn");
        let mut ctx = OpCtx {
            root: &root,
            cfg: &cfg,
            model: &mut model,
            rng: &mut rng,
            anchor,
            dry: false,
            fsync: false,
        };
        if let OpOutcome::Done(a) = dispatch(kind)(&mut ctx).unwrap() {
            last = kind;
            anchor = a.anchor;
            done += 1;
        }
    }
    assert!(done > 1000, "only {done} steps did something");
    let walked = from_disk(&root).unwrap();
    assert_eq!(
        walked.node(walked.root()).counts[NodeKind::Symlink.index()],
        0
    );
}

#[test]
fn a_read_only_file_is_off_limits() {
    let (_t, root, out) = dirs();
    let f = root.join("ro.txt");
    fs::write(&f, b"read only").unwrap();
    platform::set_mtime(&f, 1_000_000_000, 0).unwrap();
    set_readonly(&f, true);
    let m = from_disk(&fs::canonicalize(&root).unwrap()).unwrap();
    let id = m.resolve(b"ro.txt").unwrap();
    let off_limits = m.node(id).kind == NodeKind::OffLimits;
    let o = run(&root, &out, 9, 500, &[]);
    let (data, mtime) = (fs::read(&f), platform::lstat(&f).map(|s| s.mtime_ns));
    set_readonly(&f, false);
    assert!(off_limits, "the walk did not put it off limits");
    assert_ok(&o);
    assert_eq!(data.unwrap(), b"read only");
    assert_eq!(mtime.unwrap(), 1_000_000_000_000_000_000);
}

#[test]
fn a_junction_is_off_limits_and_not_entered() {
    let (_t, root, out) = dirs();
    let d = root.join("d");
    fs::create_dir(&d).unwrap();
    fs::write(d.join("f"), b"f").unwrap();
    testutil::junction(&d, &d.join("loop"));
    let m = from_disk(&fs::canonicalize(&root).unwrap()).unwrap();
    let id = m.resolve(b"d/loop").unwrap();
    assert_eq!(m.node(id).kind, NodeKind::OffLimits);
    assert!(m.node(id).children.is_empty());
    assert!(m.resolve(b"d/f").is_some());
    assert_eq!(m.entries(), 3);
    // A run goes around it and leaves it be.
    assert_ok(&run(&root, &out, 10, 300, &[]));
    assert_eq!(
        platform::lstat(&d.join("loop")).unwrap().kind,
        FileKind::Other
    );
    // Still a way into `d`: the junction was neither removed nor replaced.
    assert!(fs::read_dir(d.join("loop")).is_ok());
}

#[test]
fn a_relative_root_works() {
    let (_t, root, out) = dirs();
    let o = bin()
        .current_dir(&root)
        .args(["run", "-q", "--seed", "3", "--steps", "200", "--out-dir"])
        .arg(&out)
        .arg(".")
        .output()
        .unwrap();
    assert_ok(&o);
    assert_eq!(steps(&out.join("filegen-3.log")).len(), 200);
    assert!(!snapshot(&root).is_empty());
}

#[test]
fn rename_over_takes_files_only() {
    let (t, root, out) = dirs();
    for i in 0..10 {
        fs::write(root.join(format!("f{i}")), format!("file {i}")).unwrap();
    }
    for i in 0..5 {
        fs::create_dir(root.join(format!("e{i}"))).unwrap();
        fs::hard_link(root.join(format!("f{i}")), root.join(format!("h{i}"))).unwrap();
    }
    let cfg = weights_config(t.path(), &[(OpKind::RenameOver, 1.0)]);
    assert_ok(&run(
        &root,
        &out,
        11,
        500,
        &["--config", cfg.to_str().unwrap()],
    ));
    let lines = steps(&out.join("filegen-11.log"));
    let over: Vec<&StepLine> = lines
        .iter()
        .filter(|l| l.op == OpKind::RenameOver)
        .collect();
    assert!(over.len() > 5, "only {} rename_over steps", over.len());
    // The only directories are e0..e4: nothing else makes one here.
    let dirs: Vec<Vec<u8>> = (0..5).map(|i| format!("e{i}").into_bytes()).collect();
    for l in over {
        let to = l.to.as_ref().unwrap();
        assert!(!dirs.contains(to), "{l:?}");
        assert_eq!(l.post_to.unwrap().unwrap().kind, 'f', "{l:?}");
    }
    for i in 0..5 {
        assert!(root.join(format!("e{i}")).is_dir());
    }
}

#[test]
fn a_row_left_empty_names_the_kinds_not_available_here() {
    // Valid as given, but from every kind only POSIX-only kinds follow.
    let (t, root, out) = dirs();
    let cfg = weights_config(t.path(), &[(OpKind::Mkfifo, 1.0), (OpKind::Chmod, 1.0)]);
    let o = run(&root, &out, 12, 10, &["--config", cfg.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    let err = stderr(&o);
    assert!(err.contains("has no non-zero weight"), "{err}");
    assert!(
        err.contains(&format!("were set to 0: {POSIX_ONLY}")),
        "{err}"
    );
}
