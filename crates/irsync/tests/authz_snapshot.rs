use irsync::authz::{Access, Authorizer, FileAuthorizer, Snapshot};
use std::path::Path;

fn id(n: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}
fn snap(clients: &str, exports: &str) -> Snapshot {
    Snapshot::from_files(
        &parse_clients(clients, Path::new("h")).unwrap(),
        &parse_exports(exports, Path::new("e")).unwrap(),
        true,
    )
}

#[test]
fn name_and_raw_id_entries_both_grant() {
    let s = snap(
        &format!("[clients.laptop]\nclient_id = \"{}\"\n", id(1)),
        &format!(
            "[exports.photos]\npath = \"/p\"\nrw = [\"laptop\"]\nro = [\"{}\"]\n",
            id(2)
        ),
    );
    assert!(s.allows(&id(1), "photos", true));
    assert!(s.allows(&id(2), "photos", false) && !s.allows(&id(2), "photos", true));
    assert!(s.grants(&id(3)).is_empty());
}

/// A grant that names a machine is for its client id, not its daemon's.
#[test]
fn a_named_machine_is_granted_as_a_client() {
    let s = snap(
        &format!(
            "[servers.laptop]\nendpoint_id = \"{}\"\n[clients.laptop]\nclient_id = \"{}\"\n",
            id(1),
            id(2)
        ),
        "[exports.photos]\npath = \"/p\"\nrw = [\"laptop\"]\n",
    );
    assert!(s.allows(&id(2), "photos", true));
    assert!(s.grants(&id(1)).is_empty());
}

#[test]
fn per_export_entry_wins_over_all_exports_and_rw_wins_over_ro() {
    let clients = format!(
        "[clients.a]\nclient_id = \"{}\"\nall_exports = \"rw\"\n[clients.b]\nclient_id = \"{}\"\nall_exports = \"ro\"\n",
        id(1),
        id(2)
    );
    let s = snap(&clients, &format!("[exports.x]\npath = \"/x\"\nro = [\"{a}\"]\nrw = [\"{b}\"]\n[exports.y]\npath = \"/y\"\nrw = [\"{a}\"]\nro = [\"{a}\"]\n[exports.other]\npath = \"/o\"\n", a = id(1), b = id(2)));
    assert!(!s.allows(&id(1), "x", true) && s.allows(&id(1), "other", true));
    assert!(s.allows(&id(2), "x", true) && !s.allows(&id(2), "other", true));
    assert!(s.allows(&id(1), "y", true));
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
    let e = d.path().join("irsync.toml");
    let a = FileAuthorizer::new(irsync::config::paths::Paths::resolve(Some(&e)));
    assert!(a.snapshot().unwrap().exports.is_empty()); // missing = empty
    irsync::fs::private::write(
        &e,
        format!("[exports.x]\npath = \"/x\"\nrw = [\"{}\"]\n", id(1)),
    )
    .unwrap();
    assert!(a.snapshot().unwrap().allows(&id(1), "x", true));
    irsync::fs::private::write(&e, "[x\n").unwrap();
    assert!(a.snapshot().is_err()); // broken = error, not stale
    irsync::fs::private::write(
        &e,
        format!(
            "[exports.x]\npath = \"/x\"\nrw = [\"lap\"]\n[clients.lap]\nclient_id = \"{}\"\n",
            id(1)
        ),
    )
    .unwrap();
    assert!(a.snapshot().unwrap().allows(&id(1), "x", true));
}

#[test]
fn all_paths_reaches() {
    let s = snap(&laptop("all_paths = \"rw\"\n"), "");
    assert_eq!(s.grants(&id(1)).all_paths, Some(Access::ReadWrite));
    assert!(!s.grants(&id(1)).is_empty());
}

#[test]
fn all_paths_not_allowed_is_ignored() {
    let s = Snapshot::from_files(
        &parse_clients(&laptop("all_paths = \"rw\"\n"), Path::new("h")).unwrap(),
        &parse_exports("", Path::new("e")).unwrap(),
        false,
    );
    assert_eq!(s.grants(&id(1)).all_paths, None);
    assert!(s.grants(&id(1)).is_empty());
}

/// An automatic export carries its peer's own `rw` entry; it counts only
/// while that peer's `all_paths` is in effect, so removing it or turning
/// `allow_all_paths` off also ends access through earlier automatic
/// exports. Exports written by hand are unaffected.
#[test]
fn auto_export_grant_needs_all_paths() {
    let exports = parse_exports(
        &format!(
            "[exports.home-auto]\npath = \"/h\"\nauto = true\nrw = [\"{a}\"]\n\
             [exports.x]\npath = \"/x\"\nrw = [\"{a}\"]\n",
            a = id(1)
        ),
        Path::new("e"),
    )
    .unwrap();
    let clients = |rights: &str| parse_clients(&laptop(rights), Path::new("h")).unwrap();
    let with = clients("all_paths = \"rw\"\n");
    let on = Snapshot::from_files(&with, &exports, true);
    assert!(on.allows(&id(1), "home-auto", true));
    for (s, why) in [
        (
            Snapshot::from_files(&clients(""), &exports, true),
            "no all_paths",
        ),
        (
            Snapshot::from_files(&with, &exports, false),
            "allow_all_paths off",
        ),
    ] {
        let g = s.grants(&id(1));
        assert_eq!(g.exports.get("home-auto"), None, "{why}");
        assert!(s.allows(&id(1), "x", true), "{why}");
        assert_eq!(g.all_paths, None, "{why}");
    }
}

