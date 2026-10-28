use irsync::authz::{Access, Grants};
use irsync::config::exports::Export;
use irsync::resolve::{containing, resolve, resolve_path, ResolveError};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn export(p: &str, ro: bool) -> Export {
    Export {
        path: PathBuf::from(p),
        read_only: ro,
        warm_index: false,
        rw: vec![],
        ro: vec![],
    }
}

fn exports(v: &[(&str, &str)]) -> BTreeMap<String, Export> {
    v.iter()
        .map(|(n, p)| (n.to_string(), export(p, false)))
        .collect()
}

fn all(a: Access) -> Grants {
    Grants {
        all: Some(a),
        ..Grants::default()
    }
}

fn only(n: &str, a: Access) -> Grants {
    Grants {
        exports: [(n.to_string(), a)].into(),
        all: None,
    }
}

#[test]
fn order_is_short_name_then_home_then_absolute() {
    let ex = exports(&[("photos", "/srv/photos")]);
    let home = Some(Path::new("/home/d"));
    let r = |p: &[u8]| resolve_path(p, home, &ex).unwrap();
    assert_eq!(r(b"photos/2024"), Some("/srv/photos/2024".into()));
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
        Some("/a/b/c".into())
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
        Some("/srv/photos/x".into())
    );
    assert_eq!(
        resolve_path(b"/srv/photos", None, &ex).unwrap(),
        Some("/srv/photos".into())
    );
}

#[test]
fn longest_export_wins_and_remainder_is_relative_to_it() {
    let ex = exports(&[("srv", "/srv"), ("photos", "/srv/photos")]);
    let rw = all(Access::ReadWrite);
    let r = containing(Path::new("/srv/photos/2024/a.jpg"), &ex, &rw).unwrap();
    assert_eq!((r.name, r.rel.as_bytes()), ("photos", &b"2024/a.jpg"[..]));
    let r = containing(Path::new("/srv/music"), &ex, &rw).unwrap();
    assert_eq!((r.name, r.rel.as_bytes()), ("srv", &b"music"[..]));
    let r = containing(Path::new("/srv/photos"), &ex, &all(Access::ReadOnly)).unwrap();
    assert_eq!(
        (r.name, r.rel.as_bytes(), r.access),
        ("photos", &b""[..], Access::ReadOnly)
    );
    assert!(containing(Path::new("/etc"), &ex, &rw).is_none());
    assert!(containing(Path::new("/srvx"), &ex, &rw).is_none());
}

#[test]
fn only_exports_the_peer_may_use_count() {
    let ex = exports(&[("srv", "/srv"), ("photos", "/srv/photos")]);
    let g = only("srv", Access::ReadWrite);
    // Inside photos (ungranted): none, not srv.
    assert!(containing(Path::new("/srv/photos/x"), &ex, &g).is_none());
    assert_eq!(
        containing(Path::new("/srv/x"), &ex, &g).unwrap().name,
        "srv"
    );
}

#[test]
fn equal_paths_best_access_wins() {
    let ex = exports(&[("a", "/data"), ("b", "/data")]);
    let g = Grants {
        exports: [
            ("a".to_string(), Access::ReadOnly),
            ("b".to_string(), Access::ReadWrite),
        ]
        .into(),
        all: None,
    };
    assert_eq!(
        containing(Path::new("/data/x"), &ex, &g).unwrap().access,
        Access::ReadWrite
    );
}

#[test]
fn export_path_is_compared_component_wise() {
    let ex = exports(&[("photos", "/srv/photos/"), ("docs", "/srv/./docs")]);
    let rw = all(Access::ReadWrite);
    assert_eq!(
        containing(Path::new("/srv/photos/a"), &ex, &rw)
            .unwrap()
            .name,
        "photos"
    );
    assert_eq!(
        containing(Path::new("/srv/docs/a"), &ex, &rw).unwrap().name,
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
