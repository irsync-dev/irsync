use irsync::authz::{Access, Permissions};
use irsync::config::exports::Export;
use irsync::resolve::{containing, resolve, resolve_path, ResolveError};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn export(p: &str, ro: bool) -> Export {
    Export {
        path: PathBuf::from(p),
        read_only: ro,
        index_at_start: false,
        fsync: false,
        auto: false,
        rw: vec![],
        ro: vec![],
    }
}

fn exports(v: &[(&str, &str)]) -> BTreeMap<String, Export> {
    v.iter()
        .map(|(n, p)| (n.to_string(), export(p, false)))
        .collect()
}

fn all(a: Access) -> Permissions {
    Permissions {
        all_exports: Some(a),
        ..Permissions::default()
    }
}

fn only(n: &str, a: Access) -> Permissions {
    Permissions {
        exports: [(n.to_string(), a)].into(),
        ..Permissions::default()
    }
}

#[test]
fn order_is_short_name_then_home_then_absolute() {
    let ex = exports(&[("photos", "/srv/photos")]);
    let home = Some(Path::new("/home/d"));
    let r = |p: &[u8]| resolve_path(p, home, &ex).unwrap().map(|(p, _)| p);
    assert_eq!(r(b"photos/2024"), Some("/srv/photos/2024".into()));
    // The named export travels with the path.
    assert_eq!(
        resolve_path(b"photos/2024", home, &ex).unwrap().unwrap().1,
        Some("photos")
    );
    assert_eq!(
        resolve_path(b"Pictures", home, &ex).unwrap().unwrap().1,
        None
    );
    assert_eq!(r(b"Pictures"), Some("/home/d/Pictures".into()));
    assert_eq!(r(b""), Some("/home/d".into()));
    assert_eq!(r(b"~"), Some("/home/d".into()));
    assert_eq!(r(b"~/x"), Some("/home/d/x".into()));
    assert_eq!(r(b"/srv/photos/2024"), Some("/srv/photos/2024".into()));
    assert_eq!(r(b"/"), Some("/".into()));
    // A name that exists is never read as home-relative.
    assert_eq!(r(b"photos"), Some("/srv/photos".into()));
}

#[test]
fn lexical_rules() {
    let ex = exports(&[]);
    let home = Some(Path::new("/home/d"));
    assert_eq!(
        resolve_path(b"/a/./b//c/", home, &ex).unwrap(),
        Some(("/a/b/c".into(), None))
    );
    for bad in [&b"../x"[..], b"/a/../b", b"a/..", b"a\0b"] {
        assert!(
            matches!(resolve_path(bad, home, &ex), Err(ResolveError::BadPath)),
            "{bad:?}"
        );
    }
}

#[test]
fn relative_without_home_is_unresolvable() {
    let ex = exports(&[("photos", "/srv/photos")]);
    assert_eq!(resolve_path(b"Pictures", None, &ex).unwrap(), None);
    assert_eq!(resolve_path(b"", None, &ex).unwrap(), None);
    assert_eq!(
        resolve_path(b"photos/x", None, &ex).unwrap(),
        Some(("/srv/photos/x".into(), Some("photos")))
    );
    assert_eq!(
        resolve_path(b"/srv/photos", None, &ex).unwrap(),
        Some(("/srv/photos".into(), None))
    );
}

#[test]
fn longest_export_wins_and_remainder_is_relative_to_it() {
    let ex = exports(&[("srv", "/srv"), ("photos", "/srv/photos")]);
    let rw = all(Access::ReadWrite);
    let r = containing(Path::new("/srv/photos/2024/a.jpg"), &ex, &rw, None).unwrap();
    assert_eq!((r.name, r.rel.as_bytes()), ("photos", &b"2024/a.jpg"[..]));
    let r = containing(Path::new("/srv/music"), &ex, &rw, None).unwrap();
    assert_eq!((r.name, r.rel.as_bytes()), ("srv", &b"music"[..]));
    let r = containing(Path::new("/srv/photos"), &ex, &all(Access::ReadOnly), None).unwrap();
    assert_eq!(
        (r.name, r.rel.as_bytes(), r.access),
        ("photos", &b""[..], Access::ReadOnly)
    );
    assert!(containing(Path::new("/etc"), &ex, &rw, None).is_none());
    assert!(containing(Path::new("/srvx"), &ex, &rw, None).is_none());
}

#[test]
fn only_exports_the_peer_may_use_count() {
    let ex = exports(&[("srv", "/srv"), ("photos", "/srv/photos")]);
    let g = only("srv", Access::ReadWrite);
    // Inside photos (ungranted): srv's grant reaches everything below /srv,
    // so the reply is the same as for any other path under it; a peer
    // cannot tell where ungranted exports sit.
    let r = containing(Path::new("/srv/photos/x"), &ex, &g, None).unwrap();
    assert_eq!((r.name, r.rel.as_bytes()), ("srv", &b"photos/x"[..]));
    assert_eq!(
        containing(Path::new("/srv/x"), &ex, &g, None).unwrap().name,
        "srv"
    );
    // Granted on the inner one only: the outer is not reachable.
    let g = only("photos", Access::ReadWrite);
    assert!(containing(Path::new("/srv/x"), &ex, &g, None).is_none());
    assert_eq!(
        containing(Path::new("/srv/photos/x"), &ex, &g, None)
            .unwrap()
            .name,
        "photos"
    );
}