/// A client `laptop` with id 1 and the rights in `rights` (lines of
/// its table).
fn laptop(rights: &str) -> String {
    format!("[clients.laptop]\nclient_id = \"{}\"\n{rights}", id(1))
}

#[test]
fn all_exports_reaches_every_hand_made_export() {
    let s = snap(
        &laptop("all_exports = \"ro\"\n"),
        &format!(
            "[exports.docs]\npath = \"/d\"\n\
             [exports.auto-h]\npath = \"/h\"\nauto = true\nrw = [\"{}\"]\n",
            id(2)
        ),
    );
    let g = s.grants(&id(1));
    assert_eq!(g.all_exports, Some(Access::ReadOnly));
    assert_eq!(s.access(&id(1), "docs"), Some(Access::ReadOnly));
    assert_eq!(
        g.access_to("docs", &s.exports["docs"]),
        Some(Access::ReadOnly)
    );
    assert_eq!(g.access_to("auto-h", &s.exports["auto-h"]), None);
}

#[test]
fn an_exports_own_entry_wins_over_all_exports() {
    let s = snap(
        &laptop("all_exports = \"rw\"\n"),
        "[exports.docs]\npath = \"/d\"\nro = [\"laptop\"]\n",
    );
    assert_eq!(s.access(&id(1), "docs"), Some(Access::ReadOnly));
}

/// The level through an automatic export is the client's `all_paths` now,
/// not the list the export names it in: lowered at once, and raised again
/// on an export made while it was `ro`.
#[test]
fn an_automatic_export_gives_the_current_all_paths_level() {
    for (list, all_paths, want) in [
        ("rw", "ro", Access::ReadOnly),
        ("ro", "rw", Access::ReadWrite),
    ] {
        let s = snap(
            &laptop(&format!("all_paths = \"{all_paths}\"\n")),
            &format!("[exports.auto-h]\npath = \"/h\"\nauto = true\n{list} = [\"laptop\"]\n"),
        );
        let g = s.grants(&id(1));
        assert_eq!(
            g.access_to("auto-h", &s.exports["auto-h"]),
            Some(want),
            "listed under {list}, all_paths = {all_paths}"
        );
    }
}

#[test]
fn an_automatic_export_counts_only_with_all_paths_allowed() {
    let s = Snapshot::from_files(
        &parse_clients(&laptop("all_paths = \"rw\"\n"), Path::new("h")).unwrap(),
        &parse_exports(
            "[exports.auto-h]\npath = \"/h\"\nauto = true\nrw = [\"laptop\"]\n\
             [exports.x]\npath = \"/x\"\nrw = [\"laptop\"]\n",
            Path::new("e"),
        )
        .unwrap(),
        false,
    );
    let g = s.grants(&id(1));
    assert_eq!(g.all_paths, None);
    assert_eq!(g.access_to("auto-h", &s.exports["auto-h"]), None);
    assert_eq!(g.access_to("x", &s.exports["x"]), Some(Access::ReadWrite));
}

/// Two names for one id: `rw` beats `ro`, whichever comes first.
#[test]
fn two_names_for_one_id_take_the_higher_level() {
    for (a, b) in [("ro", "rw"), ("rw", "ro")] {
        let s = snap(
            &format!(
                "[clients.a]\nclient_id = \"{i}\"\nall_paths = \"{a}\"\nall_exports = \"{a}\"\n\
                 [clients.b]\nclient_id = \"{i}\"\nall_paths = \"{b}\"\nall_exports = \"{b}\"\n",
                i = id(1)
            ),
            "[exports.auto-h]\npath = \"/h\"\nauto = true\nro = [\"a\"]\n",
        );
        let g = s.grants(&id(1));
        assert_eq!(g.all_paths, Some(Access::ReadWrite), "a = {a}, b = {b}");
        assert_eq!(g.all_exports, Some(Access::ReadWrite), "a = {a}, b = {b}");
        assert_eq!(
            g.access_to("auto-h", &s.exports["auto-h"]),
            Some(Access::ReadWrite),
            "a = {a}, b = {b}"
        );
    }
}

fn parse_clients(
    text: &str,
    path: &std::path::Path,
) -> Result<irsync::config::clients::Clients, irsync::config::ConfigError> {
    irsync::config::file::Config::parse(text, path).map(|c| c.clients)
}

fn parse_exports(
    text: &str,
    path: &std::path::Path,
) -> Result<irsync::config::exports::Exports, irsync::config::ConfigError> {
    irsync::config::file::Config::parse(text, path).map(|c| c.exports)
}

#[test]
fn admin_alone_is_not_empty() {
    let s = snap(&laptop("admin = \"ro\"\n"), "[exports.x]\npath = \"/x\"\n");
    let g = s.grants(&id(1));
    assert!(!g.is_empty());
    assert_eq!(g.admin, Some(Access::ReadOnly));
    assert_eq!(s.admin(&id(1)), Some(Access::ReadOnly));
    assert_eq!(s.admin(&id(2)), None);
    assert!(g.exports.is_empty() && g.all_exports.is_none() && g.all_paths.is_none());
    assert!(!s.allows(&id(1), "x", false));
}
