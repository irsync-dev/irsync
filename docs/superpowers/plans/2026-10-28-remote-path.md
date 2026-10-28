# Remote Paths Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `HOST:PATH` with one resolution order (export name, then home-relative, then absolute), exports as the access boundary, and `irsync HOST:` / `irsync HOST:PATH` listings.

**Architecture:** The client stops interpreting the remote path: `Location::Remote` carries the bytes as typed, and the rsync basename rule for a push travels as a separate `Hello.name`. A new pure module `resolve` turns (path, home, exports) into (export, remainder) and the daemon calls it before the existing grant and direction checks, so everything below the hello handshake is untouched. The export listing is a third `Hello.direction`; the directory listing is a dry-run pull into a temporary directory.

**Tech Stack:** Rust 2021, clap 4, prost/protox (protos in `proto/irsync/v1/`), iroh, tokio, `dirs`.

**Spec:** `docs/superpowers/specs/2026-10-28-remote-path-design.md`

## Global Constraints

- No compatibility with the current wire format or state files is owed ("No compatibility is owed to any released version"); `PROTOCOL_VERSION` becomes 2.
- Resolution order is fixed: short name, home-relative, absolute; "a name that exists is never reinterpreted as a home-relative path".
- The reply for a path the peer may not use is `UNAUTHORIZED`, "no access to PATH" with the path as typed, identical whether the path is outside every export, in an ungranted export, or missing.
- `..`, an empty component and NUL are rejected (`BAD_PATH`); `.` is dropped; no symlink resolution.
- Longest export path wins; equal paths: the peer's best access wins.
- `irsync HOST:` lists `NAME PATH ACCESS` sorted by name, only exports the peer may use; `theoden:/` is the root, never the home.
- The hint on both `UNAUTHORIZED` signals names `HOST:PATH`, the client id, and the three commands `irsync grant list <id>`, `irsync grant add EXPORT <id>`, `irsync export add NAME:PATH`.
- Before every commit: `cargo fmt --all` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean (the pre-push hook enforces fmt).

## Review Focus

- A push of a local directory whose basename equals an export name into the home (`irsync -av ~/photos theoden:`) must land in `~/photos` on the daemon, not in the export `photos`. Pinned in Task 2 (`push_basename_is_joined_after_resolution`).
- A daemon with no usable home directory (`$HOME` unset and no passwd entry) must still serve absolute and short-name paths and refuse home-relative ones with the uniform error. Pinned in Task 1 (`relative_without_home_is_unresolvable`).
- An export whose configured path has a trailing slash or a `.` component must still contain its own files. Pinned in Task 1 (`export_path_is_compared_component_wise`).
- A session resumed with a different spelling of the same place (`photos/2024` then `/srv/photos/2024`) must not corrupt either side: the daemon keys sessions by the resolved export and remainder, the client by what was typed. Pinned in Task 2 (`daemon_session_key_is_by_resolved_place`).
- `irsync theoden:photos` (single operand, no trailing slash) must list the directory and write nothing anywhere, including no state directory entry. Pinned in Task 5 (`directory_listing_leaves_no_state`).

---

### Task 1: The `resolve` module

**Files:**
- Create: `crates/irsync/src/resolve.rs`
- Modify: `crates/irsync/src/lib.rs` (add `pub mod resolve;`)
- Test: `crates/irsync/tests/resolve.rs`

**Interfaces:**
- Consumes: `crate::config::exports::Export` (`path: PathBuf`, `read_only`), `crate::authz::{Grants, Access}`, `crate::fs::paths::RelPath`.
- Produces:
  ```rust
  pub enum ResolveError { BadPath }           // `..`, empty component, NUL
  pub struct Resolved<'a> {
      pub name: &'a str,                      // the export's name
      pub export: &'a Export,
      pub rel: RelPath,                       // remainder below the export's path
      pub access: Access,                     // the peer's access, read_only not yet applied
  }
  /// `path` as typed; `home` = the daemon's home, `None` when unknown.
  pub fn resolve_path(path: &[u8], home: Option<&Path>, exports: &BTreeMap<String, Export>)
      -> Result<Option<PathBuf>, ResolveError>;   // Ok(None): relative path, no home
  /// Longest export path containing `abs`; equal paths: best access wins.
  pub fn containing<'a>(abs: &Path, exports: &'a BTreeMap<String, Export>, grants: &Grants)
      -> Option<Resolved<'a>>;                 // None: outside every export the peer may use
  /// The two above, for the daemon: Err(BadPath) or Ok(None) = no access.
  pub fn resolve<'a>(path: &[u8], home: Option<&Path>, exports: &'a BTreeMap<String, Export>, grants: &Grants)
      -> Result<Option<Resolved<'a>>, ResolveError>;
  ```

- [ ] **Step 1: Write the failing tests**

