#[path = "common/bin.rs"]
mod bin;

use irsync::cli::{validate, Args, Command};
use irsync::location::Location;
use irsync::options::{DeleteMode, Direction, FilterRule, SyncOptions};
use std::path::PathBuf;
use std::time::Duration;

fn parse(a: &[&str]) -> Args {
    let mut v = vec!["irsync"];
    v.extend_from_slice(a);
    Args::try_parse_from(v).unwrap()
}

fn opts(a: &[&str]) -> SyncOptions {
    SyncOptions::from_args(&parse(a))
}

fn loc(s: &str) -> Location {
    Location::parse(s).unwrap()
}

fn sync_paths(a: &Args) -> Vec<String> {
    match &a.cmd {
        Some(Command::Sync(s)) => s.paths.clone(),
        c => panic!("not Sync: {c:?}"),
    }
}

fn list_paths(a: &Args) -> Vec<String> {
    match &a.cmd {
        Some(Command::List(l)) => vec![l.operand.clone()],
        c => panic!("not List: {c:?}"),
    }
}

fn parse_err(a: &[&str]) -> String {
    let mut v = vec!["irsync"];
    v.extend_from_slice(a);
    Args::try_parse_from(v).unwrap_err().to_string()
}

#[test]
fn pair_parses_host_and_flags() {
    let a = parse(&[
        "pair",
        "--server-no-dht",
        "--no-ssh-install",
        "--rsh",
        "ssh -p 2222",
        "mike@myserver",
    ]);
    match a.cmd {
        Some(Command::Pair(p)) => {
            assert_eq!(p.host, "mike@myserver");
            assert_eq!(p.rsh.as_deref(), Some("ssh -p 2222"));
            assert!(p.server_no_dht && p.no_ssh_install);
        }
        c => panic!("not Pair: {c:?}"),
    }
}

#[test]
fn sync_has_no_ssh_flags() {
    for flag in [
        &["--ssh"][..],
        &["--rsh", "x"],
        &["--no-ssh-install"],
        &["--no-remote-dht"],
    ] {
        let mut a = flag.to_vec();
        a.extend(["a/", "h:b"]);
        let e = parse_err(&a);
        assert!(e.contains(flag[0]), "{e}");
    }
}

#[test]
fn pair_refuses_a_sync_flag() {
    let e = parse_err(&["pair", "-a", "myserver"]);
    assert!(e.contains("-a"), "{e}");
}

#[test]
fn short_sync_is_rewritten_into_sync_command() {
    for (short, long) in [
        (
            vec!["-av", "a", "b", "h:m/"],
            vec!["sync", "-av", "a", "b", "h:m/"],
        ),
        (vec!["h:m/", "local/"], vec!["sync", "h:m/", "local/"]),
    ] {
        let (s, l) = (parse(&short), parse(&long));
        assert_eq!(sync_paths(&s), sync_paths(&l));
        assert!(s.sync.paths.is_empty() && l.sync.paths.is_empty());
        assert_eq!(
            format!("{:?}", SyncOptions::from_args(&s)),
            format!("{:?}", SyncOptions::from_args(&l))
        );
    }
}

#[test]
fn listings_are_rewritten_into_list_command() {
    assert_eq!(list_paths(&parse(&["home:"])), ["home:"]);
    assert_eq!(list_paths(&parse(&["sync", "home:"])), ["home:"]);
    assert_eq!(list_paths(&parse(&["list", "home:"])), ["home:"]);
    assert_eq!(list_paths(&parse(&["list", "home"])), ["home:"]);
    assert_eq!(
        list_paths(&parse(&["-v", "home:photos/"])),
        ["home:photos/"]
    );
    assert_eq!(
        list_paths(&parse(&["list", "-v", "home:photos/"])),
        ["home:photos/"]
    );
}

#[test]
fn filters_keep_order_under_the_sync_command() {
    let a = parse(&[
        "sync",
        "--exclude=a",
        "--include-from=inc",
        "--include",
        "b",
        "--exclude-from",
        "exc",
        "x",
        "h:m",
    ]);
    assert_eq!(
        SyncOptions::from_args(&a).filters,
        vec![
            FilterRule::Exclude("a".into()),
            FilterRule::IncludeFrom(PathBuf::from("inc")),
            FilterRule::Include("b".into()),
            FilterRule::ExcludeFrom(PathBuf::from("exc")),
        ]
    );
}

#[test]
fn a_listing_takes_the_list_flags_only() {
    use irsync::options::FilterRule;
    // Connection flags, filters and --no-recursive go with a listing.
    for args in [
        vec![
            "--no-dht",
            "--relay",
            "disabled",
            "--idle-timeout",
            "5",
            "h:p/",
        ],
        vec!["--no-recursive", "--exclude", "*.tmp", "h:p/"],
        vec!["list", "--no-mdns", "--address", "127.0.0.1:9", "h"],
    ] {
        let a = parse(&args);
        assert!(matches!(a.cmd, Some(Command::List(_))), "{args:?}");
    }
    let a = parse(&["--exclude", "a", "--include", "b", "h:p/"]);
    let Some(Command::List(l)) = &a.cmd else {
        panic!("not List")
    };
    assert_eq!(
        l.filters,
        [
            FilterRule::Exclude("a".into()),
            FilterRule::Include("b".into())
        ]
    );
    assert!(SyncOptions::from_args(&a).relay.is_none());
    // A sync's own flags are refused, in either form.
    for flag in ["-a", "--delete", "--inplace", "-n", "--parallel=4"] {
        for form in [
            vec![flag, "h:p/"],
            vec!["list", flag, "h:p/"],
            vec!["sync", flag, "h:p/"],
        ] {
            let mut full = vec!["irsync"];
            full.extend(form.iter());
            let e = Args::try_parse_from(&full).unwrap_err();
            assert_eq!(e.exit_code(), 2, "{form:?}");
        }
        let flag_name = flag.split('=').next().unwrap();
        assert!(
            parse_err(&[flag, "h:p/"]).contains(&format!(
                "{flag_name} is a sync option and cannot be used with a listing"
            )),
            "{flag}"
        );
    }
}

#[test]
fn sync_and_list_follow_the_command_rules() {
    // Global flags may come first, as for every command.
    let a = parse(&["-v", "sync", "a/", "h:m/"]);
    assert_eq!(sync_paths(&a), ["a/", "h:m/"]);
    assert_eq!(a.log.verbose, 1);
    assert_eq!(list_paths(&parse(&["-q", "list", "h"])), ["h:"]);
    // A sync option before the command name is an error, as with `gc`.
    assert!(parse_err(&["-av", "sync", "h:m/"])
        .contains("-a is a sync option and cannot be used with 'sync'"));
    assert!(parse_err(&["-a", "list", "h:"])
        .contains("-a is a sync option and cannot be used with 'list'"));
    // After a path the words are paths; `./sync` is always a path.
    assert_eq!(
        sync_paths(&parse(&["h:m/", "-v", "sync"])),
        ["h:m/", "sync"]
    );
    assert_eq!(sync_paths(&parse(&["./sync", "h:m/"])), ["./sync", "h:m/"]);
}

#[test]
fn sync_and_list_usage_errors() {
    for bad in [
        vec!["sync"],
        vec!["sync", "-a", "./x"],
        vec!["-a", "./x"],
        vec!["list"],
        vec!["list", "a:", "b:"],
        vec!["list", "./photos"],
        vec!["list", "photos/x"],
        vec!["list", "."],
        vec!["list", ".."],
    ] {
        let mut full = vec!["irsync"];
        full.extend(bad.iter());
        assert!(Args::try_parse_from(full).is_err(), "{bad:?}");
    }
}

#[test]
fn bad_operands_are_usage_errors_that_exit_2() {
    for (bad, says) in [
        (vec!["a", "b"], "neither source nor destination is remote"),
        (vec!["h:a", "h:b"], "cannot both be remote"),
        (vec!["h:a", "h:b", "c"], "multiple sources"),
        (vec!["h:a", "b", "c"], "multiple sources"),
        (vec![":x", "y"], "empty host"),
        (vec![":x"], "empty host"),
        (vec!["[::1:x", "y"], "no closing"),
    ] {
        for named in [false, true] {
            let mut full = vec!["irsync"];
            if named {
                full.push("sync");
            }
            full.extend(bad.iter());
            let e = Args::try_parse_from(&full).unwrap_err();
            assert_eq!(e.exit_code(), 2, "{full:?}");
            assert!(e.to_string().contains(says), "{full:?}: {e}");
        }
    }
    let e = Args::try_parse_from(["irsync", "list", ":"]).unwrap_err();
    assert_eq!(e.exit_code(), 2);
    assert!(e.to_string().contains("empty host"), "{e}");
}

