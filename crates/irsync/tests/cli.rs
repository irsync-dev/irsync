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
        "--config",
        "cfg",
        "--hosts",
        "hosts",
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
    assert_eq!(o.hosts, Some(PathBuf::from("hosts")));
    assert!(o.no_dht && o.numeric_ids);
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
    let a = parse(&[
        "daemon",
        "--config",
        "/c",
        "--hosts",
        "/h",
        "--exports",
        "/e",
    ]);
    let Some(Command::Daemon(d)) = a.cmd else {
        panic!()
    };
    assert_eq!(
        (d.config, d.hosts, d.exports),
        (Some("/c".into()), Some("/h".into()), Some("/e".into()))
    );
    assert!(Args::try_parse_from(["irsync", "daemon", "--foreground"]).is_err());
    // `id --daemon` is the one way to print the daemon's id.
    assert!(Args::try_parse_from(["irsync", "daemon", "--show-id"]).is_err());
}

#[test]
fn export_update_takes_the_noun_file_flags_in_either_position() {
    use irsync::cli::ExportCmd;
    for args in [
        &[
            "export",
            "update",
            "--exports",
            "/e",
            "--config",
            "/c",
            "photos",
        ][..],
        &[
            "export",
            "--exports",
            "/e",
            "--config",
            "/c",
            "update",
            "photos",
        ][..],
    ] {
        let Some(Command::Export(e)) = parse(args).cmd else {
            panic!("{args:?}")
        };
        assert_eq!(e.files.exports, Some(PathBuf::from("/e")));
        assert_eq!(e.files.config, Some(PathBuf::from("/c")));
        assert!(
            matches!(e.cmd, ExportCmd::Update { ref names, dry_run: false, .. } if names == &["photos"])
        );
    }
    // The old spelling is gone: `index` has no --daemon and no file flags.
    assert!(Args::try_parse_from(["irsync", "index", "--daemon", "photos"]).is_err());
    assert!(Args::try_parse_from(["irsync", "index", "--exports", "/e", "photos"]).is_err());
    assert!(Args::try_parse_from(["irsync", "index", "--config", "/c", "/srv/a"]).is_err());
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
            export,
            path,
            contents_only,
        } => {
            assert_eq!(host, "home");
            assert_eq!(export, "data");
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
fn location_missing_export_explains_the_address_form() {
    // rsync-over-ssh habit: an absolute path after the host.
    let err = Location::parse("eowyn:/tmp/xxx").unwrap_err().to_string();
    assert_eq!(
        err,
        "missing export in \"eowyn:/tmp/xxx\": remote addresses are HOST:EXPORT/PATH, \
         where EXPORT is a directory the daemon shares by name (irsync export add NAME:PATH)"
    );
    let err = Location::parse("home:").unwrap_err().to_string();
    assert!(err.starts_with("missing export in \"home:\": remote addresses are"));
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
        Location::Remote { host, export, .. } => {
            assert_eq!(host, id);
            assert_eq!(export, "mod");
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
fn daemon_hosts_and_exports_flags_parse() {
    match parse(&["daemon", "--hosts", "h.toml", "--exports", "e.toml"]).cmd {
        Some(Command::Daemon(d)) => {
            assert_eq!(d.hosts, Some(PathBuf::from("h.toml")));
            assert_eq!(d.exports, Some(PathBuf::from("e.toml")));
        }
        c => panic!("{c:?}"),
    }
    match parse(&["daemon"]).cmd {
        Some(Command::Daemon(d)) => assert!(d.hosts.is_none() && d.exports.is_none()),
        c => panic!("{c:?}"),
    }
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
        Some(Command::Export(e)) => match e.cmd {
            ExportCmd::Update {
                names,
                dry_run,
                log,
            } => {
                assert_eq!(names, ["photos", "music"]);
                assert!(dry_run && log.verbose == 1 && !log.quiet);
            }
            c => panic!("{c:?}"),
        },
        other => panic!("{other:?}"),
    }
    // `all` is the word for every export; a name is required.
    let a = parse(&["export", "update", "all"]);
    assert!(matches!(
        a.cmd,
        Some(Command::Export(ref e)) if matches!(e.cmd, ExportCmd::Update { ref names, dry_run: false, .. } if names == &["all"])
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
            if matches!(e.cmd, ExportCmd::Add(ref a) if a.target == "photos:/srv/photos" && a.ro && a.warm_index)
    ));
    assert!(matches!(
        parse(&["export", "remove", "photos"]).cmd,
        Some(Command::Export(ref e)) if matches!(e.cmd, ExportCmd::Remove { ref name } if name == "photos")
    ));
    assert!(matches!(
        parse(&["grant", "add", "photos", "laptop", "--ro"]).cmd,
        Some(Command::Grant(ref g))
            if matches!(g.cmd, GrantCmd::Add { ref export, ref who, ro: true } if export == "photos" && who == "laptop")
    ));
    assert!(matches!(
        parse(&["grant", "remove", "all", "all"]).cmd,
        Some(Command::Grant(ref g)) if matches!(g.cmd, GrantCmd::Remove { .. })
    ));
    assert!(matches!(
        parse(&["host", "remove", "home"]).cmd,
        Some(Command::Host(ref h)) if matches!(h.cmd, HostCmd::Remove { ref alias } if alias == "home")
    ));
    assert!(matches!(
        parse(&["host", "list"]).cmd,
        Some(Command::Host(ref h)) if matches!(h.cmd, HostCmd::List)
    ));
    assert!(matches!(
        parse(&["export", "list"]).cmd,
        Some(Command::Export(ref e)) if matches!(e.cmd, ExportCmd::List)
    ));
    assert!(matches!(
        parse(&["grant", "list"]).cmd,
        Some(Command::Grant(ref g)) if matches!(g.cmd, GrantCmd::List { target: None })
    ));
    assert!(matches!(
        parse(&["grant", "list", "photos"]).cmd,
        Some(Command::Grant(ref g)) if matches!(g.cmd, GrantCmd::List { target: Some(ref t) } if t == "photos")
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
    let HostCmd::Add(h) = h.cmd else { panic!() };
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
    let after_noun = parse(&["host", "--hosts", "/h", "--config", "/c", "list"]);
    let after_verb = parse(&["host", "list", "--hosts", "/h", "--config", "/c"]);
    for a in [after_noun, after_verb] {
        let Some(Command::Host(h)) = a.cmd else {
            panic!()
        };
        assert_eq!(
            (h.files.hosts, h.files.config, h.files.exports),
            (Some("/h".into()), Some("/c".into()), None)
        );
    }
    let a = parse(&["grant", "add", "photos", "laptop", "--exports", "/e"]);
    let Some(Command::Grant(g)) = a.cmd else {
        panic!()
    };
    assert_eq!(g.files.exports, Some("/e".into()));
    // Before the noun the flag belongs to a sync, and after a sync's flag
    // `host list` is its two paths, not a command.
    let a = parse(&["--hosts", "/h", "host", "list"]);
    assert!(
        a.cmd.is_none() && a.sync.paths == vec!["host", "list"],
        "{a:?}"
    );
}

#[test]
fn id_and_show_parse() {
    use irsync::cli::IdCmd;
    assert!(matches!(
        parse(&["id", "show"]).cmd,
        Some(Command::Id(ref i)) if matches!(i.cmd, IdCmd::Show { daemon: false, all: false, .. })
    ));
    assert!(matches!(
        parse(&["id", "show", "--daemon"]).cmd,
        Some(Command::Id(ref i)) if matches!(i.cmd, IdCmd::Show { daemon: true, all: false, .. })
    ));
    assert!(matches!(
        parse(&["id", "show", "-a"]).cmd,
        Some(Command::Id(ref i)) if matches!(i.cmd, IdCmd::Show { daemon: false, all: true, .. })
    ));
    // The file flags belong to the noun, so they go on either side of the verb.
    for args in [
        &["id", "show", "--config", "/c"][..],
        &["id", "--config", "/c", "show"][..],
    ] {
        let Some(Command::Id(i)) = parse(args).cmd else {
            panic!("{args:?}")
        };
        assert_eq!(i.files.config, Some(PathBuf::from("/c")));
    }
    assert!(Args::try_parse_from(["irsync", "id", "show", "--daemon", "--all"]).is_err());
    assert!(matches!(
        parse(&["id", "generate"]).cmd,
        Some(Command::Id(ref i)) if matches!(i.cmd, IdCmd::Generate { daemon: false, out: None, force: false })
    ));
    assert!(matches!(
        parse(&["id", "generate", "--daemon"]).cmd,
        Some(Command::Id(ref i)) if matches!(i.cmd, IdCmd::Generate { daemon: true, out: None, force: false })
    ));
    for args in [
        &["id", "generate", "--force"][..],
        &["id", "generate", "-f", "--daemon"][..],
    ] {
        assert!(
            matches!(
                parse(args).cmd,
                Some(Command::Id(ref i)) if matches!(i.cmd, IdCmd::Generate { force: true, .. })
            ),
            "{args:?}"
        );
    }
    assert!(matches!(
        parse(&["id", "generate", "--out", "/k"]).cmd,
        Some(Command::Id(ref i))
            if matches!(i.cmd, IdCmd::Generate { daemon: false, out: Some(ref p), .. } if p == &PathBuf::from("/k"))
    ));
    // `--out` names the file; `--daemon` picks the configured one.
    assert!(Args::try_parse_from(["irsync", "id", "generate", "--daemon", "--out", "/k"]).is_err());
    // `id` is a noun: it needs a verb.
    assert!(Args::try_parse_from(["irsync", "id"]).is_err());
    assert!(Args::try_parse_from(["irsync", "id", "--daemon"]).is_err());
    assert!(matches!(
        parse(&["show", "--config", "/c"]).cmd,
        Some(Command::Show {
            passphrase_file: None,
            ..
        })
    ));
    // An encrypted identity can be unlocked from a file, as for a sync.
    assert!(matches!(
        parse(&["id", "show", "--daemon", "--passphrase-file", "/p"]).cmd,
        Some(Command::Id(ref i))
            if matches!(i.cmd, IdCmd::Show { daemon: true, passphrase_file: Some(ref p), .. } if p == &PathBuf::from("/p"))
    ));
    assert!(matches!(
        parse(&["show", "--passphrase-file", "/p"]).cmd,
        Some(Command::Show { passphrase_file: Some(ref p), .. }) if p == &PathBuf::from("/p")
    ));
}

#[test]
fn subcommands_take_verbose_and_quiet() {
    match parse(&["daemon", "-vv"]).cmd {
        Some(Command::Daemon(d)) => assert_eq!((d.log.verbose, d.log.quiet), (2, false)),
        c => panic!("{c:?}"),
    }
    match parse(&["gc", "-n", "-q"]).cmd {
        Some(Command::Gc { dry_run, log }) => assert!(dry_run && log.quiet && log.verbose == 0),
        c => panic!("{c:?}"),
    }
    match parse(&["index", "-v", "/srv/a"]).cmd {
        Some(Command::Index(ix)) => assert_eq!(ix.log.verbose, 1),
        c => panic!("{c:?}"),
    }
    // One or the other.
    assert!(Args::try_parse_from(["irsync", "daemon", "-v", "-q"]).is_err());
    // Top-level flags still belong to a sync, not to a subcommand.
    assert!(Args::try_parse_from(["irsync", "-v", "gc"]).is_err());
}
