use irsync::authz::{Access, Authorizer, FileAuthorizer, Snapshot};
use irsync::config::{exports::Exports, hosts::Hosts};
use std::path::Path;

fn id(n: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}
fn snap(hosts: &str, exports: &str) -> Snapshot {
    Snapshot::from_files(
        &Hosts::parse(hosts, Path::new("h")).unwrap(),
        &Exports::parse(exports, Path::new("e")).unwrap(),
        true,
    )
}

#[test]
fn alias_and_raw_id_entries_both_grant() {
    let s = snap(
        &format!("[hosts.laptop]\nendpoint_id = \"{}\"\n", id(1)),
        &format!(
            "[exports.photos]\npath = \"/p\"\nrw = [\"laptop\"]\nro = [\"{}\"]\n",
            id(2)
        ),
    );
    assert!(s.grants(&id(1)).allows("photos", true));
    assert!(s.grants(&id(2)).allows("photos", false) && !s.grants(&id(2)).allows("photos", true));
    assert!(s.grants(&id(3)).is_empty());
}

#[test]
fn per_export_entry_wins_over_top_level_and_rw_wins_over_ro() {
    let s = snap("", &format!("[all]\nrw = [\"{a}\"]\nro = [\"{b}\"]\n[exports.x]\npath = \"/x\"\nro = [\"{a}\"]\nrw = [\"{b}\"]\n[exports.y]\npath = \"/y\"\nrw = [\"{a}\"]\nro = [\"{a}\"]\n", a = id(1), b = id(2)));
    assert!(!s.grants(&id(1)).allows("x", true) && s.grants(&id(1)).allows("other", true));
    assert!(s.grants(&id(2)).allows("x", true) && !s.grants(&id(2)).allows("other", true));
    assert!(s.grants(&id(1)).allows("y", true));
    assert_eq!(s.grants(&id(1)).exports.get("x"), Some(&Access::ReadOnly));
}

#[test]
fn unresolvable_entry_matches_nobody() {
    let s = snap(
        "",
        "[exports.x]\npath = \"/x\"\nrw = [\"ghost\", \"not-an-id\"]\n",
    );
    assert_eq!(s.exports.len(), 1);
    assert!(s.grants(&id(1)).is_empty());
}

#[test]
fn file_authorizer_reads_fresh_each_call_and_fails_closed() {
    let d = tempfile::tempdir().unwrap();
    let (h, e) = (d.path().join("hosts.toml"), d.path().join("irsync.toml"));
    let a = FileAuthorizer::new(irsync::config::paths::Paths::resolve(Some(&e)));
    assert!(a.snapshot().unwrap().exports.is_empty()); // missing = empty
    std::fs::write(
        &e,
        format!("[exports.x]\npath = \"/x\"\nrw = [\"{}\"]\n", id(1)),
    )
    .unwrap();
    assert!(a.snapshot().unwrap().grants(&id(1)).allows("x", true));
    std::fs::write(&e, "[x\n").unwrap();
    assert!(a.snapshot().is_err()); // broken = error, not stale
    std::fs::write(&e, "[exports.x]\npath = \"/x\"\nrw = [\"lap\"]\n").unwrap();
    std::fs::write(&h, format!("[hosts.lap]\nendpoint_id = \"{}\"\n", id(1))).unwrap();
    assert!(a.snapshot().unwrap().grants(&id(1)).allows("x", true));
}

#[test]
fn immediate_grant_reaches() {
    let s = Snapshot::from_files(
        &Hosts::parse("", Path::new("h")).unwrap(),
        &Exports::parse(
            &format!("[immediate]\nrw = [\"{}\"]\n", id(1)),
            Path::new("e"),
        )
        .unwrap(),
        true,
    );
    assert!(s.grants(&id(1)).immediate);
    assert!(!s.grants(&id(1)).is_empty());
}

#[test]
fn immediate_off_ignores_grant() {
    let s = Snapshot::from_files(
        &Hosts::parse("", Path::new("h")).unwrap(),
        &Exports::parse(
            &format!("[immediate]\nrw = [\"{}\"]\n", id(1)),
            Path::new("e"),
        )
        .unwrap(),
        false,
    );
    assert!(!s.grants(&id(1)).immediate);
    assert!(s.grants(&id(1)).is_empty());
}

/// An automatic export carries its peer's own `rw` entry; it counts only
/// while that peer's immediate grant is in effect, so removing the grant
/// or turning `immediate` off also ends access through earlier automatic
/// exports. Exports written by hand are unaffected.
#[test]
fn auto_export_grant_needs_the_immediate_grant() {
    let exports = |immediate: bool| {
        let mut t = format!(
            "[exports.home-auto]\npath = \"/h\"\nauto = true\nrw = [\"{a}\"]\n\
             [exports.x]\npath = \"/x\"\nrw = [\"{a}\"]\n",
            a = id(1)
        );
        if immediate {
            t = format!("[immediate]\nrw = [\"{}\"]\n{t}", id(1));
        }
        Exports::parse(&t, Path::new("e")).unwrap()
    };
    let hosts = Hosts::parse("", Path::new("h")).unwrap();
    let on = Snapshot::from_files(&hosts, &exports(true), true);
    assert!(on.grants(&id(1)).allows("home-auto", true));
    for (s, why) in [
        (
            Snapshot::from_files(&hosts, &exports(false), true),
            "no grant",
        ),
        (
            Snapshot::from_files(&hosts, &exports(true), false),
            "immediate off",
        ),
    ] {
        let g = s.grants(&id(1));
        assert_eq!(g.exports.get("home-auto"), None, "{why}");
        assert!(g.allows("x", true), "{why}");
        assert!(!g.immediate, "{why}");
    }
}