```rust
// crates/irsync/tests/resolve.rs
use irsync::authz::{Access, Grants};
use irsync::config::exports::Export;
use irsync::resolve::{containing, resolve, resolve_path, ResolveError};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn export(p: &str, ro: bool) -> Export { Export { path: PathBuf::from(p), read_only: ro, warm_index: false, rw: vec![], ro: vec![] } }
fn exports(v: &[(&str, &str)]) -> BTreeMap<String, Export> { v.iter().map(|(n, p)| (n.to_string(), export(p, false))).collect() }
fn all(a: Access) -> Grants { Grants { all: Some(a), ..Grants::default() } }
fn only(n: &str, a: Access) -> Grants { Grants { exports: [(n.to_string(), a)].into(), all: None } }

#[test]
fn order_is_short_name_then_home_then_absolute() {
    let ex = exports(&[("photos", "/srv/photos")]);
    let home = Some(Path::new("/home/d"));
    assert_eq!(resolve_path(b"photos/2024", home, &ex).unwrap(), Some("/srv/photos/2024".into()));
    assert_eq!(resolve_path(b"Pictures", home, &ex).unwrap(), Some("/home/d/Pictures".into()));
    assert_eq!(resolve_path(b"", home, &ex).unwrap(), Some("/home/d".into()));
    assert_eq!(resolve_path(b"~", home, &ex).unwrap(), Some("/home/d".into()));
    assert_eq!(resolve_path(b"~/x", home, &ex).unwrap(), Some("/home/d/x".into()));
    assert_eq!(resolve_path(b"/srv/photos/2024", home, &ex).unwrap(), Some("/srv/photos/2024".into()));
    assert_eq!(resolve_path(b"/", home, &ex).unwrap(), Some("/".into()));
    // A name that exists is never read as home-relative.
    assert_eq!(resolve_path(b"photos", home, &ex).unwrap(), Some("/srv/photos".into()));
}

#[test]
fn lexical_rules() {
    let ex = exports(&[]);
    let home = Some(Path::new("/home/d"));
    assert_eq!(resolve_path(b"/a/./b//c/", home, &ex).unwrap(), Some("/a/b/c".into()));
    for bad in [&b"../x"[..], b"/a/../b", b"a/..", b"a\0b"] {
        assert!(matches!(resolve_path(bad, home, &ex), Err(ResolveError::BadPath)), "{bad:?}");
    }
}

#[test]
fn relative_without_home_is_unresolvable() {
    let ex = exports(&[("photos", "/srv/photos")]);
    assert_eq!(resolve_path(b"Pictures", None, &ex).unwrap(), None);
    assert_eq!(resolve_path(b"", None, &ex).unwrap(), None);
    assert_eq!(resolve_path(b"photos/x", None, &ex).unwrap(), Some("/srv/photos/x".into()));
    assert_eq!(resolve_path(b"/srv/photos", None, &ex).unwrap(), Some("/srv/photos".into()));
}

#[test]
fn longest_export_wins_and_remainder_is_relative_to_it() {
    let ex = exports(&[("srv", "/srv"), ("photos", "/srv/photos")]);
    let r = containing(Path::new("/srv/photos/2024/a.jpg"), &ex, &all(Access::ReadWrite)).unwrap();
    assert_eq!((r.name, r.rel.as_bytes()), ("photos", &b"2024/a.jpg"[..]));
    let r = containing(Path::new("/srv/music"), &ex, &all(Access::ReadWrite)).unwrap();
    assert_eq!((r.name, r.rel.as_bytes()), ("srv", &b"music"[..]));
    let r = containing(Path::new("/srv/photos"), &ex, &all(Access::ReadOnly)).unwrap();
    assert_eq!((r.name, r.rel.as_bytes(), r.access), ("photos", &b""[..], Access::ReadOnly));
    assert!(containing(Path::new("/etc"), &ex, &all(Access::ReadWrite)).is_none());
    assert!(containing(Path::new("/srvx"), &ex, &all(Access::ReadWrite)).is_none());
}

#[test]
fn only_exports_the_peer_may_use_count() {
    let ex = exports(&[("srv", "/srv"), ("photos", "/srv/photos")]);
    // Granted on srv only: /srv/photos/x is inside photos (ungranted) -> none, not srv.
    assert!(containing(Path::new("/srv/photos/x"), &ex, &only("srv", Access::ReadWrite)).is_none());
    assert_eq!(containing(Path::new("/srv/x"), &ex, &only("srv", Access::ReadWrite)).unwrap().name, "srv");
}

#[test]
fn equal_paths_best_access_wins() {
    let ex = exports(&[("a", "/data"), ("b", "/data")]);
    let g = Grants { exports: [("a".to_string(), Access::ReadOnly), ("b".to_string(), Access::ReadWrite)].into(), all: None };
    assert_eq!(containing(Path::new("/data/x"), &ex, &g).unwrap().access, Access::ReadWrite);
}

#[test]
fn export_path_is_compared_component_wise() {
    let ex = exports(&[("photos", "/srv/photos/"), ("docs", "/srv/./docs")]);
    assert_eq!(containing(Path::new("/srv/photos/a"), &ex, &all(Access::ReadWrite)).unwrap().name, "photos");
    assert_eq!(containing(Path::new("/srv/docs/a"), &ex, &all(Access::ReadWrite)).unwrap().name, "docs");
}

#[test]
fn resolve_combines_both() {
    let ex = exports(&[("photos", "/srv/photos")]);
    let g = all(Access::ReadWrite);
    let r = resolve(b"photos/2024", Some(Path::new("/h")), &ex, &g).unwrap().unwrap();
    assert_eq!((r.name, r.rel.as_bytes()), ("photos", &b"2024"[..]));
    assert!(resolve(b"/etc", Some(Path::new("/h")), &ex, &g).unwrap().is_none());
    assert!(resolve(b"x", None, &ex, &g).unwrap().is_none());
    assert!(matches!(resolve(b"../x", Some(Path::new("/h")), &ex, &g), Err(ResolveError::BadPath)));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p irsync --test resolve`
