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
    )
}

#[test]
fn alias_and_raw_id_entries_both_grant() {
    let s = snap(
        &format!("[laptop]\nendpoint_id = \"{}\"\n", id(1)),
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
    let (h, e) = (d.path().join("hosts.toml"), d.path().join("exports.toml"));
    let a = FileAuthorizer::new(h.clone(), e.clone());
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
    std::fs::write(&h, format!("[lap]\nendpoint_id = \"{}\"\n", id(1))).unwrap();
    assert!(a.snapshot().unwrap().grants(&id(1)).allows("x", true));
}