#[test]
fn equal_paths_best_access_wins() {
    let ex = exports(&[("a", "/data"), ("b", "/data")]);
    let g = Permissions {
        exports: [
            ("a".to_string(), Access::ReadOnly),
            ("b".to_string(), Access::ReadWrite),
        ]
        .into(),
        ..Permissions::default()
    };
    assert_eq!(
        containing(Path::new("/data/x"), &ex, &g, None)
            .unwrap()
            .access,
        Access::ReadWrite
    );
}

#[test]
fn export_path_is_compared_component_wise() {
    let ex = exports(&[("photos", "/srv/photos/"), ("docs", "/srv/./docs")]);
    let rw = all(Access::ReadWrite);
    assert_eq!(
        containing(Path::new("/srv/photos/a"), &ex, &rw, None)
            .unwrap()
            .name,
        "photos"
    );
    assert_eq!(
        containing(Path::new("/srv/docs/a"), &ex, &rw, None)
            .unwrap()
            .name,
        "docs"
    );
}

#[test]
fn resolve_combines_both() {
    let ex = exports(&[("photos", "/srv/photos")]);
    let g = all(Access::ReadWrite);
    let home = Some(Path::new("/h"));
    let r = resolve(b"photos/2024", home, &ex, &g).unwrap().unwrap();
    assert_eq!((r.name, r.rel.as_bytes()), ("photos", &b"2024"[..]));
    assert!(resolve(b"/etc", home, &ex, &g).unwrap().is_none());
    assert!(resolve(b"x", None, &ex, &g).unwrap().is_none());
    assert!(matches!(
        resolve(b"../x", home, &ex, &g),
        Err(ResolveError::BadPath)
    ));
}

#[test]
fn a_short_name_prefers_its_export_among_equal_paths_and_ties_rank_by_effective_access() {
    let mut ex = exports(&[("rw", "/data"), ("ro", "/data")]);
    ex.get_mut("ro").unwrap().read_only = true;
    let rw = all(Access::ReadWrite);
    // Named: the name wins even though `rw` has the better effective access.
    assert_eq!(
        containing(Path::new("/data"), &ex, &rw, Some("ro"))
            .unwrap()
            .name,
        "ro"
    );
    assert_eq!(resolve(b"ro", None, &ex, &rw).unwrap().unwrap().name, "ro");
    // Unnamed: the best effective access wins (read_only caps `ro`).
    assert_eq!(
        containing(Path::new("/data/x"), &ex, &rw, None)
            .unwrap()
            .name,
        "rw"
    );
    // Named but ungranted: the name is not an export's to this peer, also
    // when a granted twin shares the path. It is a directory under the
    // home, as if the export `rw` were not there.
    let g = only("ro", Access::ReadWrite);
    assert!(resolve(b"rw", None, &ex, &g).unwrap().is_none());
    assert!(resolve(b"rw", Some(Path::new("/h")), &ex, &g)
        .unwrap()
        .is_none());
    let mut with_home = ex.clone();
    with_home.insert("home".to_string(), export("/h", false));
    let g2 = Permissions {
        exports: [
            ("ro".to_string(), Access::ReadWrite),
            ("home".to_string(), Access::ReadWrite),
        ]
        .into(),
        ..Permissions::default()
    };
    let r = resolve(b"rw/x", Some(Path::new("/h")), &with_home, &g2)
        .unwrap()
        .unwrap();
    assert_eq!((r.name, r.rel.as_bytes()), ("home", &b"rw/x"[..]));
    // The directory itself stays in reach, by the granted name or its path.
    assert_eq!(resolve(b"ro", None, &ex, &g).unwrap().unwrap().name, "ro");
    assert_eq!(
        resolve(b"/data", None, &ex, &g).unwrap().unwrap().name,
        "ro"
    );
    // Nesting still wins over the name: `srv/photos` is the inner export.
    let ex = exports(&[("srv", "/srv"), ("photos", "/srv/photos")]);
    assert_eq!(
        resolve(b"srv/photos/x", None, &ex, &rw)
            .unwrap()
            .unwrap()
            .name,
        "photos"
    );
}

/// An export the peer has no grant for, in a directory it cannot reach,
/// does not take its name away: the name is a directory under the home,
/// exactly as if that export were not there.
#[test]
fn the_name_of_an_export_out_of_reach_is_a_directory_under_the_home() {
    let home = Some(Path::new("/home/d"));
    let with = exports(&[("home", "/home/d"), ("photos", "/srv/photos")]);
    let without = exports(&[("home", "/home/d")]);
    let g = only("home", Access::ReadWrite);
    for ex in [&with, &without] {
        let r = resolve(b"photos/2024", home, ex, &g).unwrap().unwrap();
        assert_eq!((r.name, r.rel.as_bytes()), ("home", &b"photos/2024"[..]));
    }
    // With a grant for it, the name is the export.
    let both = Permissions {
        exports: [
            ("home".to_string(), Access::ReadWrite),
            ("photos".to_string(), Access::ReadOnly),
        ]
        .into(),
        ..Permissions::default()
    };
    let r = resolve(b"photos/2024", home, &with, &both)
        .unwrap()
        .unwrap();
    assert_eq!((r.name, r.rel.as_bytes()), ("photos", &b"2024"[..]));
    // Without a home there is nothing else to read it as.
    assert!(resolve(b"photos/2024", None, &with, &g).unwrap().is_none());
}