Expected: compile error, `unresolved import irsync::resolve`.

- [ ] **Step 3: Implement `crates/irsync/src/resolve.rs` with the signatures above**

Resolution: split `path` on `/` into components (bytes); reject `..`, NUL; drop `.` and empty. Short name: first component, as UTF-8, is a key of `exports`. Home: `home.join(rest)`; `~` as the first component is the home. Absolute: `/` plus components. `containing`: for each export, split its path into normalized components the same way and test for a prefix of `abs`'s components; keep the longest, tie-break by `grants.access(name)` with `ReadWrite > ReadOnly`; exports with no access are skipped before the comparison. Build `rel` with `RelPath::new` on the joined remainder (`RelPath::root()` when equal).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p irsync --test resolve`
Expected: 8 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/resolve.rs crates/irsync/src/lib.rs crates/irsync/tests/resolve.rs
git commit -m "feat(resolve): HOST:PATH resolution: export name, home, absolute; longest export wins"
```

---

### Task 2: The wire and the daemon: `Hello.path` as typed, `Hello.name`, resolution before the grant check

**Files:**
- Modify: `proto/irsync/v1/common.proto` (`Hello`: remove `export`, reserve 3; `path` comment "PATH as typed"; add `bytes name = 21;`; `Error.Code`: remove `NO_SUCH_EXPORT`, reserve 2), `crates/irsync-proto/src/lib.rs` (`PROTOCOL_VERSION = 2`)
- Modify: `crates/irsync/src/cli/location.rs` (`Location::Remote { host: String, path: Vec<u8>, contents_only: bool }`; no "missing export" error; a lone `/` after the colon kept)
- Modify: `crates/irsync/src/session/mod.rs` (`resolve_roots`, `location_bytes`, `SessionKey::for_daemon`)
- Modify: `crates/irsync/src/session/engine.rs` (`remote_location`, `serve_daemon` takes the resolved `rel` and the export name; the `export = %hello.export` log field)
- Modify: `crates/irsync/src/daemon.rs` (`validate` resolves; home directory in `Shared`)
- Modify: `crates/irsync/src/main.rs` (hello construction)
- Test: `crates/irsync/tests/cli.rs`, `crates/irsync/tests/daemon_auth.rs`, `crates/irsync/src/session/mod.rs` (unit tests of `resolve_roots`), `crates/irsync/tests/sync_e2e.rs`

**Interfaces:**
- Consumes: Task 1's `resolve`, `Resolved`, `ResolveError`.
- Produces:
  - `Location::Remote { host: String, path: Vec<u8>, contents_only: bool }`.
  - `pub fn resolve_roots(direction, local: &Path, local_contents_only: bool, remote: &[u8], remote_contents_only: bool) -> (PathBuf, Vec<u8>)` where the second value is `Hello.name` (the local basename on a push without a trailing slash, else empty); the remote path itself is sent unchanged.
  - `Hello { path: PATH as typed, name: basename or empty, contents_only, .. }`.
  - daemon: `fn validate<'a>(snap: &'a Snapshot, grants: &Grants, home: Option<&Path>, hello: &Hello) -> Result<(Resolved<'a>, RelPath), (Code, String)>` where the `RelPath` is `resolved.rel` joined with `hello.name`.
  - `serve_daemon(conn, send, recv, export_root: PathBuf, export_name: String, rel: RelPath, hello: Hello, index, shared)`.
  - `SessionKey::for_daemon(client, export_name: &str, rel: &RelPath, hello: &Hello)` (fields: client, export name, rel bytes, direction, contents_only, the option fields as today).
  - `Shared.home: Option<PathBuf>` filled with `dirs::home_dir()` at daemon start.

- [ ] **Step 1: Write the failing parser tests** (replace `location_missing_export_explains_the_address_form` and adjust `location_remote_with_trailing_slash` in `crates/irsync/tests/cli.rs`)

```rust
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
            Location::Remote { host, path: p, contents_only } => {
                assert_eq!((host.as_str(), p.as_slice(), contents_only), ("home", path, co), "{s}");
            }
            l => panic!("{s}: {l:?}"),
        }
    }
    assert!(Location::parse(":x").is_err());
    // Validity (`..`, NUL) is the daemon's call now; the client passes it on.
    assert!(matches!(Location::parse("home:m/../x").unwrap(), Location::Remote { .. }));
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p irsync --test cli remote_path_is_passed_as_typed`
Expected: FAIL to compile (`export` field in the pattern elsewhere) or assertion on `home:`.

- [ ] **Step 3: Write the failing `resolve_roots` unit tests** (replace `trailing_slash_rule` in `crates/irsync/src/session/mod.rs`)

```rust
#[test]
fn trailing_slash_rule_puts_the_basename_in_name() {
    use Direction::*;
    let p = Path::new("/l/photos");
    assert_eq!(resolve_roots(ClientSends, p, false, b"", false), (p.into(), b"photos".to_vec()));
    assert_eq!(resolve_roots(ClientSends, p, true, b"", false), (p.into(), vec![]));
    assert_eq!(resolve_roots(ClientSends, p, false, b"/srv/x", false), (p.into(), b"photos".to_vec()));
    // Pull: the remote's last component names the local directory; `name` stays empty.
    assert_eq!(resolve_roots(DaemonSends, Path::new("/l"), false, b"/srv/photos", false), (PathBuf::from("/l/photos"), vec![]));
    assert_eq!(resolve_roots(DaemonSends, Path::new("/l"), false, b"/srv/photos", true), (PathBuf::from("/l"), vec![]));
    assert_eq!(resolve_roots(DaemonSends, Path::new("/l"), false, b"", false), (PathBuf::from("/l"), vec![]));
    assert_eq!(resolve_roots(DaemonSends, Path::new("/l"), false, b"/", false), (PathBuf::from("/l"), vec![]));
}

#[test]
fn daemon_session_key_is_by_resolved_place() {
    let c = iroh::SecretKey::from_bytes(&[1; 32]).public();
    let rel = RelPath::new(b"2024").unwrap();
    let h = |path: &[u8]| Hello { path: path.to_vec(), ..Hello::default() };
    assert_eq!(SessionKey::for_daemon(c, "photos", &rel, &h(b"photos/2024")), SessionKey::for_daemon(c, "photos", &rel, &h(b"/srv/photos/2024")));
    assert_ne!(SessionKey::for_daemon(c, "photos", &rel, &h(b"photos/2024")), SessionKey::for_daemon(c, "srv", &rel, &h(b"photos/2024")));
}
```

- [ ] **Step 4: Write the failing daemon tests** (`crates/irsync/tests/daemon_auth.rs`; the `hello` helper becomes `hello(path: &[u8], dir) -> Hello` and `spawn_daemon` in the helpers gets a home via a new `spawn_daemon_home(exports, authz, home: Option<PathBuf>)` in `tests/common/mod.rs`; the existing two exports `rw` and `ro` at the same temp dir stay)

```rust
// helpers: `hello(path: &[u8], dir: Direction) -> Hello`; `exchange_with(g, h)` spawns the daemon
// with exports rw = <dir> and ro = <dir> (read_only) and home = <dir>.parent(), via
// common::spawn_daemon_home(exports, authz, Some(home)).
fn no_access(path: &[u8]) -> String { format!("no access to {}", irsync::fs::paths::escape_path(path)) }

#[tokio::test]
async fn short_name_home_and_absolute_all_reach_the_export() {
    let (dir, home_rel, abs) = export_dir(); // <dir>, its file name as bytes, its absolute path as bytes
    for path in [&b"rw"[..], &b"rw/"[..], &home_rel, &abs] {
        let r = exchange_in(&dir, grants(Access::ReadWrite), hello(path, Direction::ClientSends)).await;
        assert!(matches!(r.unwrap(), HelloAckOrError::Ack(_)), "{path:?}");
    }
}

#[tokio::test]
async fn path_outside_every_export_is_unauthorized_with_the_path_as_typed() {
    let (dir, ..) = export_dir();
    for path in [&b"/etc"[..], b"nope", b"", b"~", b"/"] {
        let e = reply(exchange_in(&dir, grants(Access::ReadWrite), hello(path, Direction::DaemonSends)).await);
        assert_eq!(e, (Code::Unauthorized as i32, no_access(path)), "{path:?}");
    }
}

#[tokio::test]
async fn dotdot_is_bad_path_for_a_granted_key() {
    let (dir, ..) = export_dir();
    for path in [&b"rw/../x"[..], b"/a/../b", b"a\0b"] {
        let e = reply(exchange_in(&dir, grants(Access::ReadWrite), hello(path, Direction::DaemonSends)).await);
        assert_eq!(e.0, Code::BadPath as i32, "{path:?}: {}", e.1);
    }
}

#[tokio::test]
async fn ungranted_reply_does_not_reveal_whether_a_path_exists() {
    // The key has a grant on nothing that contains either path.
    let (dir, _, abs) = export_dir();
    let none = Grants::default_with(("other", Access::ReadWrite)); // grants on an export that is not served
    let mut existing = abs.clone(); existing.extend_from_slice(b"/sub");
    let a = reply(exchange_in(&dir, none.clone(), hello(&existing, Direction::DaemonSends)).await);
    let b = reply(exchange_in(&dir, none, hello(b"/no/such", Direction::DaemonSends)).await);
    assert_eq!(a, (Code::Unauthorized as i32, no_access(&existing)));
    assert_eq!(b, (Code::Unauthorized as i32, no_access(b"/no/such")));
}

#[tokio::test]
async fn push_basename_is_joined_after_resolution() {
    // exports: photos = <tmp>/srv/photos, home = <tmp>/home (both granted rw); daemon home = <tmp>/home.
    // Hello { path: b"", name: b"photos", direction: ClientSends, dry_run: false } -> HelloAck,
    // and the daemon created <tmp>/home/photos (prepare_root with Missing::Create), not <tmp>/srv/photos/photos.
    let tmp = tempfile::tempdir().unwrap();
    let (srv, home) = (tmp.path().join("srv/photos"), tmp.path().join("home"));
    std::fs::create_dir_all(&srv).unwrap(); std::fs::create_dir_all(&home).unwrap();
    let d = spawn_daemon_home(vec![("photos", srv.clone(), false), ("home", home.clone(), false)], MemoryAuthorizer::single(client.id(), grants(Access::ReadWrite)), Some(home.clone())).await;
    let mut h = hello(b"", Direction::ClientSends); h.name = b"photos".to_vec();
    assert!(matches!(exchange_against(&d, &client, h).await.unwrap(), HelloAckOrError::Ack(_)));
    assert!(home.join("photos").is_dir() && !srv.join("photos").exists());
}

#[tokio::test]
async fn read_only_export_and_ro_grant_still_refuse_sends() {
    let (dir, _, abs) = export_dir();
    // "ro" is read_only: a rw grant cannot send to it, by short name or by path.
    expect_err(exchange_in(&dir, grants(Access::ReadWrite), hello(b"ro", Direction::ClientSends)).await, Code::ReadOnly);
    // the same directory reached by absolute path resolves to the best access among rw/ro (rw) and is writable.
    assert!(matches!(exchange_in(&dir, grants(Access::ReadWrite), hello(&abs, Direction::ClientSends)).await.unwrap(), HelloAckOrError::Ack(_)));
    expect_err(exchange_in(&dir, grants(Access::ReadOnly), hello(&abs, Direction::ClientSends)).await, Code::ReadOnly);
}
```

`export_dir()`, `exchange_in(dir, grants, hello)` and `reply(r) -> (i32, String)` are small helpers in the test file; `Grants::default_with` is shorthand for building `Grants { exports: [..].into(), all: None }` inline.

Delete `unknown_export_rejected` and `export_path_escape_rejected` (covered above); keep the rest, retargeted to `hello(b"rw", ..)`.

- [ ] **Step 5: Run them to verify they fail**

Run: `cargo test -p irsync --test daemon_auth`
Expected: compile errors on `Hello.export` / `Code::NoSuchExport`.

- [ ] **Step 6: Change the proto and bump the version**

`proto/irsync/v1/common.proto`: in `Hello`, replace `string export = 3;` by `reserved 3;`, comment `path` as "PATH as typed: export name, home-relative or absolute; a trailing slash is stripped (contents_only)", add `bytes name = 21; // on CLIENT_SENDS without a trailing slash on the source: its basename, joined by the daemon after resolution`. In `Error.Code`, replace `NO_SUCH_EXPORT = 2;` by `reserved 2;`. `PROTOCOL_VERSION = 2`.

- [ ] **Step 7: Implement the client side**

`Location::parse`: after the colon, strip trailing slashes unless the remainder is exactly `/`; `path = rest.as_bytes().to_vec()`; the only error is an empty host. `resolve_roots` per the Produces block. `main.rs`: `Hello { path: rpath.clone(), name, contents_only: *remote_co, .. }`. `location_bytes`: `remote\0`, contents_only, path bytes. `engine.rs::remote_location`: `Location::Remote { host, path: hello.path.clone(), contents_only }`.

- [ ] **Step 8: Implement the daemon side**

`Shared.home = dirs::home_dir()`. `validate`: version and chunk checks as today, then `resolve(&hello.path, home, &snap.exports, grants)`: `Err(BadPath)` → `(Code::BadPath, "invalid path")`; `Ok(None)` → `(Code::Unauthorized, format!("no access to {}", escape_path(&hello.path)))` (the same `escape_path` the tests use); then `rel = resolved.rel.join(&hello.name)` when `name` is non-empty (`BadPath` if the join fails); then the direction check as today with `resolved.export.read_only || !resolved.access.permits(true)`. `handle`: root = `resolved.export.path`, index via `index_for(&shared, resolved.name, resolved.export)`, `serve_daemon(.., root, resolved.name.to_string(), rel, hello, ..)`. In `serve_daemon` use the passed `rel` instead of `RelPath::new(&hello.path)`, log `export = %export_name`, and `SessionKey::for_daemon(remote, &export_name, &rel, &hello)`.

- [ ] **Step 9: Run the whole suite**

Run: `cargo test --workspace`
Expected: all green; the end-to-end tests (`sync_e2e.rs`, `isolated_e2e.rs`, `live_exports_e2e.rs`, the stress crate) still pass since they spell `HOST:export/` which resolves by short name.

- [ ] **Step 10: Commit**

```bash
git add proto crates
git commit -m "feat(proto,daemon,cli): HOST:PATH as typed; the daemon resolves export, home or absolute paths; Hello.name carries the push basename"
```

---

### Task 3: The hint on both `UNAUTHORIZED` signals

**Files:**
- Modify: `crates/irsync/src/session/reconnect.rs` (`describe_close`, `classify`, `run_with_reconnect`), `crates/irsync/src/main.rs` (pass the typed `HOST:PATH`)
- Test: `crates/irsync/src/session/reconnect.rs` unit tests, `crates/irsync/tests/live_exports_e2e.rs`

**Interfaces:**
- Consumes: Task 2's `Location::Remote`.
- Produces: `pub fn run_with_reconnect(endpoint, target, remote: &str, hello_for, local, sync_opts, opts, on_state)` where `remote` is the operand as typed (`theoden:/srv/photos`); `fn no_access_hint(remote: &str, client: &EndpointId) -> String` whose text is:
  `the daemon rejected this client's key: no access to {remote}. On the daemon's machine, irsync grant list {client} shows what this key may reach; irsync grant add EXPORT {client} grants an export, irsync export add NAME:PATH exports a directory (irsync id show prints this id)`.

- [ ] **Step 1: Write the failing unit tests** (replace the `describe_close` tests)

```rust
#[test]
fn unauthorized_close_and_refusal_share_the_hint() {
    let id = iroh::SecretKey::from_bytes(&[7; 32]).public();
    let close = describe_close(1, "unauthorized", "theoden:/srv/x", &id);
    let refused = describe_refusal(&SessionRefused { code: Code::Unauthorized, message: "no access to \"/srv/x\"".into() }, "theoden:/srv/x", &id).unwrap();
    for msg in [&close, &refused] {
        assert!(msg.contains("no access to theoden:/srv/x"), "{msg}");
        assert!(msg.contains(&format!("irsync grant list {id}")), "{msg}");
        assert!(msg.contains(&format!("irsync grant add EXPORT {id}")), "{msg}");
        assert!(msg.contains("irsync export add NAME:PATH"), "{msg}");
    }
    assert!(describe_refusal(&SessionRefused { code: Code::ReadOnly, message: "x".into() }, "t:/p", &id).is_none());
    assert!(describe_close(2, "hello timeout", "t:/p", &id).contains("hello timeout"));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p irsync --lib reconnect`
Expected: FAIL, `describe_refusal` not found.

- [ ] **Step 3: Implement**

`describe_close(1, ..)` returns `no_access_hint(remote, client)`; `describe_refusal(r, remote, client) -> Option<String>` returns the hint for `Code::Unauthorized`. In `run_with_reconnect`, on `Next::Fatal` when the error chain holds a `SessionRefused` with `Unauthorized`, return `anyhow!(describe_refusal(..).unwrap())`. `main.rs` passes the typed operand (the `String` from `args.sync` for the remote side).

- [ ] **Step 4: Extend the live test** (`live_exports_e2e.rs`, step 4 and step 7): the destination is `format!("{daemon_id}:{}/", data.display())` (absolute, no export name), and both failures assert `irsync grant list {client_id}` and `irsync grant add EXPORT {client_id}` in stderr. Add a step between 6 and 7: with the grant in place, a push to `format!("{daemon_id}:/nowhere/")` fails with the same hint (the refusal path).

- [ ] **Step 5: Run the tests**

Run: `cargo test -p irsync --lib reconnect && cargo test -p irsync --test live_exports_e2e`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/irsync/src/session/reconnect.rs crates/irsync/src/main.rs crates/irsync/tests/live_exports_e2e.rs
git commit -m "feat(client): the no-access hint names HOST:PATH and both remedies, on the close and on the refusal"
```

---

### Task 4: `irsync HOST:` lists the reachable exports

**Files:**
- Modify: `proto/irsync/v1/common.proto` (`Hello.Direction` gets `LIST_EXPORTS = 2;`; new messages `ExportEntry { string name = 1; bytes path = 2; Access access = 3; enum Access { RO = 0; RW = 1; } }` and `ExportList { repeated ExportEntry exports = 1; }`)
- Modify: `crates/irsync/src/daemon.rs` (answer `LIST_EXPORTS` before `validate`), `crates/irsync/src/session/engine.rs` (`pub async fn list_exports(conn: &Connection, hello: Hello) -> anyhow::Result<Vec<ExportEntry>>`), `crates/irsync/src/cli/mod.rs` (`try_parse_from`: one path allowed when `Location::parse` says it is remote), `crates/irsync/src/main.rs` (`run_list`)
- Test: `crates/irsync/tests/cli.rs`, `crates/irsync/tests/daemon_auth.rs`, `crates/irsync/tests/listing_e2e.rs` (new; the daemon spawn of `live_exports_e2e.rs` moves to `tests/common/mod.rs` as `pub fn spawn_daemon_binary(server_home: &Path) -> (DaemonGuard, EndpointId, SocketAddr, PathBuf /*log*/)` and both tests use it)

**Interfaces:**
- Consumes: Task 2's hello shape; Task 3's `run_with_reconnect` is not used (a listing does not resume).
- Produces: `fn run_list(args: &Args, opts: &SyncOptions) -> i32` in `main.rs`, dispatching on the single operand: empty path → export listing (this task), else directory listing (Task 5). Output line: `format!("{name} {} {}", escape_path(&path), access)` with `access` `rw`/`ro`, sorted by name.

- [ ] **Step 1: Write the failing parse test** (`cli.rs`)

```rust
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
```

- [ ] **Step 2: Write the failing daemon test** (`daemon_auth.rs`)

```rust
#[tokio::test]
async fn list_exports_names_only_what_the_key_may_use() {
    // exports rw (= dir) and ro (= dir, read_only); grants: exports {rw: ReadWrite} only -> one entry "rw", path = dir, access RW.
    // grants all = ReadWrite -> two entries, "ro" shows RO (read_only caps), "rw" shows RW; sorted by name.
    // a key with Grants::default() is closed before Hello as today.
}
```

- [ ] **Step 3: Write the failing end-to-end test** (`listing_e2e.rs`)

```rust
#[test]
fn host_colon_lists_reachable_exports_with_real_paths() {
    // spawn_daemon_binary; server: export add photos:<srv/photos>, export add docs:<srv/docs> --ro, export add secret:<srv/secret>;
    // grant add photos <client>, grant add docs <client>.
    // client: irsync --address 127.0.0.1:PORT <daemon_id>:
    // stdout == format!("docs {} ro\nphotos {} rw\n", docs.display(), photos.display()); "secret" absent; exit 0.
    // a client with no grant: exit != 0, stderr contains "rejected this client's key".
}
```

- [ ] **Step 4: Run to verify they fail**

Run: `cargo test -p irsync --test cli one_remote_operand_is_a_listing`
Expected: FAIL, `<SRC>... <DEST>` usage error.

- [ ] **Step 5: Implement**

Proto as above. Daemon `handle`: after reading `Hello`, if `direction == LIST_EXPORTS`, reply `ExportList` built from `snap.exports` filtered by `grants.access(name)`, `access` = `RO` when `export.read_only || access == ReadOnly`, sorted by name, then `send.finish()` and return (no session, no index). Client `list_exports`: open the control stream, write `Hello { direction: LIST_EXPORTS, path: vec![], .. }`, read one message: `ExportList` → entries; `Error` → `SessionRefused`. `try_parse_from`: the `< 2` check becomes `paths.len() < 2 && !(paths.len() == 1 && Location::parse(&paths[0]).is_ok_and(|l| l.is_remote()))`. `main.rs`: in `main`, `None` with one path → `run_list`; `run_list` resolves the host, addresses, identity and endpoint exactly as `sync` does (extract that prefix of `sync` into `async fn connect_for(remote_host: &str, opts: &SyncOptions) -> anyhow::Result<(Endpoint, EndpointAddr)>` and use it from both), then prints the lines; a closed connection with code 1 prints the Task 3 hint.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p irsync --test cli --test daemon_auth --test listing_e2e --test live_exports_e2e`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add proto crates
git commit -m "feat(cli): irsync HOST: lists the exports this client may reach, with their real paths"
```

---

### Task 5: `irsync HOST:PATH` lists a directory

**Files:**
- Modify: `crates/irsync/src/main.rs` (`run_list` directory branch)
- Test: `crates/irsync/tests/listing_e2e.rs`

**Interfaces:**
- Consumes: Task 4's `run_list` and `connect_for`; the existing `sync` with `opts.dry_run = true`, `opts.verbose = max(opts.verbose, 1)`, `opts.preserve.recursive = true`, source = the operand, destination = a `tempfile::tempdir()` whose path is also the state directory for the run.
- Produces: nothing new.

- [ ] **Step 1: Write the failing end-to-end tests** (`listing_e2e.rs`)

```rust
#[test]
fn host_path_lists_the_directory_without_writing() {
    // daemon with export photos containing a/x.jpg and b.jpg, granted; also exported home for a short-name check is not needed.
    // irsync --address .. <id>:photos/  -> stdout lines contain "a/x.jpg" and "b.jpg"; exit 0.
    // irsync --address .. <id>:<photos abs path>  -> same lines.
}

#[test]
fn directory_listing_leaves_no_state() {
    // After `irsync --address .. <id>:photos` (no trailing slash, so the listing is of the directory itself)
    // the client's XDG_STATE_HOME has no irsync directory and the listing's temp dir is gone:
    // assert!(!client_home.join("xdg/irsync").exists()); assert the output names "photos/b.jpg".
}

#[test]
fn listing_an_unreachable_path_prints_the_hint() {
    // <id>:/nowhere -> exit != 0, stderr contains "irsync grant list <client_id>"
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p irsync --test listing_e2e host_path_lists_the_directory_without_writing`
Expected: FAIL (the operand is treated as the export listing or errors).

- [ ] **Step 3: Implement**

`run_list` non-empty path: create `tempfile::tempdir()`, set the state directory for this run to it (the `state_dir()` helper in `main.rs` gains an override, `set_state_dir_override(PathBuf)`, read before the XDG default), build `SyncOptions` as in Consumes, call the existing `sync` machinery with `srcs = [operand]`, `dest = <tempdir>/list`, and drop the tempdir afterwards.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p irsync --test listing_e2e`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/main.rs crates/irsync/tests/listing_e2e.rs
git commit -m "feat(cli): irsync HOST:PATH lists a directory as a dry-run pull into a throwaway root"
```

---

### Task 6: `export add` tilde, docs and spec notes

**Files:**
- Modify: `crates/irsync/src/manage/exports.rs` (`parse_add_target`: a path `~` or `~/...` is the home directory of the invoking user, via `dirs::home_dir()`; error "no home directory" when unknown)
- Modify: `README.md` (quick start, "Addresses are" paragraph, the `Three nouns` table's intro sentence stays, a new paragraph on listings), `docs/daemon.md` (line 45 hint text, line 298 example), `docs/superpowers/specs/2026-10-23-zero-config-cli-design.md` (a note under "Vocabulary: exports, not modules" pointing at the 2026-10-28 remote-path spec)
- Test: `crates/irsync/tests/manage_exports.rs`, `crates/irsync/tests/commands_e2e.rs`

- [ ] **Step 1: Write the failing test** (`manage_exports.rs`)

```rust
#[test]
fn add_target_expands_a_leading_tilde() {
    let home = dirs::home_dir().unwrap();
    let (name, path) = parse_add_target("home:~", Path::new("/cwd")).unwrap();
    assert_eq!((name.as_str(), path), ("home", home.clone()));
    // `~x` is a plain name, not a user.
    assert!(matches!(parse_add_target("x:~nobody", Path::new("/cwd")), Err(ManageError::NotADirectory(_))));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p irsync --test manage_exports add_target_expands_a_leading_tilde`
Expected: FAIL, `NotADirectory("/cwd/~")`.

- [ ] **Step 3: Implement the expansion, then the docs**

README "Addresses" paragraph becomes: `HOST:PATH`, where `PATH` is an export name followed by a path inside it, a path relative to the daemon account's home directory, or an absolute path on the daemon's machine, tried in that order; the path must lie inside an exported directory, whose grants apply; `irsync HOST:` lists the exports this client may reach with their real paths, `irsync HOST:PATH` alone lists a directory; the trailing-slash rule as before. Quick start keeps `home:photos/` and adds one absolute example (`irsync -av ~/Documents/ home:/backup/laptop/Documents/`) with `export add backup:/backup` on the server. `docs/daemon.md`: the hint sentence names `grant list <id>`; the isolated example may stay `data/`.

- [ ] **Step 4: Run the whole suite, fmt and clippy**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`
Expected: all clean and green.

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/manage/exports.rs crates/irsync/tests/manage_exports.rs README.md docs
git commit -m "docs: HOST:PATH addressing, listings and the absolute-path quick start; export add expands ~"
```
