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
fn timeout_zero_is_none_and_parallel_zero_is_rejected() {
    assert_eq!(opts(&["--timeout=0", "a", "h:m"]).timeout, None);
    assert_eq!(
        opts(&["--timeout=5", "a", "h:m"]).timeout,
        Some(std::time::Duration::from_secs(5))
    );
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
        "--timeout=9",
        "--config",
        "cfg",
        "--no-dht",
        "--max-delete=5",
        "--partial-dir=.p",
        "a",
        "h:m",
    ]);
    assert_eq!(o.verbose, 2);
    assert_eq!(o.reconnect, Some(Duration::from_secs(30)));
    assert_eq!(o.dial_retries, Some(2));
    assert_eq!(o.parallel, Some(4));
    assert_eq!(o.timeout, Some(Duration::from_secs(9)));
    assert_eq!(o.config, Some(PathBuf::from("cfg")));
    assert!(
        o.no_dht && !o.numeric_ids,
        "names are mapped unless asked not to"
    );
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
    assert!(a.cmd.is_none());
    assert_eq!(a.sync.src(), vec!["a", "b"]);
    assert_eq!(a.sync.dest(), Some("h:m/"));
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
fn export_update_takes_the_noun_file_flags_in_either_position() {
    use irsync::cli::ExportCmd;
    for args in [
        &["export", "update", "--config", "/c", "photos"][..],
        &["export", "--config", "/c", "update", "photos"][..],
    ] {
        let a = parse(args);
        let Some(Command::Export(e)) = a.cmd else {
            panic!("{args:?}")
        };
        assert_eq!(a.files.config, Some(PathBuf::from("/c")));
        assert!(
            matches!(e.cmd, Some(ExportCmd::Update { ref names, dry_run: false, .. }) if names == &["photos"])
        );
    }
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
        &["host", "list", "--hosts", "h"],
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
fn no_relay_flag_parses_and_conflicts_with_relay() {
    assert!(opts(&["--no-relay", "a", "h:m"]).no_relay);
    assert!(!opts(&["a", "h:m"]).no_relay);
    assert!(
        Args::try_parse_from(["irsync", "--no-relay", "--relay", "https://r", "a", "h:m"]).is_err()
    );
    // host add takes the mode by name.
    use irsync::config::settings::RelayMode;
    let a = Args::try_parse_from([
        "irsync",
        "host",
        "add",
        "home",
        "k51",
        "--relay-mode",
        "disabled",
        "--dial-retries",
        "1",
    ])
    .unwrap();
    match a.cmd {
        Some(Command::Host(h)) => match h.cmd.unwrap() {
            irsync::cli::HostCmd::Add(add) => {
                assert_eq!(add.relay_mode, Some(RelayMode::Disabled));
                assert_eq!(add.dial_retries, Some(1));
            }
            c => panic!("{c:?}"),
        },
        c => panic!("{c:?}"),
    }
    assert!(Args::try_parse_from([
        "irsync",
        "host",
        "add",
        "home",
        "k51",
        "--relay-mode",
        "maybe"
    ])
    .is_err());
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
fn export_update_parses_export_names_or_all() {
    use irsync::cli::ExportCmd;
    let a = parse(&["export", "update", "-n", "-v", "photos", "music"]);
    match a.cmd {
        Some(Command::Export(e)) => match e.cmd.unwrap() {
            ExportCmd::Update { names, dry_run } => {
                assert_eq!(names, ["photos", "music"]);
                assert!(dry_run && a.log.verbose == 1 && !a.log.quiet);
            }
            c => panic!("{c:?}"),
        },
        other => panic!("{other:?}"),
    }
    // `all` is the word for every export; a name is required.
    let a = parse(&["export", "update", "all"]);
    assert!(matches!(
        a.cmd,
        Some(Command::Export(ref e)) if matches!(e.cmd, Some(ExportCmd::Update { ref names, dry_run: false, .. }) if names == &["all"])
    ));
    assert!(Args::try_parse_from(["irsync", "export", "update"]).is_err());
    assert!(Args::try_parse_from(["irsync", "export", "update", "-n"]).is_err());
    assert!(Args::try_parse_from(["irsync", "export", "update", "-v", "-q"]).is_err());
}

#[test]
fn nouns_share_add_remove_list() {
    use irsync::cli::{ExportCmd, GrantCmd, HostCmd};
    assert!(matches!(
        parse(&["export", "add", "photos:/srv/photos", "--ro", "--warm-index"]).cmd,
        Some(Command::Export(ref e))
            if matches!(e.cmd, Some(ExportCmd::Add(ref a)) if a.target == "photos:/srv/photos" && a.ro && a.warm_index)
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
        parse(&["host", "remove", "home"]).cmd,
        Some(Command::Host(ref h)) if matches!(h.cmd, Some(HostCmd::Remove { ref alias, force: false }) if alias == "home")
    ));
    for flag in ["-f", "--force"] {
        assert!(matches!(
            parse(&["host", "remove", "home", flag]).cmd,
            Some(Command::Host(ref h)) if matches!(h.cmd, Some(HostCmd::Remove { force: true, .. }))
        ));
    }
    assert!(matches!(
        parse(&["host", "list"]).cmd,
        Some(Command::Host(ref h)) if matches!(h.cmd, Some(HostCmd::List))
    ));
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
    // host add keeps every flag
    let a = parse(&[
        "host",
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
    ]);
    let Some(Command::Host(h)) = a.cmd else {
        panic!()
    };
    let Some(HostCmd::Add(h)) = h.cmd else {
        panic!()
    };
    assert!(
        h.alias == "home"
            && h.id == "abc"
            && h.address.len() == 2
            && h.dht == Some(false)
            && h.mdns == Some(true)
    );
    assert!(
        Args::try_parse_from(["irsync", "host", "add", "home", "abc", "--dht", "maybe"]).is_err()
    );
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
        let a = parse(&[v, "photos"]);
        assert!(
            a.cmd.is_none() && a.sync.paths == vec![v, "photos"],
            "{v}: {a:?}"
        );
    }
}

#[test]
fn file_flags_are_accepted_after_the_noun_in_either_position() {
    let after_noun = parse(&["host", "--config", "/c", "list"]);
    let after_verb = parse(&["host", "list", "--config", "/c"]);
    for a in [after_noun, after_verb] {
        assert!(matches!(a.cmd, Some(Command::Host(_))));
        assert_eq!(a.files.config, Some("/c".into()));
    }
    let a = parse(&["grant", "add", "photos", "laptop", "--config", "/c"]);
    assert!(matches!(a.cmd, Some(Command::Grant(_))));
    assert_eq!(a.files.config, Some("/c".into()));
    // Before the noun it is the same flag.
    let a = parse(&["--config", "/c", "host", "list"]);
    assert!(matches!(a.cmd, Some(Command::Host(_))), "{a:?}");
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
    // `--out` names the file; `--daemon` picks the configured one.
    assert!(Args::try_parse_from(["irsync", "id", "generate", "--daemon", "--out", "/k"]).is_err());
    // A noun alone parses: it runs `list`.
    for noun in ["id", "host", "export", "grant", "config"] {
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
    let a = parse(&["home:"]);
    assert!(a.cmd.is_none() && a.sync.paths == ["home:"]);
    let a = parse(&["-v", "home:photos/"]);
    assert!(a.cmd.is_none() && a.sync.paths == ["home:photos/"]);
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
fn help_does_not_name_the_old_split_files() {
    use clap::CommandFactory;
    fn render(cmd: &mut clap::Command, out: &mut String) {
        out.push_str(&cmd.render_long_help().to_string());
        for sub in cmd.get_subcommands_mut() {
            render(sub, out);
        }
    }
    let mut help = String::new();
    render(&mut Args::command(), &mut help);
    for stale in ["exports.toml", "hosts.toml"] {
        assert!(!help.contains(stale), "{stale:?} still in help");
    }
}

#[test]
fn debug_addr_bare_port_is_loopback() {
    let o = opts(&["--debug-addr", "9464", "a", "b"]);
    assert_eq!(o.debug_addr, Some("127.0.0.1:9464".parse().unwrap()));
    assert!(!o.debug_no_wait);
    let o = opts(&[
        "--debug-addr",
        "0.0.0.0:1",
        "--debug-port-no-wait",
        "a",
        "b",
    ]);
    assert_eq!(o.debug_addr, Some("0.0.0.0:1".parse().unwrap()));
    assert!(o.debug_no_wait);
    assert!(Args::try_parse_from(["irsync", "--debug-addr", "x", "a", "b"]).is_err());
}

#[test]
fn no_wait_requires_debug_addr() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_irsync"))
        .args(["--debug-port-no-wait", "a", "b"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--debug-addr"), "{err}");
}

#[test]
fn progress_block_is_asked_for_with_progress_p_or_info_progress() {
    assert!(!opts(&["a/", "b/"]).progress);
    assert!(!opts(&["-v", "a/", "b/"]).progress);
    assert!(opts(&["--progress", "a/", "b/"]).progress);
    assert!(opts(&["-P", "a/", "b/"]).progress);
    assert!(opts(&["-avP", "a/", "b/"]).progress);
    assert!(opts(&["--info=progress2", "a/", "b/"]).progress);
    assert!(opts(&["--info=flist0,progress1", "a/", "b/"]).progress);
    assert!(!opts(&["--info=progress0", "a/", "b/"]).progress);
    assert!(!opts(&["--info=stats2", "a/", "b/"]).progress);
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
    assert!(parse(&["--quiet", "host", "list"]).log.quiet);
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
    assert!(a.cmd.is_none() && a.sync.paths == ["home:photos/"] && a.log.verbose == 1);
}

#[test]
fn a_path_named_like_a_command_is_still_a_path() {
    let a = parse(&["-a", "myserver:photos/", "config"]);
    assert!(a.cmd.is_none() && a.sync.paths == ["myserver:photos/", "config"]);
    let a = parse(&["src", "gc"]);
    assert!(a.cmd.is_none() && a.sync.paths == ["src", "gc"]);
    let a = parse(&["-v", "./gc", "h:m/"]);
    assert!(a.cmd.is_none() && a.sync.paths == ["./gc", "h:m/"]);
}

#[test]
fn config_is_accepted_before_and_after_any_command() {
    let f = Some(PathBuf::from("f"));
    for args in [
        &["--config", "f", "host", "list"][..],
        &["host", "--config", "f", "list"],
        &["host", "list", "--config", "f"],
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
    assert_eq!(parse(&["host", "list"]).files.config, None);
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
        &["--passphrase-file", "p", "host", "list"],
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
