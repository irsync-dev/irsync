use irsync::cli::location::Location;
use irsync::cli::{validate, Args, Command, DeleteMode, Direction, FilterRule, SyncOptions};
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

#[test]
fn archive_expands_to_rlptgod() {
    let p = opts(&["-a", "src", "h:m/"]).preserve;
    assert!(p.recursive && p.links && p.perms && p.times && p.group && p.owner && p.devices);
    assert!(!p.hard_links && !p.sparse);
}

#[test]
fn recursion_is_implied_without_r_or_a() {
    assert!(opts(&["-t", "src", "h:m/"]).preserve.recursive);
    assert!(opts(&["src", "h:m/"]).preserve.recursive);
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
    assert_eq!(opts(&["--bwlimit=1gb", "a", "h:m"]).bwlimit, Some(1 << 30));
    assert_eq!(opts(&["--bwlimit=0", "a", "h:m"]).bwlimit, None);
    assert!(Args::try_parse_from(["irsync", "--bwlimit=x", "a", "h:m"]).is_err());
}

#[test]
fn irsync_flags_and_misc() {
    let o = opts(&[
        "-vv",
        "--reconnect=30s",
        "--parallel",
        "4",
        "--timeout=9",
        "-F",
        "cfg",
        "--no-dht",
        "--max-delete=5",
        "--partial-dir=.p",
        "a",
        "h:m",
    ]);
    assert_eq!(o.verbose, 2);
    assert_eq!(o.reconnect, Some(Duration::from_secs(30)));
    assert_eq!(o.parallel, Some(4));
    assert_eq!(o.timeout, Some(Duration::from_secs(9)));
    assert_eq!(o.config, Some(PathBuf::from("cfg")));
    assert!(o.no_dht && o.numeric_ids);
    assert_eq!(o.max_delete, Some(5));
    assert_eq!(o.partial_dir, Some(PathBuf::from(".p")));
}

#[test]
fn subcommands_parse() {
    match parse(&["daemon", "--show-id"]).cmd {
        Some(Command::Daemon(d)) => assert!(d.show_id && !d.foreground),
        c => panic!("{c:?}"),
    }
    assert!(matches!(
        parse(&["gc", "--dry-run"]).cmd,
        Some(Command::Gc { dry_run: true })
    ));
    assert!(matches!(
        parse(&["keygen"]).cmd,
        Some(Command::Keygen { out: None })
    ));
    let a = parse(&["-av", "a", "b", "h:m/"]);
    assert!(a.cmd.is_none());
    assert_eq!(a.sync.src(), vec!["a", "b"]);
    assert_eq!(a.sync.dest(), Some("h:m/"));
}

#[test]
fn sync_requires_positionals() {
    assert!(Args::try_parse_from(["irsync", "-a"]).is_err());
    assert!(Args::try_parse_from(["irsync", "a"]).is_err());
}

#[test]
fn location_remote_with_trailing_slash() {
    match loc("home:data/photos/") {
        Location::Remote {
            host,
            module,
            path,
            contents_only,
        } => {
            assert_eq!(host, "home");
            assert_eq!(module, "data");
            assert_eq!(path.as_bytes(), b"photos");
            assert!(contents_only);
        }
        l => panic!("{l:?}"),
    }
    match loc("home:data") {
        Location::Remote {
            path,
            contents_only,
            ..
        } => {
            assert_eq!(path.as_bytes(), b"");
            assert!(!contents_only);
        }
        l => panic!("{l:?}"),
    }
    assert!(Location::parse("home:").is_err());
    assert!(Location::parse("home:m/../x").is_err());
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
        Location::Remote { host, module, .. } => {
            assert_eq!(host, id);
            assert_eq!(module, "mod");
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
fn daemon_listen_flag_repeats() {
    match parse(&["daemon", "--listen", "127.0.0.1:0", "--listen", "[::1]:0"]).cmd {
        Some(Command::Daemon(d)) => assert_eq!(d.listen.len(), 2),
        c => panic!("{c:?}"),
    }
    assert!(Args::try_parse_from(["irsync", "daemon", "--listen", "nope"]).is_err());
}