#[test]
fn bad_operands_exit_2_from_the_binary() {
    let home = tempfile::tempdir().unwrap();
    let out = bin::irsync_command(home.path())
        .args(["/nonexistent-a", "/nonexistent-b"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("neither source nor destination is remote"),
        "{err}"
    );
}

#[test]
fn archive_expands_to_rlptgod() {
    let p = opts(&["-a", "src", "h:m/"]).preserve;
    assert!(p.recursive && p.links && p.perms && p.times && p.group && p.owner);
    assert!(p.devices && p.specials);
    assert!(!p.hard_links && !p.sparse);
}

#[test]
fn big_d_is_devices_and_specials() {
    let p = opts(&["-D", "src", "h:m/"]).preserve;
    assert!(p.devices && p.specials);
    let p = opts(&["--devices", "src", "h:m/"]).preserve;
    assert!(p.devices && !p.specials);
    let p = opts(&["--specials", "src", "h:m/"]).preserve;
    assert!(!p.devices && p.specials);
    let p = opts(&["src", "h:m/"]).preserve;
    assert!(!p.devices && !p.specials);
}

#[test]
fn recursion_needs_r_or_a_and_no_recursive_turns_it_off() {
    assert!(!opts(&["-t", "src", "h:m/"]).preserve.recursive);
    assert!(!opts(&["src", "h:m/"]).preserve.recursive);
    assert!(opts(&["-r", "src", "h:m/"]).preserve.recursive);
    assert!(opts(&["-a", "src", "h:m/"]).preserve.recursive);
    let p = opts(&["-a", "--no-recursive", "src", "h:m/"]).preserve;
    assert!(!p.recursive && p.links && p.times);
    assert!(!opts(&["-a", "--no-r", "src", "h:m/"]).preserve.recursive);
    assert!(Args::try_parse_from(["irsync", "-r", "--no-recursive", "src", "h:m/"]).is_err());
    assert!(!opts(&["-t", "src", "h:m/"]).preserve.dirs);
    assert!(opts(&["-td", "src", "h:m/"]).preserve.dirs);
    assert!(opts(&["--dirs", "src", "h:m/"]).preserve.dirs);
}

#[test]
fn individual_flags_and_combined_short() {
    let p = opts(&["-rtHS", "src", "h:m/"]).preserve;
    assert!(p.recursive && p.times && p.hard_links && p.sparse);
    assert!(!p.links && !p.perms && !p.owner);
}

#[test]
fn delete_alone_means_after() {
    assert_eq!(opts(&["--delete", "a", "h:m"]).delete, DeleteMode::After);
    assert_eq!(opts(&["a", "h:m"]).delete, DeleteMode::Off);
    assert_eq!(
        opts(&["--delete-before", "a", "h:m"]).delete,
        DeleteMode::Before
    );
    assert_eq!(
        opts(&["--delete-during", "a", "h:m"]).delete,
        DeleteMode::During
    );
    let o = opts(&["--delete-excluded", "a", "h:m"]);
    assert!(o.delete_excluded);
    assert_eq!(o.delete, DeleteMode::After);
    // `--delete` names no time, so it goes with any one of them.
    assert_eq!(
        opts(&["--delete", "--delete-during", "a", "h:m"]).delete,
        DeleteMode::During
    );
}

#[test]
fn delete_times_exclude_each_other() {
    for (a, b) in [
        ("--delete-before", "--delete-during"),
        ("--delete-before", "--delete-after"),
        ("--delete-during", "--delete-after"),
    ] {
        let e = parse_err(&[a, b, "a", "h:m"]);
        assert!(e.contains("cannot be used with"), "{a} {b}: {e}");
        let e = parse_err(&["sync", a, b, "a", "h:m"]);
        assert!(e.contains("cannot be used with"), "sync {a} {b}: {e}");
    }
}

#[test]
fn filters_keep_command_line_order() {
    let o = opts(&[
        "--exclude=a",
        "--include-from=inc",
        "--include",
        "b",
        "--exclude-from",
        "exc",
        "x",
        "h:m",
    ]);
    assert_eq!(
        o.filters,
        vec![
            FilterRule::Exclude("a".into()),
            FilterRule::IncludeFrom(PathBuf::from("inc")),
            FilterRule::Include("b".into()),
            FilterRule::ExcludeFrom(PathBuf::from("exc")),
        ]
    );
}

#[test]
fn bwlimit_forms() {
    assert_eq!(
        opts(&["--bwlimit=100", "a", "h:m"]).bwlimit,
        Some(100 * 1024)
    );
    assert_eq!(opts(&["--bwlimit=2M", "a", "h:m"]).bwlimit, Some(2 << 20));
    // rsync: K/M/G and KiB/MiB/GiB are 1024-based, KB/MB/GB 1000-based.
    assert_eq!(opts(&["--bwlimit=1g", "a", "h:m"]).bwlimit, Some(1 << 30));
    assert_eq!(opts(&["--bwlimit=1GiB", "a", "h:m"]).bwlimit, Some(1 << 30));
    assert_eq!(
        opts(&["--bwlimit=1gb", "a", "h:m"]).bwlimit,
        Some(1_000_000_000)
    );
    assert_eq!(opts(&["--bwlimit=3KB", "a", "h:m"]).bwlimit, Some(3000));
    assert_eq!(opts(&["--bwlimit=500b", "a", "h:m"]).bwlimit, Some(500));
    assert_eq!(opts(&["--bwlimit=1.5M", "a", "h:m"]).bwlimit, Some(3 << 19));
    assert_eq!(opts(&["--bwlimit=.5k", "a", "h:m"]).bwlimit, Some(512));
    assert_eq!(opts(&["--bwlimit=0", "a", "h:m"]).bwlimit, None);
    for bad in [
        "x",
        "",
        "1.2.3",
        "-1",
        "1xb",
        "k",
        "1kib2",
        "inf",
        "99999999999P",
    ] {
        let a = format!("--bwlimit={bad}");
        assert!(
            Args::try_parse_from(["irsync", a.as_str(), "a", "h:m"]).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn idle_timeout_takes_a_duration_and_parallel_zero_is_rejected() {
    let secs = std::time::Duration::from_secs;
    assert_eq!(opts(&["a", "h:m"]).idle_timeout, None);
    // 0 is kept: it overrides [defaults] idle_timeout, and means none.
    assert_eq!(
        opts(&["--idle-timeout=0", "a", "h:m"]).idle_timeout,
        Some(secs(0))
    );
    assert_eq!(
        opts(&["--idle-timeout=5", "a", "h:m"]).idle_timeout,
        Some(secs(5))
    );
    assert_eq!(
        opts(&["--idle-timeout", "2m", "a", "h:m"]).idle_timeout,
        Some(secs(120))
    );
    assert!(Args::try_parse_from(["irsync", "--idle-timeout=x", "a", "h:m"]).is_err());
    assert!(Args::try_parse_from(["irsync", "--timeout=5", "a", "h:m"]).is_err());
    assert!(Args::try_parse_from(["irsync", "--parallel=0", "a", "h:m"]).is_err());
    assert_eq!(opts(&["--parallel=3", "a", "h:m"]).parallel, Some(3));
}

#[test]
fn irsync_flags_and_misc() {
    let o = opts(&[
        "-vv",
        "--reconnect=30s",
        "--dial-retries=2",
        "--parallel",
        "4",
        "--idle-timeout=9s",
        "--config",
        "cfg",
        "--no-dht",
        "--no-dns",
        "--no-mdns",
        "--max-delete=5",
        "--partial-dir=.p",
        "a",
        "h:m",
    ]);
    assert_eq!(o.verbose, 2);
    assert_eq!(o.reconnect, Some(Duration::from_secs(30)));
    assert_eq!(o.dial_retries, Some(2));
    assert_eq!(o.parallel, Some(4));
    assert_eq!(o.idle_timeout, Some(Duration::from_secs(9)));
    assert_eq!(o.config, Some(PathBuf::from("cfg")));
    assert!(o.no_dht && o.no_dns && o.no_mdns);
    assert!(!o.numeric_ids, "names are mapped unless asked not to");
    let plain = opts(&["a", "h:m"]);
    assert!(!plain.no_dht && !plain.no_dns && !plain.no_mdns);
    // pair's `--no-dht` is for its connection check, as for a sync;
    // `--server-no-dht` is the one for the server's publishing.
    match parse(&["pair", "--no-dht", "--address", "127.0.0.1:1", "h"]).cmd {
        Some(Command::Pair(p)) => {
            assert!(p.conn.no_dht && !p.server_no_dht);
            assert_eq!(p.conn.address.len(), 1);
        }
        c => panic!("not Pair: {c:?}"),
    }
    assert!(opts(&["--numeric-ids", "a", "h:m"]).numeric_ids);
    assert!(!o.fsync, "no flush unless asked, as in rsync");
    assert!(opts(&["--fsync", "a", "h:m"]).fsync);
    assert!(!o.paranoid, "no second hash unless asked");
    assert!(opts(&["--paranoid", "a", "h:m"]).paranoid);
    assert_eq!(o.max_delete, Some(5));
    assert_eq!(o.partial_dir, Some(PathBuf::from(".p")));
}

#[test]
fn subcommands_parse() {
    assert!(matches!(
        parse(&["gc", "--dry-run"]).cmd,
        Some(Command::Gc { dry_run: true, .. })
    ));
    // `keygen` is gone; a lone word is a sync missing its DEST.
    assert!(Args::try_parse_from(["irsync", "keygen"]).is_err());
    let a = parse(&["-av", "a", "b", "h:m/"]);
    assert!(matches!(a.cmd, Some(Command::Sync(_))));
    assert_eq!(a.sync_args().src(), vec!["a", "b"]);
    assert_eq!(a.sync_args().dest(), Some("h:m/"));
}

#[test]
fn daemon_flags() {
    let a = parse(&["daemon", "--config", "/c"]);
    assert!(matches!(a.cmd, Some(Command::Daemon(_))));
    assert_eq!(a.files.config, Some("/c".into()));
    assert!(Args::try_parse_from(["irsync", "daemon", "--foreground"]).is_err());
    // `id --daemon` is the one way to print the daemon's id.
    assert!(Args::try_parse_from(["irsync", "daemon", "--show-id"]).is_err());
}

#[test]
fn index_takes_the_file_flags_and_exports_in_any_position() {
    for args in [
        &["index", "--config", "/c", "--export", "photos"][..],
        &["index", "--export", "photos", "--config", "/c"][..],
        &["--config", "/c", "index", "--export=photos"][..],
    ] {
        let a = parse(args);
        let Some(Command::Index(ix)) = a.cmd else {
            panic!("{args:?}")
        };
        assert_eq!(a.files.config, Some(PathBuf::from("/c")));
        assert_eq!(ix.exports, ["photos"]);
        assert!(ix.targets.is_empty() && !ix.dry_run);
    }
    // `export update` is gone: `index --export` hashes exports.
    assert!(Args::try_parse_from(["irsync", "export", "update", "photos"]).is_err());
    // The old spelling is gone: `index` has no --daemon and no --exports;
    // its --config names the file to read `state_dir` from, as elsewhere.
    assert!(Args::try_parse_from(["irsync", "index", "--daemon", "photos"]).is_err());
    assert!(Args::try_parse_from(["irsync", "index", "--exports", "/e", "photos"]).is_err());
    let a = parse(&["index", "--config", "/c", "/srv/a"]);
    let Some(Command::Index(ix)) = a.cmd else {
        panic!("index --config")
    };
    assert_eq!(a.files.config, Some(PathBuf::from("/c")));
    assert_eq!(ix.targets, [PathBuf::from("/srv/a")]);
}

#[test]
fn sync_requires_positionals() {
    assert!(Args::try_parse_from(["irsync", "-a"]).is_err());
    assert!(Args::try_parse_from(["irsync", "a"]).is_err());
}

#[test]
fn remote_path_is_passed_as_typed() {
    let cases: [(&str, &[u8], bool); 7] = [
        ("home:photos/2024", b"photos/2024", false),
        ("home:photos/2024/", b"photos/2024", true),
        ("home:/srv/photos", b"/srv/photos", false),
        ("home:/srv/photos/", b"/srv/photos", true),
        ("home:", b"", false),
        ("home:/", b"/", true),
        ("home:~/x", b"~/x", false),
    ];
    for (s, path, co) in cases {
        match Location::parse(s).unwrap() {
            Location::Remote {
                host,
                path: p,
                contents_only,
            } => {
                assert_eq!(
                    (host.as_str(), p.as_slice(), contents_only),
                    ("home", path, co),
                    "{s}"
                );
            }
            l => panic!("{s}: {l:?}"),
        }
    }
    assert!(Location::parse(":x").is_err());
    // Validity (`..`, NUL) is the daemon's call now; the client passes it on.
    assert!(matches!(
        Location::parse("home:m/../x").unwrap(),
        Location::Remote { .. }
    ));
}

/// On Windows a letter, `:` and a separator (or nothing more) is a local
/// drive path; everywhere else, and on Unix always, the usual rule holds.
#[test]
fn a_drive_path_is_local_on_windows_only() {
    use irsync::location::is_drive_path;
    use irsync::platform::Platform::{Unix, Windows};
    let remote = |s: &str, p| match Location::parse_on(s, p).unwrap() {
        Location::Remote { host, path, .. } => Some((host, String::from_utf8(path).unwrap())),
        Location::Local { .. } => None,
    };
    let local = |s: &str, p| match Location::parse_on(s, p).unwrap() {
        Location::Local {
            path,
            contents_only,
        } => (path, contents_only),
        l => panic!("{s}: {l:?}"),
    };
    let r = |h: &str, p: &str| Some((h.to_string(), p.to_string()));
    for (s, drive) in [
        (r"C:\x", true),
        ("c:/x", true),
        ("C:", true),
        (r"z:\", true),
        ("C:x", false),
        (r"host:C:\x", false),
        ("[::1]:path", false),
        ("h:photos", false),
        ("1:/x", false),
        ("CC:/x", false),
        ("", false),
    ] {
        assert_eq!(is_drive_path(s, Windows), drive, "{s:?}");
        assert!(!is_drive_path(s, Unix), "{s:?}");
    }
    // Windows: drive paths are local, the rest as on Unix.
    assert_eq!(local(r"C:\x", Windows), (PathBuf::from(r"C:\x"), false));
    assert_eq!(local("c:/x/", Windows), (PathBuf::from("c:/x"), true));
    assert_eq!(local("C:", Windows), (PathBuf::from("C:"), false));
    // The root of a drive keeps its slash: `C:` alone is another place.
    assert_eq!(local("C:/", Windows), (PathBuf::from("C:/"), true));
    assert_eq!(local("C://", Windows), (PathBuf::from("C:/"), true));
    assert_eq!(remote("C:x", Windows), r("C", "x"));
    assert_eq!(remote(r"host:C:\x", Windows), r("host", r"C:\x"));
    assert_eq!(remote("[::1]:path", Windows), r("::1", "path"));
    assert_eq!(remote("h:photos/x", Windows), r("h", "photos/x"));
    // Unix: unchanged, so a drive path is `HOST:PATH` there.
    assert_eq!(remote(r"C:\x", Unix), r("C", r"\x"));
    assert_eq!(remote("c:/x", Unix), r("c", "/x"));
    assert_eq!(remote("C:", Unix), r("C", ""));
    assert_eq!(remote("C:x", Unix), r("C", "x"));
    assert_eq!(remote(r"host:C:\x", Unix), r("host", r"C:\x"));
    assert_eq!(remote("[::1]:path", Unix), r("::1", "path"));
    assert_eq!(remote("h:photos", Unix), r("h", "photos"));
    assert_eq!(local("/x/", Unix), (PathBuf::from("/x"), true));
    // Windows: `\` separates as `/` does, so a `:` after one is no host,
    // and a trailing one is contents only, for a local path.
    assert_eq!(
        local(r"\\?\C:\x", Windows),
        (PathBuf::from(r"\\?\C:\x"), false)
    );
    assert_eq!(
        local(r"\\server\share\x", Windows),
        (PathBuf::from(r"\\server\share\x"), false)
    );
    assert_eq!(local(r".\C:x", Windows), (PathBuf::from(r".\C:x"), false));
    assert_eq!(local(r"..\h:x", Windows), (PathBuf::from(r"..\h:x"), false));
    assert_eq!(local(r"C:\src\", Windows), (PathBuf::from(r"C:\src"), true));
    assert_eq!(local(r"src\", Windows), (PathBuf::from("src"), true));
    assert_eq!(local(r"src/\", Windows), (PathBuf::from("src"), true));
    assert_eq!(local(r"C:\", Windows), (PathBuf::from(r"C:\"), true));
    assert_eq!(local(r"\", Windows), (PathBuf::from("/"), true));
    // A remote path is passed on as typed: only `/` is contents only.
    match Location::parse_on(r"h:x\", Windows).unwrap() {
        Location::Remote {
            path,
            contents_only,
            ..
        } => assert_eq!((path, contents_only), (br"x\".to_vec(), false)),
        l => panic!("{l:?}"),
    }
    // Unix: a backslash is part of a name.
    assert_eq!(remote(r".\C:x", Unix), r(r".\C", "x"));
    assert_eq!(remote(r"\\?\C:\x", Unix), r(r"\\?\C", r"\x"));
    assert_eq!(local(r"src\", Unix), (PathBuf::from(r"src\"), false));
    assert_eq!(
        local(r"\\server\share\x", Unix),
        (PathBuf::from(r"\\server\share\x"), false)
    );
    // A listing has no local side: there `h:` is the host, as on Unix.
    assert_eq!(
        Location::parse_listed("h:").unwrap(),
        Location::parse_on("h:", Unix).unwrap()
    );
    assert!(Location::parse_listed("h:").unwrap().is_remote());
    assert_eq!(
        Location::parse_listed("h:m/").unwrap(),
        Location::parse("h:m/").unwrap()
    );
    // `parse` is `parse_on` this system.
    assert_eq!(
        Location::parse(r"C:\x").unwrap(),
        Location::parse_on(r"C:\x", irsync::platform::Platform::CURRENT).unwrap()
    );
}

/// `irsync C:` alone: on Windows the drive, so a sync without a
/// destination; on Unix the host `C`, listed. `irsync list h` and
/// `irsync list h:` list the host `h` everywhere.
#[test]
fn a_lone_drive_is_no_listing_on_windows_but_list_h_is() {
    let windows = irsync::platform::Platform::CURRENT == irsync::platform::Platform::Windows;
    let lone = Args::try_parse_from(["irsync", "C:"]);
    if windows {
        assert!(lone.is_err());
    } else {
        assert!(matches!(lone.unwrap().cmd, Some(Command::List(_))));
    }
    for op in ["h", "h:"] {
        let a = parse(&["list", op]);
        assert!(matches!(a.cmd, Some(Command::List(_))), "{op}");
        assert_eq!(a.sync_args().paths, ["h:"], "{op}");
    }
}

/// Usage names the program: on Windows always `irsync` (never
/// `irsync.exe`); on Unix as it was started, as before.
#[test]
fn usage_names_the_program_as_each_system_expects() {
    let usage = |argv0: &str| {
        Args::try_parse_from([argv0, "list"])
            .unwrap_err()
            .to_string()
    };
    if irsync::platform::Platform::CURRENT == irsync::platform::Platform::Windows {
        assert!(usage(r"C:\x\irsync.exe").contains("Usage: irsync list"));
        assert!(usage(r"C:\x\irsync-beta.exe").contains("Usage: irsync list"));
    } else {
        assert!(usage("/opt/x/irsync").contains("Usage: irsync list"));
        assert!(usage("/opt/x/irsync-beta").contains("Usage: irsync-beta list"));
    }
}

#[test]
fn bracketed_ipv6_host() {
    let host = |s: &str| match Location::parse(s).unwrap() {
        Location::Remote { host, path, .. } => (host, path),
        l => panic!("{l:?}"),
    };
    assert_eq!(host("[fe80::1]:/p"), ("fe80::1".into(), b"/p".to_vec()));
    assert_eq!(
        host("mike@[::1]:dir/"),
        ("mike@::1".into(), b"dir".to_vec())
    );
    assert_eq!(host("[fe80::1%eth0]:").0, "fe80::1%eth0");
    // Not an address in the brackets, or no closing bracket:
    assert!(Location::parse("[myserver]:/p").is_err());
    assert!(Location::parse("[fe80::1:/p").is_err());
    // A local path may still start with a bracket.
    assert!(!Location::parse("[fe80::1]/p").unwrap().is_remote());
    assert!(!Location::parse("[notes]/a").unwrap().is_remote());
    // Also with a `:` inside the brackets, and behind `./`.
    for local in ["[a:b]/c", "[a:b]", "mike@[a:b]/c", "./[myserver]:x"] {
        assert!(!Location::parse(local).unwrap().is_remote(), "{local}");
    }
}

#[test]
fn location_local_with_colon_after_slash_is_local() {
    let local = |p: &str, c: bool| Location::Local {
        path: PathBuf::from(p),
        contents_only: c,
    };
    assert_eq!(loc("./a:b"), local("./a:b", false));
    assert_eq!(loc("dir/"), local("dir", true));
    assert_eq!(loc("/"), local("/", true));
    assert_eq!(loc("."), local(".", false));
}

#[test]
fn endpoint_id_literal_is_remote_host() {
    let id = iroh::SecretKey::from_bytes(&[7u8; 32]).public().to_string();
    match loc(&format!("{id}:mod/x")) {
        Location::Remote { host, path, .. } => {
            assert_eq!(host, id);
            assert_eq!(path, b"mod/x");
        }
        l => panic!("{l:?}"),
    }
}

#[test]
fn two_remotes_rejected() {
    assert!(validate(&[loc("a:m")], &loc("b:m")).is_err());
}

#[test]
fn no_remote_rejected() {
    assert!(validate(&[loc("a")], &loc("b")).is_err());
}

#[test]
fn directions() {
    assert_eq!(
        validate(&[loc("a")], &loc("h:m")).unwrap(),
        Direction::ClientSends
    );
    assert_eq!(
        validate(&[loc("h:m")], &loc("a")).unwrap(),
        Direction::DaemonSends
    );
}

#[test]
fn multiple_local_sources_to_remote_ok() {
    assert_eq!(
        validate(&[loc("a"), loc("b/")], &loc("h:m")).unwrap(),
        Direction::ClientSends
    );
}

#[test]
fn multiple_sources_with_remote_source_rejected() {
    assert!(validate(&[loc("h:m"), loc("h:n")], &loc("a")).is_err());
    assert!(validate(&[loc("a"), loc("h:m")], &loc("h:n")).is_err());
    assert!(validate(&[loc("a"), loc("b")], &loc("c")).is_err());
}

#[test]
fn hosts_and_exports_flags_are_gone() {
    for args in [
        &["daemon", "--hosts", "h"][..],
        &["daemon", "--exports", "e"],
        &["server", "list", "--hosts", "h"],
        &["grant", "list", "--exports", "e"],
        &["show", "--hosts", "h"],
        &["-a", "--hosts", "h", "a", "b:c"],
    ] {
        assert!(
            Args::try_parse_from([&["irsync"][..], args].concat()).is_err(),
            "{args:?}"
        );
    }
    let a = parse(&["daemon"]);
    assert!(matches!(a.cmd, Some(Command::Daemon(_))) && a.files.config.is_none());
}

#[test]
fn daemon_root_flag_parses() {
    assert!(matches!(parse(&["daemon"]).cmd, Some(Command::Daemon(ref d)) if !d.run_as_root));
    assert!(matches!(
        parse(&["daemon", "--run-as-root-i-know-what-i-am-doing"]).cmd,
        Some(Command::Daemon(ref d)) if d.run_as_root
    ));
}

#[test]
fn daemon_listen_flag_repeats() {
    match parse(&["daemon", "--listen", "127.0.0.1:0", "--listen", "[::1]:0"]).cmd {
        Some(Command::Daemon(d)) => assert_eq!(d.listen.len(), 2),
        c => panic!("{c:?}"),
    }
    assert!(Args::try_parse_from(["irsync", "daemon", "--listen", "nope"]).is_err());
}

#[test]
fn address_flag_repeats_and_conflicts_with_relay() {
    let o = opts(&[
        "--address",
        "127.0.0.1:9",
        "--address",
        "localhost:9",
        "a",
        "h:m",
    ]);
    assert_eq!(o.addresses.len(), 2);
    assert!(Args::try_parse_from([
        "irsync",
        "--address",
        "127.0.0.1:9",
        "--relay",
        "https://r",
        "a",
        "h:m"
    ])
    .is_err());
    assert!(Args::try_parse_from(["irsync", "--address", "127.0.0.1", "a", "h:m"]).is_err());
    assert!(
        opts(&["--address", "127.0.0.1:9", "--no-dht", "a", "h:m"]).no_dht,
        "redundant --no-dht is accepted"
    );
}

#[test]
fn relay_takes_default_disabled_or_a_url() {
    use irsync::config::settings::Relay;
    assert_eq!(
        opts(&["--relay", "disabled", "a", "h:m"]).relay,
        Some(Relay::Disabled)
    );
    assert_eq!(
        opts(&["--relay", "default", "a", "h:m"]).relay,
        Some(Relay::Default)
    );
    assert_eq!(
        opts(&["--relay", "https://r", "a", "h:m"]).relay,
        Some(Relay::Url("https://r".parse().unwrap()))
    );
    assert_eq!(opts(&["a", "h:m"]).relay, None);
    assert!(Args::try_parse_from(["irsync", "--relay", "maybe", "a", "h:m"]).is_err());
    assert!(Args::try_parse_from(["irsync", "--no-relay", "a", "h:m"]).is_err());
    // server add takes the same values.
    let a = Args::try_parse_from([
        "irsync",
        "server",
        "add",
        "home",
        "k51",
        "--relay",
        "disabled",
        "--dial-retries",
        "1",
    ])
    .unwrap();
    match a.cmd {
        Some(Command::Server(h)) => match h.cmd.unwrap() {
            irsync::cli::ServerCmd::Add(add) => {
                assert_eq!(add.relay, Some(Relay::Disabled));
                assert_eq!(add.dial_retries, Some(1));
            }
            c => panic!("{c:?}"),
        },
        c => panic!("{c:?}"),
    }
    for gone in ["--relay-mode", "--no-relay"] {
        assert!(
            Args::try_parse_from(["irsync", "server", "add", "home", "k51", gone, "disabled"])
                .is_err(),
            "{gone}"
        );
    }
}

#[test]
fn index_parses_local_roots_and_requires_one() {
    let a = parse(&["index", "-n", "/srv/a", "/srv/b"]);
    match a.cmd {
        Some(Command::Index(ix)) => {
            assert!(ix.dry_run);
            assert_eq!(
                ix.targets,
                [PathBuf::from("/srv/a"), PathBuf::from("/srv/b")]
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(Args::try_parse_from(["irsync", "index"]).is_err());
    assert!(Args::try_parse_from(["irsync", "index", "-n"]).is_err());
}

#[test]
fn index_parses_exports_and_roots_together() {
    let a = parse(&[
        "index", "-n", "-v", "--export", "photos", "--export", "music", "/srv/a",
    ]);
    match a.cmd {
        Some(Command::Index(ix)) => {
            assert_eq!(ix.exports, ["photos", "music"]);
            assert_eq!(ix.targets, [PathBuf::from("/srv/a")]);
            assert!(ix.dry_run && a.log.verbose == 1 && !a.log.quiet);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        parse(&["index", "--export", "all"]).cmd,
        Some(Command::Index(ref ix)) if ix.exports == ["all"] && ix.targets.is_empty()
    ));
    assert!(Args::try_parse_from(["irsync", "index", "--export"]).is_err());
}

#[test]
fn nouns_share_add_remove_list() {
    use irsync::cli::{ClientCmd, ExportCmd, GrantCmd, ServerCmd};
    assert!(matches!(
        parse(&["export", "add", "photos:/srv/photos", "--read-only", "--index-at-start"]).cmd,
        Some(Command::Export(ref e))
            if matches!(e.cmd, Some(ExportCmd::Add(ref a)) if a.target == "photos:/srv/photos" && a.read_only && a.index_at_start)
    ));
    assert!(matches!(
        parse(&["export", "remove", "photos"]).cmd,
        Some(Command::Export(ref e)) if matches!(e.cmd, Some(ExportCmd::Remove { ref name }) if name == "photos")
    ));
    assert!(matches!(
        parse(&["grant", "add", "photos", "laptop", "--ro"]).cmd,
        Some(Command::Grant(ref g))
            if matches!(g.cmd, Some(GrantCmd::Add { ref export, ref who, ro: true }) if export == "photos" && who == "laptop")
    ));
    assert!(matches!(
        parse(&["grant", "remove", "all", "all"]).cmd,
        Some(Command::Grant(ref g)) if matches!(g.cmd, Some(GrantCmd::Remove { .. }))
    ));
    assert!(matches!(
        parse(&["server", "remove", "home"]).cmd,
        Some(Command::Server(ref h)) if matches!(h.cmd, Some(ServerCmd::Remove { ref name }) if name == "home")
    ));
    assert!(matches!(
        parse(&["server", "rename", "home", "work"]).cmd,
        Some(Command::Server(ref h))
            if matches!(h.cmd, Some(ServerCmd::Rename { ref old, ref new }) if old == "home" && new == "work")
    ));
    assert!(matches!(
        parse(&["server", "list"]).cmd,
        Some(Command::Server(ref h)) if matches!(h.cmd, Some(ServerCmd::List))
    ));
    assert!(matches!(
        parse(&["client", "add", "home", "abc"]).cmd,
        Some(Command::Client(ref h))
            if matches!(h.cmd, Some(ClientCmd::Add { ref name, ref id }) if name == "home" && id == "abc")
    ));
    assert!(matches!(
        parse(&["client", "remove", "home"]).cmd,
        Some(Command::Client(ref h)) if matches!(h.cmd, Some(ClientCmd::Remove { ref name, force: false }) if name == "home")
    ));
    for flag in ["-f", "--force"] {
        assert!(matches!(
            parse(&["client", "remove", "home", flag]).cmd,
            Some(Command::Client(ref h)) if matches!(h.cmd, Some(ClientCmd::Remove { force: true, .. }))
        ));
    }
    assert!(matches!(
        parse(&["client", "rename", "home", "work"]).cmd,
        Some(Command::Client(ref h))
            if matches!(h.cmd, Some(ClientCmd::Rename { ref old, ref new }) if old == "home" && new == "work")
    ));
    assert!(matches!(
        parse(&["client", "list"]).cmd,
        Some(Command::Client(ref h)) if matches!(h.cmd, Some(ClientCmd::List))
    ));
    // `client add` has no flags of a server; `server remove` has no --force.
    assert!(
        Args::try_parse_from(["irsync", "client", "add", "a", "b", "--parallel", "2"]).is_err()
    );
    assert!(Args::try_parse_from(["irsync", "server", "remove", "a", "-f"]).is_err());
    assert!(
        Args::try_parse_from(["irsync", "server", "add", "a", "b", "--client-id", "c"]).is_err()
    );
    assert!(matches!(
        parse(&["export", "list"]).cmd,
        Some(Command::Export(ref e)) if matches!(e.cmd, Some(ExportCmd::List))
    ));
    assert!(matches!(
        parse(&["grant", "list"]).cmd,
        Some(Command::Grant(ref g)) if matches!(g.cmd, Some(GrantCmd::List { target: None }))
    ));
    assert!(matches!(
        parse(&["grant", "list", "photos"]).cmd,
        Some(Command::Grant(ref g)) if matches!(g.cmd, Some(GrantCmd::List { target: Some(ref t) }) if t == "photos")
    ));
    assert!(Args::try_parse_from(["irsync", "grant", "list", "a", "b"]).is_err());
    // server add keeps every flag
    let a = parse(&[
        "server",
        "add",
        "home",
        "abc",
        "--relay",
        "https://r",
        "--address",
        "1.2.3.4:1",
        "--address",
        "h:2",
        "--parallel",
        "8",
        "--reconnect",
        "2h",
        "--dht",
        "no",
        "--mdns",
        "yes",
        "--pq-key-exchange",
        "require",
    ]);
    let Some(Command::Server(h)) = a.cmd else {
        panic!()
    };
    let Some(ServerCmd::Add(h)) = h.cmd else {
        panic!()
    };
    assert!(
        h.name == "home"
            && h.id == "abc"
            && h.address.len() == 2
            && h.dht == Some(false)
            && h.mdns == Some(true)
            && h.pq_key_exchange == Some(irsync::config::settings::PqKeyExchange::Require)
    );
    assert!(
        Args::try_parse_from(["irsync", "server", "add", "home", "abc", "--dht", "maybe"]).is_err()
    );
    assert!(Args::try_parse_from([
        "irsync",
        "server",
        "add",
        "home",
        "abc",
        "--pq-key-exchange",
        "sometimes"
    ])
    .is_err());
}

#[test]
fn grant_positionals_are_always_two() {
    for args in [
        vec!["grant", "remove", "laptop"],
        vec!["grant", "remove"],
        vec!["grant", "add", "photos"],
        vec!["grant", "remove", "a", "b", "c"],
    ] {
        let mut full = vec!["irsync"];
        full.extend(args.iter());
        assert!(Args::try_parse_from(full).is_err(), "{args:?}");
    }
}

#[test]
fn old_bare_verbs_are_gone() {
    // No subcommand is named so: the words are the two paths of a sync.
    for v in ["add", "remove", "allow", "revoke"] {
        let a = parse(&[v, "h:photos"]);
        assert!(a.sync_args().paths == vec![v, "h:photos"], "{v}: {a:?}");
    }
}

#[test]
fn file_flags_are_accepted_after_the_noun_in_either_position() {
    let after_noun = parse(&["server", "--config", "/c", "list"]);
    let after_verb = parse(&["server", "list", "--config", "/c"]);
    for a in [after_noun, after_verb] {
        assert!(matches!(a.cmd, Some(Command::Server(_))));
        assert_eq!(a.files.config, Some("/c".into()));
    }
    let a = parse(&["grant", "add", "photos", "laptop", "--config", "/c"]);
    assert!(matches!(a.cmd, Some(Command::Grant(_))));
    assert_eq!(a.files.config, Some("/c".into()));
    // Before the noun it is the same flag.
    let a = parse(&["--config", "/c", "server", "list"]);
    assert!(matches!(a.cmd, Some(Command::Server(_))), "{a:?}");
    assert_eq!(a.files.config, Some("/c".into()));
}

#[test]
fn id_and_show_parse() {
    use irsync::cli::IdCmd;
    assert!(matches!(
        parse(&["id", "show"]).cmd,
        Some(Command::Id(ref i)) if matches!(i.cmd, Some(IdCmd::Show { daemon: false, all: false, .. }))
    ));
    assert!(matches!(
        parse(&["id", "show", "--daemon"]).cmd,
        Some(Command::Id(ref i)) if matches!(i.cmd, Some(IdCmd::Show { daemon: true, all: false, .. }))
    ));
    assert!(matches!(
        parse(&["id", "show", "-a"]).cmd,
        Some(Command::Id(ref i)) if matches!(i.cmd, Some(IdCmd::Show { daemon: false, all: true, .. }))
    ));
    // The file flags belong to the noun, so they go on either side of the verb.
    for args in [
        &["id", "show", "--config", "/c"][..],
        &["id", "--config", "/c", "show"][..],
    ] {
        let a = parse(args);
        assert!(matches!(a.cmd, Some(Command::Id(_))), "{args:?}");
        assert_eq!(a.files.config, Some(PathBuf::from("/c")));
    }
    assert!(Args::try_parse_from(["irsync", "id", "show", "--daemon", "--all"]).is_err());
    assert!(matches!(
        parse(&["id", "generate"]).cmd,
        Some(Command::Id(ref i)) if matches!(i.cmd, Some(IdCmd::Generate { daemon: false, out: None, force: false }))
    ));
    assert!(matches!(
        parse(&["id", "generate", "--daemon"]).cmd,
        Some(Command::Id(ref i)) if matches!(i.cmd, Some(IdCmd::Generate { daemon: true, out: None, force: false }))
    ));
    for args in [
        &["id", "generate", "--force"][..],
        &["id", "generate", "-f", "--daemon"][..],
    ] {
        assert!(
            matches!(
                parse(args).cmd,
                Some(Command::Id(ref i)) if matches!(i.cmd, Some(IdCmd::Generate { force: true, .. }))
            ),
            "{args:?}"
        );
    }
    assert!(matches!(
        parse(&["id", "generate", "--out", "/k"]).cmd,
        Some(Command::Id(ref i))
            if matches!(i.cmd, Some(IdCmd::Generate { daemon: false, out: Some(ref p), .. }) if p == &PathBuf::from("/k"))
    ));
    let a = parse(&["id", "list", "--passphrase-file", "/p"]);
    assert!(matches!(a.cmd, Some(Command::Id(ref i)) if matches!(i.cmd, Some(IdCmd::List))));
    assert_eq!(a.files.passphrase_file, Some(PathBuf::from("/p")));
    // `list` has no roles to pick from: it prints what there is.
    assert!(Args::try_parse_from(["irsync", "id", "list", "--daemon"]).is_err());
    // `--out` names the file, also for a daemon key.
    assert!(matches!(
        parse(&["id", "generate", "--daemon", "--out", "/k"]).cmd,
        Some(Command::Id(ref i))
            if matches!(i.cmd, Some(IdCmd::Generate { daemon: true, out: Some(ref p), .. }) if p == &PathBuf::from("/k"))
    ));
    // A noun alone parses: it runs `list`.
    for noun in ["id", "server", "client", "export", "grant", "config"] {
        assert!(Args::try_parse_from(["irsync", noun]).is_ok(), "{noun}");
    }
    assert!(matches!(parse(&["id"]).cmd, Some(Command::Id(ref i)) if i.cmd.is_none()));
    assert!(Args::try_parse_from(["irsync", "id", "--daemon"]).is_err());
    let a = parse(&["show", "--config", "/c"]);
    assert!(matches!(a.cmd, Some(Command::Show)));
    assert_eq!(a.files.passphrase_file, None);
    // An encrypted identity can be unlocked from a file, as for a sync.
    let a = parse(&["id", "show", "--daemon", "--passphrase-file", "/p"]);
    assert!(
        matches!(a.cmd, Some(Command::Id(ref i)) if matches!(i.cmd, Some(IdCmd::Show { daemon: true, .. })))
    );
    assert_eq!(a.files.passphrase_file, Some(PathBuf::from("/p")));
    let a = parse(&["show", "--passphrase-file", "/p"]);
    assert!(matches!(a.cmd, Some(Command::Show)));
    assert_eq!(a.files.passphrase_file, Some(PathBuf::from("/p")));
}

#[test]
fn one_remote_operand_is_a_listing() {
    assert_eq!(list_paths(&parse(&["home:"])), ["home:"]);
    assert_eq!(
        list_paths(&parse(&["-v", "home:photos/"])),
        ["home:photos/"]
    );
    // A single local path is still a usage error.
    assert!(Args::try_parse_from(["irsync", "./x"]).is_err());
    assert!(Args::try_parse_from(["irsync"]).is_err());
}

#[test]
fn config_noun_parses() {
    use irsync::cli::ConfigCmd;
    assert!(matches!(
        parse(&["config", "get", "daemon.listen"]).cmd,
        Some(Command::Config(ref c)) if matches!(c.cmd, Some(ConfigCmd::Get { ref key }) if key == "daemon.listen")
    ));
    assert!(matches!(
        parse(&["config", "set", "daemon.listen", "a:1", "b:2"]).cmd,
        Some(Command::Config(ref c)) if matches!(c.cmd, Some(ConfigCmd::Set { ref values, .. }) if values == &["a:1", "b:2"])
    ));
    assert!(matches!(
        parse(&["config", "set", "all.rw"]).cmd,
        Some(Command::Config(ref c)) if matches!(c.cmd, Some(ConfigCmd::Set { ref values, .. }) if values.is_empty())
    ));
    assert!(matches!(
        parse(&["config", "unset", "x"]).cmd,
        Some(Command::Config(ref c)) if matches!(c.cmd, Some(ConfigCmd::Unset { ref key }) if key == "x")
    ));
    assert!(matches!(
        parse(&["config", "--config", "/c", "list"]).cmd,
        Some(Command::Config(ref c)) if matches!(c.cmd, Some(ConfigCmd::List))
    ));
    assert_eq!(
        parse(&["config", "--config", "/c", "list"]).files.config,
        Some("/c".into())
    );
    // `config` alone is `config list`.
    assert!(matches!(
        parse(&["config"]).cmd,
        Some(Command::Config(ref c)) if c.cmd.is_none()
    ));
    assert!(Args::try_parse_from(["irsync", "config", "get"]).is_err());
}

#[test]
fn debug_addr_bare_port_is_loopback() {
    let o = opts(&["--debug-addr", "9464", "a", "h:b"]);
    assert_eq!(o.debug_addr, Some("127.0.0.1:9464".parse().unwrap()));
    assert!(!o.debug_no_wait);
    let o = opts(&[
        "--debug-addr",
        "0.0.0.0:1",
        "--debug-port-no-wait",
        "a",
        "h:b",
    ]);
    assert_eq!(o.debug_addr, Some("0.0.0.0:1".parse().unwrap()));
    assert!(o.debug_no_wait);
    assert!(Args::try_parse_from(["irsync", "--debug-addr", "x", "a", "h:b"]).is_err());
}

#[test]
fn no_wait_requires_debug_addr() {
    let home = tempfile::tempdir().unwrap();
    let out = bin::irsync_command(home.path())
        .args(["--debug-port-no-wait", "a", "b"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--debug-addr"), "{err}");
}

#[test]
fn progress_block_is_asked_for_with_progress_p_or_info_progress() {
    assert!(!opts(&["a/", "h:b/"]).progress);
    assert!(!opts(&["-v", "a/", "h:b/"]).progress);
    assert!(opts(&["--progress", "a/", "h:b/"]).progress);
    assert!(opts(&["-P", "a/", "h:b/"]).progress);
    assert!(opts(&["-avP", "a/", "h:b/"]).progress);
    assert!(opts(&["--info=progress2", "a/", "h:b/"]).progress);
    assert!(opts(&["--info=flist0,progress1", "a/", "h:b/"]).progress);
    assert!(!opts(&["--info=progress0", "a/", "h:b/"]).progress);
    assert!(!opts(&["--info=stats2", "a/", "h:b/"]).progress);
}

fn err(a: &[&str]) -> String {
    let mut v = vec!["irsync"];
    v.extend_from_slice(a);
    Args::try_parse_from(v).unwrap_err().to_string()
}

#[test]
fn verbose_and_quiet_are_accepted_before_and_after_any_command() {
    let a = parse(&["-v", "gc"]);
    assert!(matches!(a.cmd, Some(Command::Gc { .. })));
    assert_eq!((a.log.verbose, a.log.quiet), (1, false));
    let a = parse(&["gc", "-n", "-q"]);
    assert!(matches!(a.cmd, Some(Command::Gc { dry_run: true, .. })));
    assert_eq!((a.log.verbose, a.log.quiet), (0, true));
    for args in [
        &["-vv", "id", "show"][..],
        &["id", "-vv", "show"],
        &["id", "show", "-vv"],
        &["id", "show", "--verbose", "--verbose"],
    ] {
        let a = parse(args);
        assert!(matches!(a.cmd, Some(Command::Id(_))), "{args:?}");
        assert_eq!(a.log.verbose, 2, "{args:?}");
    }
    assert!(parse(&["--quiet", "server", "list"]).log.quiet);
    assert_eq!(parse(&["daemon", "-vv"]).log.verbose, 2);
    assert_eq!(parse(&["index", "-v", "/srv/a"]).log.verbose, 1);
    assert_eq!(parse(&["-v", "show"]).log.verbose, 1);
    assert_eq!(
        parse(&["-v", "config", "get", "daemon.listen"]).log.verbose,
        1
    );
}

#[test]
fn verbose_with_quiet_is_accepted_everywhere() {
    for args in [
        &["daemon", "-v", "-q"][..],
        &["-q", "gc", "-v"],
        &["-vq", "src", "h:m/"],
    ] {
        let a = parse(args);
        assert_eq!((a.log.verbose, a.log.quiet), (1, true), "{args:?}");
    }
}

#[test]
fn a_sync_still_reads_verbose_and_quiet() {
    let o = opts(&["-avv", "src", "h:m/"]);
    assert_eq!((o.verbose, o.quiet), (2, false));
    assert!(opts(&["-q", "src", "h:m/"]).quiet);
    let a = parse(&["-v", "home:photos/"]);
    assert!(list_paths(&a) == ["home:photos/"] && a.log.verbose == 1);
}

#[test]
fn a_path_named_like_a_command_is_still_a_path() {
    let a = parse(&["-a", "myserver:photos/", "config"]);
    assert!(a.sync_args().paths == ["myserver:photos/", "config"]);
    let a = parse(&["h:src", "gc"]);
    assert!(a.sync_args().paths == ["h:src", "gc"]);
    let a = parse(&["-v", "./gc", "h:m/"]);
    assert!(a.sync_args().paths == ["./gc", "h:m/"]);
}

#[test]
fn config_is_accepted_before_and_after_any_command() {
    let f = Some(PathBuf::from("f"));
    for args in [
        &["--config", "f", "server", "list"][..],
        &["server", "--config", "f", "list"],
        &["server", "list", "--config", "f"],
        &["--config", "f", "daemon"],
        &["daemon", "--config", "f"],
        &["--config", "f", "gc"],
        &["index", "--config", "f", "/srv/a"],
        &["-v", "--config", "f", "id", "show"],
        &["show", "--config=f"],
    ] {
        let a = parse(args);
        assert!(a.cmd.is_some(), "{args:?}");
        assert_eq!(a.files.config, f, "{args:?}");
    }
    assert_eq!(opts(&["--config", "f", "src", "h:m/"]).config, f);
    assert_eq!(opts(&["src", "h:m/", "--config", "f"]).config, f);
    assert_eq!(parse(&["server", "list"]).files.config, None);
}

#[test]
fn identity_is_accepted_before_and_after_any_command() {
    for argv in [
        vec!["--identity", "k", "id", "show"],
        vec!["id", "show", "--identity", "k"],
        vec!["show", "--identity", "k"],
        vec!["pair", "--identity", "k", "h"],
        vec!["--identity", "k", "sync", "a", "h:m"],
        vec!["-a", "--identity", "k", "a", "h:m"],
        vec!["list", "h:", "--identity", "k"],
    ] {
        let mut full = vec!["irsync"];
        full.extend(argv.iter());
        let a = Args::try_parse_from(&full).unwrap_or_else(|e| panic!("{argv:?}: {e}"));
        assert_eq!(a.files.identity, Some(PathBuf::from("k")), "{argv:?}");
    }
    assert_eq!(
        opts(&["--identity", "k", "a", "h:m"]).identity,
        Some(PathBuf::from("k"))
    );
}

#[test]
fn passphrase_file_is_accepted_before_and_after_any_command() {
    let f = Some(PathBuf::from("p"));
    for args in [
        &["--passphrase-file", "p", "id", "show"][..],
        &["id", "--passphrase-file", "p", "show"],
        &["id", "show", "--passphrase-file", "p"],
        &["id", "list", "--passphrase-file", "p"],
        &["id", "generate", "--passphrase-file", "p"],
        &[
            "id",
            "generate",
            "--daemon",
            "--force",
            "--passphrase-file=p",
        ],
        &["show", "--passphrase-file", "p"],
        &["--passphrase-file", "p", "daemon"],
        &["daemon", "--passphrase-file", "p"],
        // Accepted and ignored where no key is loaded.
        &["gc", "--passphrase-file", "p"],
        &["--passphrase-file", "p", "server", "list"],
    ] {
        let a = parse(args);
        assert!(a.cmd.is_some(), "{args:?}");
        assert_eq!(a.files.passphrase_file, f, "{args:?}");
    }
    assert_eq!(
        opts(&["--passphrase-file", "p", "src", "h:m/"]).passphrase_file,
        f
    );
    assert_eq!(parse(&["id", "show"]).files.passphrase_file, None);
}

#[test]
fn a_sync_option_with_a_command_is_rejected_by_name() {
    assert!(err(&["-a", "gc"]).contains("-a is a sync option and cannot be used with 'gc'"));
    assert!(err(&["-n", "gc"]).contains("-n is a sync option and cannot be used with 'gc'"));
    assert!(
        err(&["-av", "id", "show"]).contains("-a is a sync option and cannot be used with 'id'")
    );
    assert!(err(&["--bwlimit", "5", "gc"])
        .contains("--bwlimit is a sync option and cannot be used with 'gc'"));
    let e = Args::try_parse_from(["irsync", "-a", "gc"]).unwrap_err();
    assert_eq!(e.exit_code(), 2);
    // After the command, the command's own flags are untouched.
    assert!(matches!(
        parse(&["gc", "-n"]).cmd,
        Some(Command::Gc { dry_run: true, .. })
    ));
    // `-v gc host:path` is `gc` with an argument it does not take.
    assert!(Args::try_parse_from(["irsync", "-v", "gc", "host:path"]).is_err());
}

#[test]
fn a_flag_between_paths_does_not_turn_a_later_path_into_a_command() {
    let a = parse(&["h:photos/", "-v", "gc"]);
    assert!(a.sync_args().paths == ["h:photos/", "gc"]);
    assert_eq!(a.log.verbose, 1);
    let a = parse(&["h:photos/", "-q", "config"]);
    assert!(a.sync_args().paths == ["h:photos/", "config"]);
    assert!(a.log.quiet);
    let a = parse(&["h:photos/", "--config", "f", "show"]);
    assert!(a.sync_args().paths == ["h:photos/", "show"]);
    assert_eq!(a.files.config, Some(PathBuf::from("f")));
    let a = parse(&["h:photos/", "-a", "config"]);
    assert!(a.sync_args().paths == ["h:photos/", "config"]);
    assert!(SyncOptions::from_args(&a).preserve.recursive);
    let a = parse(&["src", "-v", "gc", "h:m/"]);
    assert!(a.sync_args().paths == ["src", "gc", "h:m/"]);
    // A path whose command would fail to parse is still a path.
    let a = parse(&["src", "-v", "server", "h:add"]);
    assert!(a.sync_args().paths == ["src", "server", "h:add"]);
    // With no path first, a command name is the command.
    assert!(matches!(parse(&["-v", "gc"]).cmd, Some(Command::Gc { .. })));
    assert!(matches!(parse(&["gc", "-v"]).cmd, Some(Command::Gc { .. })));
    let a = parse(&["--config", "f", "server", "list"]);
    assert!(a.cmd.is_some() && a.sync.paths.is_empty());
    assert!(err(&["-a", "gc"]).contains("-a is a sync option and cannot be used with 'gc'"));
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/minisign")
        .join(name)
}

/// The base64 line of a fixture public key.
fn fixture_key(name: &str) -> String {
    let text = std::fs::read_to_string(fixture(name)).unwrap();
    text.lines().nth(1).unwrap().to_string()
}

fn verify_signature(args: &[&str]) -> std::process::Output {
    let home = tempfile::tempdir().unwrap();
    bin::irsync_command(home.path())
        .arg("verify-signature")
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn verify_signature_good() {
    let data = fixture("data.txt");
    let sig = fixture("data.txt.a.minisig");
    let key = fixture_key("a.pub");
    let o = verify_signature(&[
        "--signature",
        sig.to_str().unwrap(),
        "--public-key",
        &key,
        "--trusted-comment",
        "irsync v1.2.3",
        data.to_str().unwrap(),
    ]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{out} {}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(out.starts_with("good signature: "), "{out}");
    assert!(out.contains("trusted comment: irsync v1.2.3"), "{out}");
}

#[test]
fn verify_signature_bad_exit_1() {
    // Signed by key a, checked against key b.
    let data = fixture("data.txt");
    let sig = fixture("data.txt.a.minisig");
    let key = fixture_key("b.pub");
    let o = verify_signature(&[
        "--signature",
        sig.to_str().unwrap(),
        "--public-key",
        &key,
        data.to_str().unwrap(),
    ]);
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.starts_with("irsync: bad signature for "), "{err}");
}

#[test]
fn verify_signature_comment_mismatch_exit_1() {
    let data = fixture("data.txt");
    let sig = fixture("data.txt.a.minisig");
    let key = fixture_key("a.pub");
    let o = verify_signature(&[
        "--signature",
        sig.to_str().unwrap(),
        "--public-key",
        &key,
        "--trusted-comment",
        "irsync v9.9.9",
        data.to_str().unwrap(),
    ]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("irsync v9.9.9"));
}

#[test]
fn verify_signature_missing_sig_exit_1() {
    // No data.txt.minisig next to the fixture.
    let data = fixture("data.txt");
    let key = fixture_key("a.pub");
    let o = verify_signature(&["--public-key", &key, data.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).starts_with("irsync: bad signature for "));
}

#[test]
fn verify_signature_invalid_key_exit_2() {
    let data = fixture("data.txt");
    let o = verify_signature(&["--public-key", "not a key", data.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("public key"));
}

#[test]
fn verify_signature_empty_key_exit_2() {
    let data = fixture("data.txt");
    for key in ["", "  "] {
        let o = verify_signature(&["--public-key", key, data.to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(2));
        assert_eq!(
            String::from_utf8_lossy(&o.stderr),
            "irsync: --public-key: no key given\n"
        );
    }
}

#[test]
fn verify_signature_unreadable_file_exit_1() {
    let key = fixture_key("a.pub");
    let sig = fixture("data.txt.a.minisig");
    let o = verify_signature(&[
        "--signature",
        sig.to_str().unwrap(),
        "--public-key",
        &key,
        "/nonexistent/irsync-file",
    ]);
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.starts_with("irsync: cannot read /nonexistent/irsync-file: "),
        "{err}"
    );
}

#[test]
fn verify_signature_hidden_from_help() {
    let home = tempfile::tempdir().unwrap();
    let o = bin::irsync_command(home.path())
        .arg("--help")
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&o.stdout).contains("verify-signature"));
}

/// The help of `--parallel` names the default the code applies.
#[test]
fn parallel_help_names_the_default() {
    let home = tempfile::tempdir().unwrap();
    let want = format!("else {})", irsync::data_stream::DEFAULT_PARALLEL);
    for args in [&["--help"][..], &["server", "add", "--help"]] {
        let o = bin::irsync_command(home.path())
            .args(args)
            .output()
            .unwrap();
        let help = String::from_utf8_lossy(&o.stdout);
        let help = help.split_whitespace().collect::<Vec<_>>().join(" ");
        let line = help
            .split("--parallel <N>")
            .nth(1)
            .unwrap_or_else(|| panic!("{args:?}: no --parallel in {help}"));
        assert!(
            line[..200.min(line.len())].contains(&want),
            "{args:?}: {line}"
        );
    }
}

#[test]
fn verify_signature_help_mentions_minisign() {
    let home = tempfile::tempdir().unwrap();
    let o = bin::irsync_command(home.path())
        .args(["verify-signature", "--help"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&o.stdout).contains("minisign"));
}

#[test]
fn remote_export_set_and_rename_parse() {
    use irsync::cli::{RemoteCmd, RemoteExportCmd};
    let a = parse(&[
        "remote",
        "myserver",
        "export",
        "set",
        "photos",
        "read_only=true",
    ]);
    match &a.cmd {
        Some(Command::Remote(r)) => match &r.cmd {
            RemoteCmd::Export(e) => match &e.cmd {
                Some(RemoteExportCmd::Set { name, settings }) => {
                    assert_eq!(name, "photos");
                    assert_eq!(settings, &["read_only=true"]);
                }
                c => panic!("not Set: {c:?}"),
            },
            c => panic!("not Export: {c:?}"),
        },
        c => panic!("not Remote: {c:?}"),
    }
    let a = parse(&[
        "remote", "myserver", "export", "rename", "photos", "pictures",
    ]);
    assert!(matches!(
        &a.cmd,
        Some(Command::Remote(r)) if matches!(
            &r.cmd,
            RemoteCmd::Export(e) if matches!(
                &e.cmd,
                Some(RemoteExportCmd::Rename { old, new }) if old == "photos" && new == "pictures"
            )
        )
    ));
    let e = parse_err(&["remote", "myserver", "export", "set", "photos"]);
    assert!(e.contains("KEY=VALUE"), "{e}");
}

/// What the binary does right after parsing: a relative `--partial-dir`
/// stays relative (to the destination); an absolute one, and on Windows
/// one rooted without a drive (`\tmp.`), is read as this system means it.
#[test]
fn local_paths_keeps_a_relative_partial_dir_and_reads_a_rooted_one() {
    let partial = |a: &[&str]| {
        let mut args = parse(a);
        args.local_paths();
        args.sync_args().partial_dir.clone().unwrap()
    };
    assert_eq!(
        partial(&["--partial-dir", ".p", "a", "h:m"]),
        PathBuf::from(".p")
    );
    assert_eq!(
        partial(&["--partial-dir", "x/.p", "a", "h:m"]),
        PathBuf::from("x/.p")
    );
    #[cfg(windows)]
    {
        let got = partial(&["--partial-dir", r"\tmp.", "a", "h:m"]);
        assert!(got.is_absolute(), "{got:?}");
        assert_eq!(
            got,
            irsync::platform::local_path(std::path::Path::new(r"\tmp"))
        );
        assert!(!got.to_string_lossy().ends_with('.'), "{got:?}");
    }
}
