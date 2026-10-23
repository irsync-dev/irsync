# Zero-config defaults and the exports CLI: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** irsync works end to end with no hand-written config file: identities appear on demand, one `irsync.toml` holds settings, `hosts.toml` holds aliases, `exports.toml` holds what the daemon shares and with whom, and top-level commands edit those files while the daemon re-reads them per connection.

**Architecture:** Three TOML files parsed by `config::{settings,hosts,exports}`; a `Snapshot` (exports plus resolved grants) produced by the `Authorizer` trait, which the daemon asks for on every connection and which opens indexes lazily by export path; a `manage` module that edits the hosts and exports files with `toml_edit` and writes them atomically; thin `main.rs` dispatch for the new `add/remove/allow/revoke/host/show/id` commands. "module" is renamed "export" everywhere first, as its own commit, so later diffs stay readable.

**Tech Stack:** Rust 2021 workspace; `toml` 1 (parsing), new dependency `toml_edit` 0.25 (editing), `ssh-key` 0.6, `clap` 4 derive, `iroh` 1.3, `tempfile` for tests. Out-of-process tests use `env!("CARGO_BIN_EXE_irsync")` as `tests/isolated_e2e.rs` does.

**Spec:** `docs/superpowers/specs/2026-10-23-zero-config-cli-design.md`

## Global Constraints

- No backward compatibility with current files, flags or wire protocol (spec: "nothing here keeps compatibility").
- Default paths, exactly: `~/.config/irsync/irsync.toml`, `~/.config/irsync/hosts.toml`, `~/.config/irsync/exports.toml`, `~/.config/irsync/id_ed25519`, `~/.config/irsync/daemon_ed25519`; state dir unchanged (`$XDG_STATE_HOME/irsync` else `~/.local/state/irsync`).
- `~/.ssh/id_ed25519`, `-F`, `$IRSYNC_CONFIG`, `/etc/irsync`, `irsyncd.toml`, `authorized_keys`, `--foreground` are removed, not deprecated.
- A missing `irsync.toml`, `hosts.toml` or `exports.toml` equals an empty one; a file that does not parse is an error carrying `<path>:<line>: <message>`.
- Every error about a file carries its path.
- Generated keys: unencrypted OpenSSH Ed25519, mode 0600, `.pub` beside, comment `irsync` (client) or `irsync-daemon` (daemon); never written over an existing file.
- Reserved names: an export may not be called `rw`, `ro` or `all`; an alias may not parse as an EndpointId and may not contain `:` or `/`.
- Grant entries resolve alias first, then raw id; an entry that resolves to nothing matches nobody.
- `hosts.toml` and `exports.toml` are written atomically (temp file in the same directory, then rename).
- The word "module" does not survive in CLI text, config keys, log lines, docs, Rust identifiers or the proto.
- Every commit ends with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.

## Review Focus

1. `irsync add rw:/srv/x` (a reserved export name): refused with a message naming the reserved words, nothing written. Pinned in Task 7.
2. `irsync host add k51qzi... <id>` (an alias that parses as an id) or `host add a:b <id>`: refused; and a destination `home:photos/` with no such alias: error names `hosts.toml` and says the string is not an id either. Pinned in Tasks 8 and 4.
3. `irsync allow photos laptop` when `photos.rw` already holds laptop's raw id: one entry remains, the alias; no duplicates in either array. Pinned in Task 7.
4. An existing passphrase-protected key at the default path with no passphrase source and no tty: error names the path and no key is generated over it. Pinned in Task 4.
5. `irsync add photos:docs` from `/srv` (relative path) writes `/srv/docs`; `irsync add photos:/etc/passwd` (not a directory) is refused. Pinned in Task 7.

---

### Task 1: Rename module to export

**Files:**
- Modify: `proto/irsync/v1/common.proto` (`Hello.module` → `export`, `NO_SUCH_MODULE` → `NO_SUCH_EXPORT`, comments)
- Modify: `crates/irsync/src/config/daemon.rs` (`Module` → `Export`, `modules` → `exports`, TOML key `[modules.x]` → `[exports.x]`)
- Modify: `crates/irsync/src/daemon.rs`, `crates/irsync/src/authz.rs`, `crates/irsync/src/session/engine.rs`, `crates/irsync/src/session/mod.rs`, `crates/irsync/src/main.rs`, `crates/irsync/src/cli/mod.rs`, `crates/irsync/src/cli/location.rs`, `crates/irsync/src/warm.rs`, `crates/irsync/src/tree_hash.rs`
- Modify: every file under `crates/irsync/tests/` and `crates/stress/src/` that `grep -rliw 'module\|modules'` lists (26 files at the time of writing)
- Modify: `README.md`, `docs/daemon.md`, `docs/protocol.md`, `docs/IRSYNC_VS_RSYNC.md`, `crates/stress/README.md` where they say module

**Interfaces:**
- Produces: `irsync::config::daemon::Export` (was `Module`), `DaemonConfig::exports`, `Hello::export`, `Code::NoSuchExport`, `Grants::exports` (was `Grants::modules`), test helper `common::spawn_daemon_exports` (was `spawn_daemon_modules`), `authorized_keys` option spelled `exports="..."`.

- [ ] **Step 1: Rename in the proto and regenerate**

Edit `common.proto`: `string export = 3; // the daemon-side export`, `bytes path = 4; // relative to the export root`, `NO_SUCH_EXPORT = 2;`. Field numbers unchanged, so `PROTOCOL_VERSION` is not bumped.

- [ ] **Step 2: Rename every Rust identifier, string and TOML key**

Run `grep -rnwi 'module\|modules' crates proto docs README.md` and rename each occurrence: `Module`→`Export`, `module`→`export`, `modules`→`exports`, `NoSuchModule`→`NoSuchExport`, `spawn_daemon_modules`→`spawn_daemon_exports`, `[modules.` →`[exports.`, `modules="`→`exports="` (the authorized_keys option, which Task 3 deletes), the `irsync index --daemon` help text, log fields `module = %name` → `export = %name`, error text `"no such module"`→`"no such export"`, `"path escapes module"`→`"path escapes export"`, `"module is read-only for this key"`→`"export is read-only for this key"`, `"no access to module"`→`"no access to export"`. In prose comments use "export" too.

- [ ] **Step 3: Verify nothing is left**

Run: `grep -rnwi 'module\|modules' crates proto docs README.md | grep -v target`
Expected: no output.

- [ ] **Step 4: Build and run the whole suite**

Run: `cargo build --workspace && cargo test --workspace 2>&1 | tail -5`
Expected: all green; the only behavioural change is the spelling of `exports="..."` in authorized_keys, which the tests in `tests/authz.rs` and `tests/isolated_e2e.rs` now use.

- [ ] **Step 5: Commit**

```bash
git add -A proto crates docs README.md
git commit -m "refactor: rename module to export everywhere, including the wire protocol

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: Parsers for the three files

**Files:**
- Create: `crates/irsync/src/config/paths.rs`
- Create: `crates/irsync/src/config/settings.rs`
- Create: `crates/irsync/src/config/hosts.rs`
- Create: `crates/irsync/src/config/exports.rs`
- Modify: `crates/irsync/src/config/mod.rs` (declare the modules; add `ConfigError::File { path, line: Option<usize>, message }`, `ConfigError::UnknownAlias { alias, hosts: PathBuf }`, `ConfigError::ReservedName(String)`, `ConfigError::BadPeer(String)`; keep `expand_tilde`, `duration`)
- Test: `crates/irsync/tests/config_files.rs` (new; `tests/config.rs` is deleted in Task 4)

The old `config/client.rs` and `config/daemon.rs` stay until Tasks 4 and 5 remove their last callers.

**Interfaces:**
- Produces, in `config::paths`:
  - `pub fn config_dir() -> PathBuf` (`~/.config/irsync`, home from `dirs::home_dir()`, `.` if none)
  - `pub fn state_dir() -> PathBuf` (moved from `main.rs`, same body)
  - `pub struct Paths { pub config: PathBuf, pub hosts: PathBuf, pub exports: PathBuf }`
  - `impl Paths { pub fn resolve(config: Option<&Path>, hosts: Option<&Path>, exports: Option<&Path>) -> Paths }` (each `None` is the default under `config_dir()`)
- Produces, in `config::settings`:
  - `pub struct ClientSettings { pub parallel: Option<u32>, pub reconnect: Option<Duration>, pub dht: Option<bool>, pub mdns: Option<bool>, pub relay: Option<Url> }`
  - `pub struct DaemonSettings { pub identity: PathBuf, pub idle_timeout: Duration, pub dormant_session_timeout: Duration, pub listen: Vec<HostPort>, pub portmap: bool, pub relay: Option<Url>, pub relay_mode: RelayMode, pub discovery: Discovery }` (`RelayMode`, `Discovery`, `de_duration`, `de_listen`, `listen_sockets` move here from `config/daemon.rs`)
  - `pub struct Settings { pub identity: PathBuf, pub state_dir: PathBuf, pub client: ClientSettings, pub daemon: DaemonSettings }` with `Default` = the spec's defaults (`identity` = `config_dir()/id_ed25519`, `daemon.identity` = `config_dir()/daemon_ed25519`, `state_dir` = `paths::state_dir()`, `idle_timeout` = `DAEMON_IDLE_TIMEOUT`, `dormant_session_timeout` = 7 days, `portmap` = true, discovery all true)
  - `impl Settings { pub fn parse(text: &str, path: &Path) -> Result<Settings, ConfigError>; pub fn load(path: &Path) -> Result<Settings, ConfigError> }` (`load`: missing file → `Settings::default()`; `deny_unknown_fields` on every table; `~` expanded in the three path fields)
- Produces, in `config::hosts`:
  - `pub struct HostEntry { pub endpoint_id: EndpointId, pub relay: Option<Url>, pub address: Vec<HostPort>, pub parallel: Option<u32>, pub reconnect: Option<Duration>, pub dht: Option<bool>, pub mdns: Option<bool> }`
  - `pub struct Hosts { pub path: PathBuf, pub entries: BTreeMap<String, HostEntry> }`
  - `impl Hosts { pub fn parse(text: &str, path: &Path) -> Result<Hosts, ConfigError>; pub fn load(path: &Path) -> Result<Hosts, ConfigError>; pub fn get(&self, alias: &str) -> Option<&HostEntry>; pub fn resolve_peer(&self, s: &str) -> Result<EndpointId, ConfigError> }` (`resolve_peer`: alias first, then `identity::parse_endpoint_id`, else `ConfigError::UnknownAlias`)
  - `pub fn valid_alias(s: &str) -> bool` (non-empty, no `:` or `/`, not parseable as an EndpointId)
- Produces, in `config::exports`:
  - `pub struct Export { pub path: PathBuf, pub read_only: bool, pub warm_index: bool, pub rw: Vec<String>, pub ro: Vec<String> }`
  - `pub struct Exports { pub path: PathBuf, pub rw: Vec<String>, pub ro: Vec<String>, pub exports: BTreeMap<String, Export> }`
  - `impl Exports { pub fn parse(text: &str, path: &Path) -> Result<Exports, ConfigError>; pub fn load(path: &Path) -> Result<Exports, ConfigError> }`
  - `pub const RESERVED: [&str; 3] = ["rw", "ro", "all"]`
- Consumes: `identity::parse_endpoint_id`, `net::addr::HostPort`, `config::duration::parse`.

- [ ] **Step 1: Write the failing tests**

`crates/irsync/tests/config_files.rs`:

```rust
use irsync::config::exports::Exports;
use irsync::config::hosts::{valid_alias, Hosts};
use irsync::config::settings::{RelayMode, Settings};
use irsync::config::ConfigError;
use std::path::Path;
use std::time::Duration;

fn id(n: u8) -> iroh::EndpointId { iroh::SecretKey::from_bytes(&[n; 32]).public() }
fn p() -> &'static Path { Path::new("/x/f.toml") }

#[test]
fn settings_defaults_when_empty() {
    let s = Settings::parse("", p()).unwrap();
    assert!(s.identity.ends_with(".config/irsync/id_ed25519"));
    assert!(s.daemon.identity.ends_with(".config/irsync/daemon_ed25519"));
    assert_eq!(s.daemon.idle_timeout, Duration::from_secs(30 * 60));
    assert_eq!(s.daemon.dormant_session_timeout, Duration::from_secs(7 * 86400));
    assert!(s.daemon.portmap && s.daemon.discovery.dht && s.daemon.discovery.dns && s.daemon.discovery.mdns);
    assert_eq!(s.client.parallel, None);
}

#[test]
fn settings_tables_parse_and_expand_tilde() {
    let t = "identity = \"~/k\"\n[client]\nparallel = 8\nreconnect = \"1h\"\n[daemon]\nidentity = \"~/d\"\nrelay_mode = \"disabled\"\nlisten = [\"127.0.0.1:0\"]\n[daemon.discovery]\ndht = false\n";
    let s = Settings::parse(t, p()).unwrap();
    assert_eq!(s.identity, dirs::home_dir().unwrap().join("k"));
    assert_eq!(s.daemon.identity, dirs::home_dir().unwrap().join("d"));
    assert_eq!(s.client.parallel, Some(8));
    assert_eq!(s.client.reconnect, Some(Duration::from_secs(3600)));
    assert_eq!(s.daemon.relay_mode, RelayMode::Disabled);
    assert_eq!(s.daemon.listen.len(), 1);
    assert!(!s.daemon.discovery.dht && s.daemon.discovery.dns);
}

#[test]
fn settings_unknown_key_names_file_and_key() {
    let e = Settings::parse("[client]\nbogus = 1\n", p()).unwrap_err();
    let msg = e.to_string();
    assert!(msg.contains("/x/f.toml") && msg.contains("bogus"), "{msg}");
}

#[test]
fn settings_missing_file_is_default() {
    let d = tempfile::tempdir().unwrap();
    let s = Settings::load(&d.path().join("none.toml")).unwrap();
    assert_eq!(s.client.parallel, None);
}

#[test]
fn hosts_parse_and_resolve_alias_before_id() {
    let t = format!("[home]\nendpoint_id = \"{}\"\nparallel = 4\naddress = [\"192.168.1.10:4000\"]\n[laptop]\nendpoint_id = \"{}\"\n", id(1), id(2));
    let h = Hosts::parse(&t, p()).unwrap();
    assert_eq!(h.get("home").unwrap().endpoint_id, id(1));
    assert_eq!(h.get("home").unwrap().parallel, Some(4));
    assert_eq!(h.get("home").unwrap().address.len(), 1);
    assert_eq!(h.resolve_peer("laptop").unwrap(), id(2));
    assert_eq!(h.resolve_peer(&id(3).to_string()).unwrap(), id(3));
    match h.resolve_peer("nope") {
        Err(ConfigError::UnknownAlias { alias, hosts }) => { assert_eq!(alias, "nope"); assert_eq!(hosts, p()); }
        other => panic!("{other:?}"),
    }
}

#[test]
fn hosts_entry_without_endpoint_id_is_error_with_line() {
    let e = Hosts::parse("[home]\nparallel = 4\n", p()).unwrap_err().to_string();
    assert!(e.contains("/x/f.toml") && e.contains("endpoint_id"), "{e}");
}

#[test]
fn alias_validity() {
    assert!(valid_alias("home-server"));
    assert!(!valid_alias("") && !valid_alias("a:b") && !valid_alias("a/b"));
    assert!(!valid_alias(&id(1).to_string()));
}

#[test]
fn exports_parse_top_level_arrays_and_tables() {
    let t = "rw = [\"admin\"]\n[photos]\npath = \"/srv/photos\"\nrw = [\"laptop\"]\nro = [\"b2e1\"]\n[docs]\npath = \"/srv/docs\"\nread_only = true\nwarm_index = true\n";
    let x = Exports::parse(t, p()).unwrap();
    assert_eq!(x.rw, vec!["admin"]);
    assert_eq!(x.exports["photos"].rw, vec!["laptop"]);
    assert_eq!(x.exports["photos"].ro, vec!["b2e1"]);
    assert!(x.exports["docs"].read_only && x.exports["docs"].warm_index);
    assert_eq!(x.exports["docs"].path, Path::new("/srv/docs"));
}

#[test]
fn exports_reserved_table_name_is_error() {
    let e = Exports::parse("[all]\npath = \"/x\"\n", p()).unwrap_err();
    assert!(matches!(e, ConfigError::ReservedName(ref n) if n == "all"), "{e:?}");
}

#[test]
fn exports_missing_path_and_unknown_key_are_errors_with_file() {
    assert!(Exports::parse("[a]\nread_only = true\n", p()).unwrap_err().to_string().contains("/x/f.toml"));
    assert!(Exports::parse("[a]\npath = \"/x\"\nbogus = 1\n", p()).unwrap_err().to_string().contains("bogus"));
}

#[test]
fn exports_missing_file_is_empty() {
    let d = tempfile::tempdir().unwrap();
    let x = Exports::load(&d.path().join("none.toml")).unwrap();
    assert!(x.exports.is_empty() && x.rw.is_empty());
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p irsync --test config_files 2>&1 | head -20`
Expected: compile error, `irsync::config::settings` not found.

- [ ] **Step 3: Implement `config/paths.rs`, `config/settings.rs`, `config/hosts.rs`, `config/exports.rs` and the `ConfigError` variants**

Approach notes: `Settings`, `ClientSettings`, `DaemonSettings` derive `Deserialize` with `#[serde(default, deny_unknown_fields)]`; `parse` maps `toml::de::Error` to `ConfigError::File { path, line: e.span().map(|s| line_of(text, s.start)), message }`. `HostEntry` deserialises `endpoint_id` as `String` and converts with `parse_endpoint_id` in `Hosts::parse` (iroh's own serde form is not the printed string). `Exports::parse` parses into `toml::Table`, pops `rw`/`ro` as `Vec<String>`, rejects any remaining key that is not a table or whose name is in `RESERVED`, and deserialises each table into `Export` (`deny_unknown_fields`, `path` required). `Hosts::load`/`Exports::load`/`Settings::load`: `NotFound` → empty; other I/O → `ConfigError::Io`.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p irsync --test config_files`
Expected: 11 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/config crates/irsync/tests/config_files.rs
git commit -m "feat(config): parsers for irsync.toml, hosts.toml and exports.toml

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: Authorizer snapshot and the daemon's per-connection read

**Files:**
- Modify: `crates/irsync/src/authz.rs` (delete `FileAuthorizer::load`, `parse_line`, `exports_option`, the `ssh_key::authorized_keys` import; new `Snapshot`, new trait shape, new `FileAuthorizer`, `MemoryAuthorizer` gains exports)
- Modify: `crates/irsync/src/daemon.rs` (`Shared`, `Daemon::new`, `run`, `handle`, `validate`, `run_from_config`)
- Modify: `crates/irsync/tests/common/mod.rs` (`spawn_daemon`, `spawn_daemon_exports`, `Harness::warm`)
- Modify: `crates/irsync/tests/daemon_auth.rs` and every test that calls `spawn_daemon(…, Arc::new(MemoryAuthorizer…))` (drop the `Arc::new`)
- Delete: `crates/irsync/tests/authz.rs`; Create: `crates/irsync/tests/authz_snapshot.rs`

**Interfaces:**
- Consumes: `config::exports::{Export, Exports}`, `config::hosts::Hosts` (Task 2).
- Produces, in `authz`:
  - `pub struct Snapshot { pub exports: BTreeMap<String, Export>, grants: HashMap<EndpointId, Grants> }`
  - `impl Snapshot { pub fn from_files(hosts: &Hosts, exports: &Exports) -> Snapshot; pub fn grants(&self, id: &EndpointId) -> Grants }` (`from_files`: for each entry string, `hosts.resolve_peer`; unresolvable → `tracing::warn!(entry, export, "grant entry matches nobody")` and skipped; `rw` processed before `ro`, `ro` never downgrades; top-level arrays fill `Grants::all`)
  - `pub trait Authorizer: Send + Sync { fn snapshot(&self) -> Result<Snapshot, AuthzError>; }`
  - `pub struct FileAuthorizer { hosts: PathBuf, exports: PathBuf }` with `pub fn new(hosts: PathBuf, exports: PathBuf) -> Self`; `snapshot()` loads both files fresh, `ConfigError` → `AuthzError::Config(#[from] ConfigError)`.
  - `pub struct MemoryAuthorizer { … }` keeps `single(id, grants)` and `with(id, grants)`, adds `pub fn with_exports(self, exports: BTreeMap<String, Export>) -> Self`; `snapshot()` clones.
  - `Grants` unchanged (`exports: BTreeMap<String, Access>`, `all: Option<Access>`, `allows`, `is_empty`).
- Produces, in `daemon`:
  - `pub fn Daemon::new(settings: &Settings, authz: Arc<dyn Authorizer>, endpoint: Endpoint) -> Result<Daemon>` (no exports argument; `state_dir`, `dormant_session_timeout`, `idle_timeout` read from `settings`)
  - `pub async fn run_from_settings(settings: Settings, paths: &Paths, pass: &PassphraseSource, shutdown: CancellationToken) -> Result<()>` (replaces `run_from_config`; builds `FileAuthorizer::new(paths.hosts.clone(), paths.exports.clone())`; takes one startup snapshot: `Err` → fatal; empty `exports` → `tracing::warn!(file = %paths.exports.display(), "no exports defined; irsync add creates one")`)
  - `pub fn show_id(settings: &Settings, pass: &PassphraseSource) -> Result<String>`
- Produces, in test `common`: `pub async fn spawn_daemon(exports: Vec<(&str, PathBuf, bool)>, authz: MemoryAuthorizer) -> DaemonHandle` and `pub async fn spawn_daemon_exports(exports: Vec<(&str, Export)>, authz: MemoryAuthorizer) -> DaemonHandle` (both call `authz.with_exports(map)` and `Daemon::new(&Settings { state_dir, ..Default::default() }, Arc::new(authz), ep)`).

- [ ] **Step 1: Write the failing snapshot tests**

`crates/irsync/tests/authz_snapshot.rs`:

```rust
use irsync::authz::{Access, FileAuthorizer, Authorizer, Snapshot};
use irsync::config::{exports::Exports, hosts::Hosts};
use std::path::Path;

fn id(n: u8) -> iroh::EndpointId { iroh::SecretKey::from_bytes(&[n; 32]).public() }
fn snap(hosts: &str, exports: &str) -> Snapshot {
    Snapshot::from_files(&Hosts::parse(hosts, Path::new("h")).unwrap(), &Exports::parse(exports, Path::new("e")).unwrap())
}

#[test]
fn alias_and_raw_id_entries_both_grant() {
    let s = snap(&format!("[laptop]\nendpoint_id = \"{}\"\n", id(1)),
                 &format!("[photos]\npath = \"/p\"\nrw = [\"laptop\"]\nro = [\"{}\"]\n", id(2)));
    assert!(s.grants(&id(1)).allows("photos", true));
    assert!(s.grants(&id(2)).allows("photos", false) && !s.grants(&id(2)).allows("photos", true));
    assert!(s.grants(&id(3)).is_empty());
}

#[test]
fn per_export_entry_wins_over_top_level_and_rw_wins_over_ro() {
    let s = snap("", &format!("rw = [\"{a}\"]\nro = [\"{b}\"]\n[x]\npath = \"/x\"\nro = [\"{a}\"]\nrw = [\"{b}\"]\n[y]\npath = \"/y\"\nrw = [\"{a}\"]\nro = [\"{a}\"]\n", a = id(1), b = id(2)));
    assert!(!s.grants(&id(1)).allows("x", true) && s.grants(&id(1)).allows("other", true));
    assert!(s.grants(&id(2)).allows("x", true) && !s.grants(&id(2)).allows("other", true));
    assert!(s.grants(&id(1)).allows("y", true));
}

#[test]
fn unresolvable_entry_matches_nobody() {
    let s = snap("", "[x]\npath = \"/x\"\nrw = [\"ghost\", \"not-an-id\"]\n");
    assert_eq!(s.exports.len(), 1);
    assert!(s.grants(&id(1)).is_empty());
}

#[test]
fn file_authorizer_reads_fresh_each_call_and_fails_closed() {
    let d = tempfile::tempdir().unwrap();
    let (h, e) = (d.path().join("hosts.toml"), d.path().join("exports.toml"));
    let a = FileAuthorizer::new(h.clone(), e.clone());
    assert!(a.snapshot().unwrap().exports.is_empty());            // missing = empty
    std::fs::write(&e, format!("[x]\npath = \"/x\"\nrw = [\"{}\"]\n", id(1))).unwrap();
    assert!(a.snapshot().unwrap().grants(&id(1)).allows("x", true));
    std::fs::write(&e, "[x\n").unwrap();
    assert!(a.snapshot().is_err());                                // broken = error, not stale
    std::fs::write(&e, "[x]\npath = \"/x\"\nrw = [\"lap\"]\n").unwrap();
    std::fs::write(&h, format!("[lap]\nendpoint_id = \"{}\"\n", id(1))).unwrap();
    assert!(a.snapshot().unwrap().grants(&id(1)).allows("x", true));
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p irsync --test authz_snapshot 2>&1 | head`
Expected: compile error, no `Snapshot`.

- [ ] **Step 3: Rewrite `authz.rs` to the interface above**

- [ ] **Step 4: Run the snapshot tests**

Run: `cargo test -p irsync --test authz_snapshot`
Expected: 4 passed.

- [ ] **Step 5: Write the failing daemon tests**

Add to `crates/irsync/tests/daemon_auth.rs` (keeping the existing cases, which now build their daemon with `spawn_daemon(vec![("rw", dir, false), ("ro", dir, true)], MemoryAuthorizer::single(client.id(), g))`):

```rust
/// A daemon started with an empty exports file serves an export added
/// afterwards, and stops serving a key revoked afterwards, with no restart.
#[tokio::test]
async fn exports_file_is_read_on_every_connection() {
    let d = tempfile::tempdir().unwrap();
    let (hosts, exports) = (d.path().join("hosts.toml"), d.path().join("exports.toml"));
    let client = client_endpoint().await;
    let authz = Arc::new(FileAuthorizer::new(hosts, exports.clone()));
    let daemon = spawn_daemon_with(authz).await;          // new helper, below
    let h = hello("data", b"", Direction::DaemonSends);
    assert!(matches!(exchange_against(&daemon, &client, h.clone()).await, Err(_)), "rejected before Hello: no grant");
    std::fs::write(&exports, format!("[data]\npath = {:?}\nro = [\"{}\"]\n", d.path().join("data"), client.id())).unwrap();
    std::fs::create_dir_all(d.path().join("data")).unwrap();
    assert!(matches!(exchange_against(&daemon, &client, h.clone()).await, Ok(HelloAckOrError::Ack(_))));
    std::fs::write(&exports, "[x\n").unwrap();            // broken: fail closed, daemon alive
    assert!(matches!(exchange_against(&daemon, &client, h.clone()).await, Err(_)));
    std::fs::write(&exports, format!("[data]\npath = {:?}\n", d.path().join("data"))).unwrap();
    assert!(matches!(exchange_against(&daemon, &client, h).await, Err(_)), "revoked");
}
```

Add to `tests/common/mod.rs`: `pub async fn spawn_daemon_with(authz: Arc<dyn Authorizer>) -> DaemonHandle` (the body of `spawn_daemon_exports` after the authorizer is built), and in `daemon_auth.rs` factor the connect-and-exchange part of `exchange_with` into `async fn exchange_against(d: &DaemonHandle, client: &Endpoint, h: Hello) -> Result<HelloAckOrError, String>`.

- [ ] **Step 6: Run to verify it fails**

Run: `cargo test -p irsync --test daemon_auth exports_file_is_read_on_every_connection 2>&1 | head`
Expected: compile error (`spawn_daemon_with`, `Daemon::new` signature).

- [ ] **Step 7: Rewrite `daemon.rs`**

`Shared` gets `authz`, `state_dir: PathBuf`, `indexes: Mutex<HashMap<PathBuf, IndexHandle>>`, `tasks: CancellationToken` (set in `run`), and loses the static `exports` map. Add `fn index_for(shared: &Arc<Shared>, export: &Export) -> Result<IndexHandle>`: under the mutex, return the cached handle for `export.path` or open `Index::open(&shared.state_dir, &export.path)`, insert, and spawn the GC loop and (if `warm_index`) the warm-up task for that path, both cancelled by `shared.tasks`. `run`: take one snapshot, open every export through `index_for`, then accept as before. `handle`: `shared.authz.snapshot()`; `Err(e)` → `tracing::warn!("{e:#}")`, `conn.close(1, b"unauthorized")`, return `Ok(())`; else `snap.grants(&id)`, reject if empty as today; after the Hello, `validate(&snap, &grants, &hello)` (same checks, `snap.exports.get(&hello.export)`), then `index_for(&shared, export)`. Move `run_from_config` to `run_from_settings` per the interface; `show_id` takes `&Settings`. `main.rs` keeps compiling by calling `run_from_settings(Settings::load(...)?, …)` with `Paths::resolve(d.config, None, None)`; the full CLI wiring is Task 5.

- [ ] **Step 8: Update the test helper and callers**

`common::spawn_daemon`, `spawn_daemon_exports`, `spawn_daemon_with`, `Harness::warm` per the interface. Change every `spawn_daemon(…, Arc::new(MemoryAuthorizer…))` call to pass the `MemoryAuthorizer` by value (`grep -rn "Arc::new(MemoryAuthorizer" crates/irsync/tests`). Delete `tests/authz.rs`.

- [ ] **Step 9: Run the whole crate**

Run: `cargo test -p irsync 2>&1 | grep -E "^test result|FAILED|panicked" | head -40`
Expected: every test binary passes; the new `daemon_auth` case included.

- [ ] **Step 10: Commit**

```bash
git add -A crates/irsync
git commit -m "feat(daemon): read hosts.toml and exports.toml on every connection; drop authorized_keys

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: Identities on demand and the client's new config path

**Files:**
- Modify: `crates/irsync/src/identity.rs` (add `load_or_generate`)
- Modify: `crates/irsync/src/main.rs` (`sync`: `Settings` + `Hosts` instead of `ClientConfig`; identity chain; `resolve_target`)
- Modify: `crates/irsync/src/net/endpoint.rs` (`client_endpoint_opts` takes `&ClientSettings` and `Option<&HostEntry>` instead of `&HostConfig`)
- Modify: `crates/irsync/src/cli/mod.rs` (`SyncArgs`: `-F` → `--config PATH`; add `--hosts PATH`; `SyncOptions` gains `hosts: Option<PathBuf>`)
- Delete: `crates/irsync/src/config/client.rs`; remove `pub mod client` from `config/mod.rs`
- Modify: `crates/irsync/tests/cli.rs` (`-F` → `--config`), `crates/irsync/tests/identity.rs`, `crates/irsync/tests/isolated_e2e.rs` (the client keygen step goes away: the client generates its own key under `HOME`; the daemon config becomes the new files), `crates/stress/src/client.rs` (`-F` → `--config`)

**Interfaces:**
- Consumes: `Settings`, `Hosts`, `HostEntry`, `Paths` (Task 2).
- Produces, in `identity`: `pub fn load_or_generate(path: &Path, src: &PassphraseSource, comment: &str) -> Result<(SecretKey, bool), IdentityError>` (`true` = generated now; generation = `generate_openssh` after `create_dir_all(parent)`; an existing unreadable or encrypted-without-passphrase key is the existing error, path included, never regenerated). `daemon::show_id` and `run_from_settings` use it with comment `irsync-daemon`.
- Produces, in `net::endpoint`: `pub fn client_endpoint_opts(secret: SecretKey, cli_relay: Option<&str>, no_dht: bool, client: &ClientSettings, host: Option<&HostEntry>, addresses: &[HostPort]) -> anyhow::Result<EndpointOpts>` (precedence per field: `cli_relay`, then `host`, then `client`, then default).
- Produces, in `main`: `fn resolve_target(hosts: &Hosts, host: &str) -> anyhow::Result<EndpointId>` with the error text `"{host}" is not an alias in {hosts.path} and not an EndpointId` (append `; no such file` when `!hosts.path.exists()`); the client identity is `opts.identity`, else `settings.identity`, through `load_or_generate(…, "irsync")`; when generated, print on stderr: `generated identity {path}`, then the public key line, then the EndpointId.

- [ ] **Step 1: Write the failing identity tests**

Append to `crates/irsync/tests/identity.rs`:

```rust
use irsync::identity::{load_or_generate, IdentityError, PassphraseSource};
use std::os::unix::fs::PermissionsExt;

#[test]
fn load_or_generate_creates_once_then_loads_same_key() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("deep/dir/id_ed25519");
    let src = PassphraseSource::default();
    let (k1, gen1) = load_or_generate(&p, &src, "irsync").unwrap();
    let (k2, gen2) = load_or_generate(&p, &src, "irsync").unwrap();
    assert!(gen1 && !gen2);
    assert_eq!(k1.public(), k2.public());
    assert!(d.path().join("deep/dir/id_ed25519.pub").exists());
    assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn encrypted_existing_key_without_passphrase_is_error_not_regenerated() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("k");
    let key = ssh_key::PrivateKey::random(&mut rand_core::OsRng, ssh_key::Algorithm::Ed25519).unwrap();
    let enc = key.encrypt(&mut rand_core::OsRng, "pw").unwrap();
    enc.write_openssh_file(&p, ssh_key::LineEnding::LF).unwrap();
    let before = std::fs::read(&p).unwrap();
    let e = load_or_generate(&p, &PassphraseSource::default(), "irsync").unwrap_err();
    assert!(matches!(e, IdentityError::NoPassphraseSource(ref q) if q == &p), "{e}");
    assert_eq!(std::fs::read(&p).unwrap(), before);
}
```

(`PassphraseSource::default()` has `prompt: false`, so no tty is consulted. Add `ssh-key` and `rand_core` to `[dev-dependencies]` if `cargo test` says they are missing; `rand_core` already is.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p irsync --test identity 2>&1 | head`
Expected: compile error, no `load_or_generate`.

- [ ] **Step 3: Implement `load_or_generate` and use it in `daemon.rs`**

- [ ] **Step 4: Run the identity tests**

Run: `cargo test -p irsync --test identity`
Expected: 3 passed.

- [ ] **Step 5: Write the failing out-of-process test for the client's first run**

Replace the keygen and authorized_keys steps of `client_and_daemon_sync_over_localhost_with_no_relay_or_lookup` in `tests/isolated_e2e.rs`. New flow: write `tmp/irsync.toml` with `state_dir`, `[daemon] identity = tmp/daemon_key, relay_mode = "disabled", portmap = false`, `[daemon.discovery]` all false; run `irsync id --config tmp/irsync.toml` with `HOME=tmp/home` (new command from Task 9; until then run `irsync keygen --out tmp/home/.config/irsync/id_ed25519` and read line 2 for the id, and note the `TODO(task 9)` in the test); write `tmp/exports.toml` as `[m]\npath = <dst>\nrw = ["<client-id>"]\n`; start the daemon with `--config tmp/irsync.toml --exports tmp/exports.toml --listen 127.0.0.1:0`; run the client with `--config tmp/irsync.toml` (and no `--identity`), asserting that the sync succeeds and that `tmp/home/.config/irsync/id_ed25519` exists afterwards. Keep the rest of the test as it is. Also add:

```rust
#[test]
fn unknown_alias_error_names_hosts_file() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path();
    std::fs::create_dir_all(tmp.join("home")).unwrap();
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    let o = run_client(&["-a", "--hosts", tmp.join("hosts.toml").to_str().unwrap(), tmp.join("src/").to_str().unwrap(), "home:photos/"], tmp);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("\"home\" is not an alias in") && err.contains("hosts.toml") && err.contains("no such file"), "{err}");
}
```

`run_client` drops `.env_remove("IRSYNC_CONFIG")`.

- [ ] **Step 6: Run to verify it fails**

Run: `cargo test -p irsync --test isolated_e2e 2>&1 | tail -15`
Expected: `--config`/`--hosts` unknown, or the old `-F` path taken.

- [ ] **Step 7: Rewire the client**

`SyncArgs`: `#[arg(long, value_name = "PATH")] pub config: Option<PathBuf>` and `pub hosts: Option<PathBuf>`; `SyncOptions.hosts`. `sync()` in `main.rs`: `let paths = Paths::resolve(opts.config.as_deref(), opts.hosts.as_deref(), None); let settings = Settings::load(&paths.config)?; let hosts = Hosts::load(&paths.hosts)?; let host = hosts.get(host_str); let target = resolve_target(&hosts, host_str)?;` then `parallel`/`reconnect`/addresses from `host` then `settings.client`; identity via `load_or_generate`; `client_endpoint_opts(secret, …, &settings.client, host, &addresses)`. Delete `config/client.rs`, `tests/config.rs` (its ssh_config cases; the TOML ones live in `config_files.rs`), and the `-F` cases in `tests/cli.rs` (replace with `--config`). `crates/stress/src/client.rs`: `"-F"` → `"--config"`.

- [ ] **Step 8: Run the affected crates**

Run: `cargo test -p irsync --test isolated_e2e --test cli --test identity && cargo build -p irsync-stress`
Expected: all pass; the e2e test shows the sync succeeding with a generated client key.

- [ ] **Step 9: Commit**

```bash
git add -A crates
git commit -m "feat(client): irsync.toml and hosts.toml replace the ssh_config file; identity generated on demand

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 5: Daemon and index commands on the new files; remove the old daemon config

**Files:**
- Modify: `crates/irsync/src/cli/mod.rs` (`DaemonArgs`: remove `foreground`, add `hosts: Option<PathBuf>`, `exports: Option<PathBuf>`; `IndexArgs`: add `exports: Option<PathBuf>`, both `requires = "daemon"`)
- Modify: `crates/irsync/src/main.rs` (`run_daemon`, `run_index`)
- Delete: `crates/irsync/src/config/daemon.rs`; remove `pub mod daemon` from `config/mod.rs`
- Modify: `crates/stress/src/daemon.rs` (`DaemonConfig { config_dir, export_path, client_id }`; `write_config` writes `irsync.toml` and `exports.toml`, returns both paths; `client_keygen` returns `(PathBuf, String /*pub line*/, String /*id, line 2*/)`), `crates/stress/src/*.rs` callers, `crates/irsync/tests/gc.rs` (`daemon_show_id_prints_only_the_id` writes `irsync.toml`)
- Test: `crates/irsync/tests/cli.rs`, `crates/irsync/tests/gc.rs`, `crates/stress` unit tests

**Interfaces:**
- Consumes: `Settings`, `Exports`, `Paths`, `daemon::run_from_settings`, `daemon::show_id`.
- Produces: `irsync daemon [--config PATH] [--hosts PATH] [--exports PATH] [--show-id] [--passphrase-file PATH] [--listen ADDR]...`; `irsync index --daemon [--config PATH] [--exports PATH] [NAME...]` (names are export names from `exports.toml`; state dir from `settings.state_dir`).
- Produces, in stress: `pub fn write_config(c: &DaemonConfig) -> Result<DaemonFiles>` with `pub struct DaemonFiles { pub config: PathBuf, pub exports: PathBuf }`; `spawn` passes `--config` and `--exports`.

- [ ] **Step 1: Write the failing CLI and show-id tests**

In `tests/cli.rs`, replace `subcommands_parse`'s daemon case and `index_subcommand_parses_roots_or_daemon_exports` with:

```rust
#[test]
fn daemon_flags() {
    let a = parse(&["daemon", "--config", "/c", "--hosts", "/h", "--exports", "/e", "--show-id"]);
    let Some(Command::Daemon(d)) = a.cmd else { panic!() };
    assert_eq!((d.config, d.hosts, d.exports, d.show_id), (Some("/c".into()), Some("/h".into()), Some("/e".into()), true));
    assert!(Args::try_parse_from(["irsync", "daemon", "--foreground"]).is_err());
}

#[test]
fn index_daemon_takes_exports_file() {
    let a = parse(&["index", "--daemon", "--exports", "/e", "photos"]);
    let Some(Command::Index(ix)) = a.cmd else { panic!() };
    assert!(ix.daemon && ix.exports == Some("/e".into()) && ix.targets == vec![PathBuf::from("photos")]);
    assert!(Args::try_parse_from(["irsync", "index", "--exports", "/e", "photos"]).is_err());
}
```

In `tests/gc.rs`, `daemon_show_id_prints_only_the_id` writes `irsync.toml` as `[daemon]\nidentity = {:?}\n` (plus `state_dir` at top level) and passes `--config`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p irsync --test cli --test gc 2>&1 | grep -E "error|FAILED" | head`
Expected: `hosts`/`exports` fields missing.

- [ ] **Step 3: Implement the flags, `run_daemon`, `run_index`; delete `config/daemon.rs`**

`run_daemon`: `Paths::resolve(d.config, d.hosts, d.exports)`, `Settings::load`, `--listen` override into `settings.daemon.listen`, `show_id` or `run_from_settings`. `run_index --daemon`: `Exports::load(&paths.exports)`, unknown name → `no export {n:?} in {path}`.

- [ ] **Step 4: Update the stress harness**

`write_config` writes `irsync.toml` (`state_dir`, `[daemon]` with `identity`, `relay_mode = "disabled"`, `portmap = false`, `[daemon.discovery]` all false) and `exports.toml` (`[data]\npath = …\nrw = ["<client id>"]`). Update its unit test `write_config_writes_toml_and_authorized_keys` → `write_config_writes_settings_and_exports` asserting both files and that `exports.toml` parses with `irsync::config::exports::Exports::parse` (add `irsync` as a dev-dependency of the stress crate if it is not one; otherwise assert on the text).

- [ ] **Step 5: Run everything**

Run: `cargo test --workspace 2>&1 | grep -E "^test result|FAILED|panicked"`
Expected: all green. Then a smoke run of the harness: `cargo run -p irsync-stress -- --help` exits 0 (its README names the real invocation; run one short iteration if it takes under a minute).

- [ ] **Step 6: Commit**

```bash
git add -A crates
git commit -m "feat(daemon): settings from irsync.toml, exports from exports.toml; drop irsyncd.toml and --foreground

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 6: The editing layer (toml_edit, atomic writes, peer specs)

**Files:**
- Modify: `Cargo.toml` (workspace dep `toml_edit = "0.25"`), `crates/irsync/Cargo.toml`
- Create: `crates/irsync/src/manage/mod.rs` (`ManageError`, `load_doc`, `save_doc`, `PeerSpec`)
- Modify: `crates/irsync/src/lib.rs` (`pub mod manage`)
- Test: `crates/irsync/tests/manage_doc.rs`

**Interfaces:**
- Produces:
  - `pub enum ManageError { Io { path, source }, Config(ConfigError), Toml { path, source: toml_edit::TomlError }, Exists(String), NotFound(String), Reserved(String), BadAlias(String), NoOpenAccess, NotADirectory(PathBuf), NothingToRemove(String) }` with `Display` text per the spec's error rules (path always included where there is one).
  - `pub fn load_doc(path: &Path) -> Result<toml_edit::DocumentMut, ManageError>` (missing → empty document)
  - `pub fn save_doc(path: &Path, doc: &toml_edit::DocumentMut) -> Result<(), ManageError>` (create parent dir; write `path.with_extension("toml.tmp")`-style sibling via `tempfile::NamedTempFile::new_in(parent)`, `persist` over `path`; mode 0644)
  - `pub enum PeerSpec { Alias(String), Id { id: EndpointId, comment: Option<String> } }`
  - `pub fn PeerSpec::parse(who: &str, hosts: &Hosts) -> Result<PeerSpec, ManageError>` (order: `all`/`*` → `NoOpenAccess`; a known alias → `Alias`; a readable file whose first line is an OpenSSH public key → `Id` with the key's comment; a string starting with `ssh-` → parsed as an OpenSSH public key line, non-Ed25519 → `ManageError::Config(BadPeer)`; else `parse_endpoint_id`; else `ManageError::Config(BadPeer(who))`)
  - `impl PeerSpec { pub fn entry(&self) -> String }` (the alias, or the id in its hex form as `EndpointId::to_string` prints it)
- Consumes: `Hosts`, `identity::{parse_endpoint_id, endpoint_id_of_public_key}`.

- [ ] **Step 1: Write the failing tests**

`crates/irsync/tests/manage_doc.rs`:

```rust
use irsync::config::hosts::Hosts;
use irsync::manage::{load_doc, save_doc, ManageError, PeerSpec};
use std::path::Path;

fn id(n: u8) -> iroh::EndpointId { iroh::SecretKey::from_bytes(&[n; 32]).public() }

#[test]
fn load_missing_is_empty_and_save_creates_parent_atomically() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("sub/exports.toml");
    let mut doc = load_doc(&p).unwrap();
    assert!(doc.as_table().is_empty());
    doc["x"]["path"] = toml_edit::value("/x");
    save_doc(&p, &doc).unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "[x]\npath = \"/x\"\n");
    assert!(std::fs::read_dir(p.parent().unwrap()).unwrap().count() == 1, "no temp file left behind");
}

#[test]
fn save_preserves_comments_and_order() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("e.toml");
    std::fs::write(&p, "# top\n[b]\npath = \"/b\" # keep\n\n[a]\npath = \"/a\"\n").unwrap();
    let mut doc = load_doc(&p).unwrap();
    doc["a"]["read_only"] = toml_edit::value(true);
    save_doc(&p, &doc).unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "# top\n[b]\npath = \"/b\" # keep\n\n[a]\npath = \"/a\"\nread_only = true\n");
}

#[test]
fn peer_spec_forms() {
    let hosts = Hosts::parse(&format!("[laptop]\nendpoint_id = \"{}\"\n", id(1)), Path::new("h")).unwrap();
    assert!(matches!(PeerSpec::parse("laptop", &hosts).unwrap(), PeerSpec::Alias(ref a) if a == "laptop"));
    assert!(matches!(PeerSpec::parse(&id(2).to_string(), &hosts).unwrap(), PeerSpec::Id { id, .. } if id == id(2)));
    let key = ssh_key::PrivateKey::random(&mut rand_core::OsRng, ssh_key::Algorithm::Ed25519).unwrap();
    let mut pk = key.public_key().clone(); pk.set_comment("mike@laptop");
    let line = pk.to_openssh().unwrap();
    let want = irsync::identity::endpoint_id_of_public_key(&pk).unwrap();
    assert!(matches!(PeerSpec::parse(&line, &hosts).unwrap(), PeerSpec::Id { id, comment: Some(ref c) } if id == want && c == "mike@laptop"));
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("k.pub"), &line).unwrap();
    assert!(matches!(PeerSpec::parse(d.path().join("k.pub").to_str().unwrap(), &hosts).unwrap(), PeerSpec::Id { comment: Some(_), .. }));
    assert!(matches!(PeerSpec::parse("all", &hosts), Err(ManageError::NoOpenAccess)));
    assert!(matches!(PeerSpec::parse("*", &hosts), Err(ManageError::NoOpenAccess)));
    assert!(PeerSpec::parse("nobody", &hosts).is_err());
    assert_eq!(PeerSpec::parse("laptop", &hosts).unwrap().entry(), "laptop");
    assert_eq!(PeerSpec::parse(&id(2).to_string(), &hosts).unwrap().entry(), id(2).to_string());
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p irsync --test manage_doc 2>&1 | head`
Expected: compile error, no `irsync::manage`.

- [ ] **Step 3: Add `toml_edit`, implement `manage/mod.rs`**

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p irsync --test manage_doc`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/irsync/Cargo.toml crates/irsync/src/lib.rs crates/irsync/src/manage crates/irsync/tests/manage_doc.rs
git commit -m "feat(manage): format-preserving TOML editing, atomic saves, peer specs

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 7: `add`, `remove`, `allow`, `revoke`

**Files:**
- Create: `crates/irsync/src/manage/exports.rs`
- Modify: `crates/irsync/src/cli/mod.rs` (`FilesArgs`, `Command::{Add, Remove, Allow, Revoke}`)
- Modify: `crates/irsync/src/main.rs` (dispatch: `run_add`, `run_remove`, `run_allow`, `run_revoke`)
- Test: `crates/irsync/tests/manage_exports.rs` (library level), `crates/irsync/tests/cli.rs` (parsing), `crates/irsync/tests/commands_e2e.rs` (new, out of process)

**Interfaces:**
- Consumes: `load_doc`, `save_doc`, `PeerSpec`, `ManageError`, `Hosts`, `Exports::RESERVED`.
- Produces, in `manage::exports` (all pure on a `DocumentMut`; the `run_*` functions in `main.rs` do load, call, save, print):
  - `pub fn add_export(doc: &mut DocumentMut, name: &str, path: &Path, read_only: bool, warm_index: bool) -> Result<(), ManageError>` (`Reserved`, `Exists`; writes only `path` plus the flags that are true)
  - `pub fn remove_export(doc: &mut DocumentMut, name: &str) -> Result<(), ManageError>` (`NotFound`)
  - `pub fn allow(doc: &mut DocumentMut, export: Option<&str>, who: &PeerSpec, ro: bool, resolve: &dyn Fn(&str) -> Option<EndpointId>) -> Result<(), ManageError>` (`None` export = top level; `NotFound` for an unknown export; removes from both arrays any entry equal to `who.entry()` or resolving to the same id as `who`; appends `who.entry()` to `rw` or `ro`, with `# comment` as a trailing comment when `who` carries one)
  - `pub fn revoke(doc: &mut DocumentMut, export: Option<&str>, who: Option<&PeerSpec>, resolve: &dyn Fn(&str) -> Option<EndpointId>) -> Result<usize, ManageError>` (`who = None` empties both arrays of that export or of the top level; `export = None, who = Some` walks the top level and every export; matching as in `allow`; `0` removed → `NothingToRemove`)
  - `pub fn parse_add_target(arg: &str, cwd: &Path) -> Result<(String, PathBuf), ManageError>` (`NAME:PATH` splits at the first `:`; a bare path's name is its final component; the path is joined onto `cwd` when relative and must be an existing directory, else `NotADirectory`)
- Produces, in `cli`:
  - `#[derive(clap::Args)] pub struct FilesArgs { #[arg(long, value_name="PATH")] pub config: Option<PathBuf>, pub hosts: Option<PathBuf>, pub exports: Option<PathBuf> }`
  - `Command::Add(AddArgs { target: String, ro: bool, warm_index: bool, files: FilesArgs })`, `Command::Remove { name: String, files }`, `Command::Allow { export: String, who: String, ro: bool, files }`, `Command::Revoke { args: Vec<String> /* num_args = 1..=2 */, files }`
- Output of `run_add` (stderr line, then the daemon id on stdout): `added export {name} ({path}) to {exports_path}` / `{daemon id}`; `run_remove`: `removed export {name} from {path}`; `run_allow`: `{entry}: {rw|ro} on {export|all exports}`; `run_revoke`: `removed {n} entr(y|ies)`.

- [ ] **Step 1: Write the failing library tests**

`crates/irsync/tests/manage_exports.rs`:

```rust
use irsync::manage::exports::{add_export, allow, parse_add_target, remove_export, revoke};
use irsync::manage::{ManageError, PeerSpec};
use std::path::Path;
use toml_edit::DocumentMut;

fn id(n: u8) -> iroh::EndpointId { iroh::SecretKey::from_bytes(&[n; 32]).public() }
fn peer(n: u8) -> PeerSpec { PeerSpec::Id { id: id(n), comment: None } }
fn no_alias(_: &str) -> Option<iroh::EndpointId> { None }

#[test]
fn add_writes_only_what_was_asked_and_refuses_duplicates_and_reserved() {
    let mut doc = DocumentMut::new();
    add_export(&mut doc, "photos", Path::new("/srv/photos"), false, false).unwrap();
    add_export(&mut doc, "docs", Path::new("/srv/docs"), true, true).unwrap();
    let x = irsync::config::exports::Exports::parse(&doc.to_string(), Path::new("e")).unwrap();
    assert!(!x.exports["photos"].read_only && !x.exports["photos"].warm_index);
    assert!(x.exports["docs"].read_only && x.exports["docs"].warm_index);
    assert!(!doc.to_string().contains("read_only = false"), "only true flags are written:\n{doc}");
    assert!(matches!(add_export(&mut doc, "photos", Path::new("/x"), false, false), Err(ManageError::Exists(_))));
    for r in ["rw", "ro", "all"] {
        assert!(matches!(add_export(&mut doc, r, Path::new("/x"), false, false), Err(ManageError::Reserved(_))));
    }
}

#[test]
fn remove_drops_table_and_its_grants() {
    let mut doc: DocumentMut = "[a]\npath = \"/a\"\nrw = [\"x\"]\n[b]\npath = \"/b\"\n".parse().unwrap();
    remove_export(&mut doc, "a").unwrap();
    assert_eq!(doc.to_string(), "[b]\npath = \"/b\"\n");
    assert!(matches!(remove_export(&mut doc, "a"), Err(ManageError::NotFound(_))));
}

#[test]
fn allow_moves_between_arrays_replaces_other_spelling_and_keeps_comment() {
    let mut doc: DocumentMut = format!("[p]\npath = \"/p\"\nrw = [\"{}\"]\n", id(1)).parse().unwrap();
    let resolve = |s: &str| (s == "laptop").then(|| id(1));
    allow(&mut doc, Some("p"), &PeerSpec::Alias("laptop".into()), false, &resolve).unwrap();   // replaces the raw id
    assert_eq!(doc["p"]["rw"].as_array().unwrap().len(), 1);
    assert_eq!(doc["p"]["rw"].as_array().unwrap().get(0).unwrap().as_str(), Some("laptop"));
    allow(&mut doc, Some("p"), &PeerSpec::Alias("laptop".into()), true, &resolve).unwrap();    // rw -> ro
    assert!(doc["p"]["rw"].as_array().unwrap().is_empty());
    assert_eq!(doc["p"]["ro"].as_array().unwrap().len(), 1);
    allow(&mut doc, Some("p"), &PeerSpec::Id { id: id(2), comment: Some("mike@laptop".into()) }, false, &resolve).unwrap();
    assert!(doc.to_string().contains(&format!("\"{}\", # mike@laptop", id(2))) || doc.to_string().contains(&format!("\"{}\" # mike@laptop", id(2))), "{doc}");
    allow(&mut doc, None, &peer(3), true, &resolve).unwrap();                                   // top level
    assert_eq!(doc["ro"].as_array().unwrap().get(0).unwrap().as_str(), Some(id(3).to_string().as_str()));
    assert!(matches!(allow(&mut doc, Some("zz"), &peer(3), false, &resolve), Err(ManageError::NotFound(_))));
}

#[test]
fn revoke_by_export_by_all_and_everywhere() {
    let text = format!("rw = [\"{a}\"]\n[p]\npath = \"/p\"\nrw = [\"{a}\", \"{b}\"]\nro = [\"lap\"]\n[q]\npath = \"/q\"\nro = [\"{a}\"]\n", a = id(1), b = id(2));
    let resolve = |s: &str| (s == "lap").then(|| id(1));
    let mut doc: DocumentMut = text.parse().unwrap();
    assert_eq!(revoke(&mut doc, Some("p"), Some(&peer(1)), &resolve).unwrap(), 2);   // raw id and the alias that resolves to it
    assert_eq!(doc["p"]["rw"].as_array().unwrap().len(), 1);
    assert!(doc["p"]["ro"].as_array().unwrap().is_empty());
    let mut doc: DocumentMut = text.parse().unwrap();
    assert_eq!(revoke(&mut doc, None, Some(&peer(1)), &resolve).unwrap(), 4);        // everywhere
    let mut doc: DocumentMut = text.parse().unwrap();
    assert_eq!(revoke(&mut doc, Some("p"), None, &resolve).unwrap(), 3);              // all of p
    assert!(matches!(revoke(&mut doc, Some("p"), None, &resolve), Err(ManageError::NothingToRemove(_))));
}

#[test]
fn add_target_forms() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("docs")).unwrap();
    std::fs::write(d.path().join("f"), b"").unwrap();
    assert_eq!(parse_add_target("photos:docs", d.path()).unwrap(), ("photos".into(), d.path().join("docs")));
    assert_eq!(parse_add_target("docs", d.path()).unwrap(), ("docs".into(), d.path().join("docs")));
    assert_eq!(parse_add_target(d.path().join("docs").to_str().unwrap(), Path::new("/elsewhere")).unwrap().0, "docs");
    assert!(matches!(parse_add_target("x:f", d.path()), Err(ManageError::NotADirectory(_))));
    assert!(matches!(parse_add_target("x:missing", d.path()), Err(ManageError::NotADirectory(_))));
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p irsync --test manage_exports 2>&1 | head`
Expected: compile error, no `manage::exports`.

- [ ] **Step 3: Implement `manage/exports.rs`**

Note for `allow`'s comment: `toml_edit::Value::decor_mut().set_suffix(" # mike@laptop")` on the appended array item; keep arrays single-line unless they already span lines.

- [ ] **Step 4: Run the library tests**

Run: `cargo test -p irsync --test manage_exports`
Expected: 5 passed.

- [ ] **Step 5: Write the failing CLI parse and out-of-process tests**

`tests/cli.rs`:

```rust
#[test]
fn management_commands_parse() {
    let a = parse(&["add", "photos:/srv/photos", "--ro", "--warm-index", "--exports", "/e"]);
    let Some(Command::Add(x)) = a.cmd else { panic!() };
    assert!(x.target == "photos:/srv/photos" && x.ro && x.warm_index && x.files.exports == Some("/e".into()));
    assert!(matches!(parse(&["remove", "photos"]).cmd, Some(Command::Remove { ref name, .. }) if name == "photos"));
    assert!(matches!(parse(&["allow", "photos", "laptop", "--ro"]).cmd, Some(Command::Allow { ref export, ref who, ro: true, .. }) if export == "photos" && who == "laptop"));
    assert!(matches!(parse(&["revoke", "laptop"]).cmd, Some(Command::Revoke { ref args, .. }) if args.len() == 1));
    assert!(matches!(parse(&["revoke", "photos", "all"]).cmd, Some(Command::Revoke { ref args, .. }) if args.len() == 2));
    assert!(Args::try_parse_from(["irsync", "revoke"]).is_err());
    assert!(Args::try_parse_from(["irsync", "revoke", "a", "b", "c"]).is_err());
}
```

`crates/irsync/tests/commands_e2e.rs` (helper `fn irsync(home: &Path, args: &[&str]) -> std::process::Output` runs the binary with `HOME=home`, `XDG_STATE_HOME=home/xdg`, stdin null):

```rust
#[test]
fn add_allow_revoke_remove_round_trip_with_default_paths() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("srv/photos")).unwrap();
    let target = format!("photos:{}", home.join("srv/photos").display());
    let o = irsync(&home, &["add", &target]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8(o.stdout).unwrap();
    irsync::identity::parse_endpoint_id(out.trim()).expect("stdout is the daemon id");
    assert!(home.join(".config/irsync/daemon_ed25519").exists());
    let exports = home.join(".config/irsync/exports.toml");
    assert_eq!(std::fs::read_to_string(&exports).unwrap(), format!("[photos]\npath = {:?}\n", home.join("srv/photos")));
    let id = iroh::SecretKey::from_bytes(&[9; 32]).public().to_string();
    assert!(irsync(&home, &["allow", "photos", &id]).status.success());
    assert!(std::fs::read_to_string(&exports).unwrap().contains(&format!("rw = [\"{id}\"]")));
    let o = irsync(&home, &["allow", "photos", "all"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("no open access"));
    let o = irsync(&home, &["add", &format!("rw:{}", home.join("srv/photos").display())]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("reserved"));
    assert!(irsync(&home, &["revoke", &id]).status.success());
    assert!(!std::fs::read_to_string(&exports).unwrap().contains(&id));
    assert!(irsync(&home, &["remove", "photos"]).status.success());
    assert_eq!(std::fs::read_to_string(&exports).unwrap().trim(), "");
}
```

- [ ] **Step 6: Run to verify they fail**

Run: `cargo test -p irsync --test cli management_commands_parse --test commands_e2e 2>&1 | head`
Expected: `add` is not a subcommand.

- [ ] **Step 7: Implement the subcommands and `run_*` in `main.rs`**

`run_add`: `parse_add_target(arg, &std::env::current_dir()?)`, load/add/save, then `load_or_generate(&settings.daemon.identity, …, "irsync-daemon")` and print its id. `run_allow`/`run_revoke`: `Hosts::load(&paths.hosts)`, `PeerSpec::parse`, `resolve = |s| hosts.get(s).map(|h| h.endpoint_id)`. `revoke` with one arg: `who = Some`, `export = None`; with two: `export = Some(args[0])` unless `args[0] == "all"` (then `None`), `who = None` when `args[1] == "all"` else `Some`.

- [ ] **Step 8: Run the tests**

Run: `cargo test -p irsync --test cli --test commands_e2e --test manage_exports`
Expected: all pass.

- [ ] **Step 9: Commit**

```bash
git add -A crates/irsync
git commit -m "feat(cli): irsync add, remove, allow, revoke edit exports.toml

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 8: `host add`, `host remove`, `host list`

**Files:**
- Create: `crates/irsync/src/manage/hosts.rs`
- Modify: `crates/irsync/src/cli/mod.rs` (`Command::Host(HostCmd)`, `enum HostCmd { Add(HostAddArgs), Remove { alias, files }, List { files } }`, `HostAddArgs { alias: String, id: String, relay: Option<String>, address: Vec<HostPort>, parallel: Option<u32>, reconnect: Option<String>, dht: Option<bool>, mdns: Option<bool>, files: FilesArgs }`; `--reconnect` keeps the user's string but is validated with `value_parser = validate_duration` (runs `duration::parse`, returns the string); `--dht`/`--mdns` take `yes|no` through `fn parse_yes_no(&str) -> Result<bool, String>`)
- Modify: `crates/irsync/src/main.rs` (`run_host`)
- Test: `crates/irsync/tests/manage_hosts.rs`, `tests/cli.rs`, `tests/commands_e2e.rs`

**Interfaces:**
- Produces, in `manage::hosts`:
  - `#[derive(Default)] pub struct HostFlags { pub relay: Option<Url>, pub address: Option<Vec<HostPort>>, pub parallel: Option<u32>, pub reconnect: Option<String>, pub dht: Option<bool>, pub mdns: Option<bool> }` (`reconnect` is the validated duration text, written verbatim)
  - `pub fn host_add(doc: &mut DocumentMut, alias: &str, id: EndpointId, flags: &HostFlags) -> Result<Option<EndpointId>, ManageError>` (`BadAlias` unless `valid_alias`; returns the previous id when the alias existed with a different one; writes `endpoint_id` plus each `Some` flag)
  - `pub fn host_remove(doc: &mut DocumentMut, alias: &str) -> Result<(), ManageError>` (`NotFound`)
  - `ManageError::BadAlias` displays as `{s:?} is not a valid alias: must not contain ':' or '/' and must not be an EndpointId`
- `run_host list` prints, per alias: `{alias}: {id}` then one indented `key = value` line per other key in the table. `run_host add` prints `{alias}: {id}` or, on a changed id, `{alias}: {old} -> {new}`.

- [ ] **Step 1: Write the failing tests**

`crates/irsync/tests/manage_hosts.rs`:

```rust
use irsync::manage::hosts::{host_add, host_remove, HostFlags};
use irsync::manage::ManageError;
use toml_edit::DocumentMut;

fn id(n: u8) -> iroh::EndpointId { iroh::SecretKey::from_bytes(&[n; 32]).public() }

#[test]
fn host_add_writes_given_keys_updates_id_and_keeps_others() {
    let mut doc = DocumentMut::new();
    let f = HostFlags { parallel: Some(4), reconnect: Some("1h".into()), ..Default::default() };
    assert_eq!(host_add(&mut doc, "home", id(1), &f).unwrap(), None);
    assert_eq!(doc.to_string(), format!("[home]\nendpoint_id = \"{}\"\nparallel = 4\nreconnect = \"1h\"\n", id(1)));
    let f = HostFlags { address: Some(vec!["10.0.0.1:4000".parse().unwrap()]), ..Default::default() };
    assert_eq!(host_add(&mut doc, "home", id(2), &f).unwrap(), Some(id(1)));
    let s = doc.to_string();
    assert!(s.contains(&id(2).to_string()) && s.contains("parallel = 4") && s.contains("address = [\"10.0.0.1:4000\"]"), "{s}");
    assert_eq!(host_add(&mut doc, "home", id(2), &HostFlags::default()).unwrap(), None);
}

#[test]
fn host_add_rejects_bad_alias_and_remove_needs_existing() {
    let mut doc = DocumentMut::new();
    for bad in ["a:b", "a/b", "", &id(1).to_string()] {
        assert!(matches!(host_add(&mut doc, bad, id(1), &HostFlags::default()), Err(ManageError::BadAlias(_))), "{bad:?}");
    }
    assert!(matches!(host_remove(&mut doc, "x"), Err(ManageError::NotFound(_))));
    host_add(&mut doc, "x", id(1), &HostFlags::default()).unwrap();
    host_remove(&mut doc, "x").unwrap();
    assert!(doc.as_table().is_empty());
}
```

`tests/cli.rs`:

```rust
#[test]
fn host_subcommands_parse() {
    let a = parse(&["host", "add", "home", "abc", "--relay", "https://r", "--address", "1.2.3.4:1", "--address", "h:2", "--parallel", "8", "--reconnect", "2h", "--dht", "no", "--mdns", "yes", "--hosts", "/h"]);
    let Some(Command::Host(HostCmd::Add(h))) = a.cmd else { panic!() };
    assert!(h.alias == "home" && h.id == "abc" && h.address.len() == 2 && h.dht == Some(false) && h.mdns == Some(true) && h.files.hosts == Some("/h".into()));
    assert!(Args::try_parse_from(["irsync", "host", "add", "home", "abc", "--dht", "maybe"]).is_err());
    assert!(matches!(parse(&["host", "remove", "home"]).cmd, Some(Command::Host(HostCmd::Remove { ref alias, .. })) if alias == "home"));
    assert!(matches!(parse(&["host", "list"]).cmd, Some(Command::Host(HostCmd::List { .. }))));
}
```

`tests/commands_e2e.rs`:

```rust
#[test]
fn host_add_then_allow_by_alias_then_list() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("p")).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    assert!(irsync(&home, &["host", "add", "laptop", &id, "--parallel", "4"]).status.success());
    assert_eq!(std::fs::read_to_string(home.join(".config/irsync/hosts.toml")).unwrap(), format!("[laptop]\nendpoint_id = \"{id}\"\nparallel = 4\n"));
    assert!(irsync(&home, &["add", &format!("p:{}", home.join("p").display())]).status.success());
    assert!(irsync(&home, &["allow", "p", "laptop"]).status.success());
    assert!(std::fs::read_to_string(home.join(".config/irsync/exports.toml")).unwrap().contains("rw = [\"laptop\"]"));
    let o = irsync(&home, &["host", "list"]);
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(out.starts_with(&format!("laptop: {id}\n")) && out.contains("parallel = 4"), "{out}");
    let o = irsync(&home, &["host", "add", &id, &id]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("alias"));
    assert!(irsync(&home, &["host", "remove", "laptop"]).status.success());
    assert_eq!(std::fs::read_to_string(home.join(".config/irsync/hosts.toml")).unwrap().trim(), "");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p irsync --test manage_hosts --test cli host_subcommands_parse 2>&1 | head`
Expected: compile errors.

- [ ] **Step 3: Implement `manage/hosts.rs`, the `host` subcommand and `run_host`**

- [ ] **Step 4: Run the tests**

Run: `cargo test -p irsync --test manage_hosts --test cli --test commands_e2e`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add -A crates/irsync
git commit -m "feat(cli): irsync host add/remove/list edit hosts.toml

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 9: `id` and `show`, daemon startup warning, live end-to-end

**Files:**
- Modify: `crates/irsync/src/cli/mod.rs` (`Command::Id { daemon: bool, all: bool, files }` with `conflicts_with`; `Command::Show { files }`)
- Modify: `crates/irsync/src/main.rs` (`run_id`, `run_show`)
- Modify: `crates/irsync/tests/isolated_e2e.rs` (replace the `TODO(task 9)` keygen step with `irsync id`)
- Test: `crates/irsync/tests/cli.rs`, `crates/irsync/tests/commands_e2e.rs`, `crates/irsync/tests/live_exports_e2e.rs` (new)

**Interfaces:**
- `run_id`: default prints two lines on stdout (public key line, EndpointId) for the client identity; `--daemon` the same for the daemon identity; `--all` prints `client:`, the two lines, `daemon:`, the two lines. Keys are generated via `load_or_generate` (stderr line `generated identity {path}` when so).
- `run_show`: stdout, in the spec's order; each export block is `{name}: {path}` then indented `read_only`, `warm_index` when true, then `rw:`/`ro:` lines listing entries as `{entry}` for an id, `{alias} ({id})` for a known alias, `{alias} (unknown alias)` otherwise; the top-level grants under a heading `all exports:`.
- Daemon startup with zero exports logs `no exports defined in {exports path}; run irsync add` at `warn`.

- [ ] **Step 1: Write the failing tests**

`tests/cli.rs`:

```rust
#[test]
fn id_and_show_parse() {
    assert!(matches!(parse(&["id"]).cmd, Some(Command::Id { daemon: false, all: false, .. })));
    assert!(matches!(parse(&["id", "--daemon"]).cmd, Some(Command::Id { daemon: true, .. })));
    assert!(matches!(parse(&["id", "-a"]).cmd, Some(Command::Id { all: true, .. })));
    assert!(Args::try_parse_from(["irsync", "id", "--daemon", "--all"]).is_err());
    assert!(matches!(parse(&["show", "--config", "/c"]).cmd, Some(Command::Show { .. })));
}
```

`tests/commands_e2e.rs`:

```rust
#[test]
fn id_generates_and_prints_client_then_daemon_then_both() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let o = irsync(&home, &["id"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains("generated identity"));
    let out = String::from_utf8(o.stdout).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("ssh-ed25519 "));
    let client = irsync::identity::parse_endpoint_id(lines[1]).unwrap();
    assert!(home.join(".config/irsync/id_ed25519").exists());
    let o = irsync(&home, &["id", "--daemon"]);
    let daemon = irsync::identity::parse_endpoint_id(String::from_utf8(o.stdout).unwrap().lines().nth(1).unwrap()).unwrap();
    assert_ne!(client, daemon);
    let o = irsync(&home, &["id", "--all"]);
    let out = String::from_utf8(o.stdout).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!((lines[0], lines[3]), ("client:", "daemon:"));
    assert_eq!(irsync::identity::parse_endpoint_id(lines[2]).unwrap(), client);
    assert_eq!(irsync::identity::parse_endpoint_id(lines[5]).unwrap(), daemon);
    assert!(!String::from_utf8_lossy(&irsync(&home, &["id"]).stderr).contains("generated"));
}

#[test]
fn show_lists_files_identities_exports_and_flags_unknown_aliases() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("p")).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    irsync(&home, &["host", "add", "laptop", &id]);
    irsync(&home, &["add", &format!("p:{}", home.join("p").display())]);
    irsync(&home, &["allow", "p", "laptop"]);
    std::fs::write(home.join(".config/irsync/exports.toml"), format!("[p]\npath = {:?}\nrw = [\"laptop\", \"ghost\"]\n", home.join("p"))).unwrap();
    let o = irsync(&home, &["show"]);
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(out.contains("irsync.toml") && out.contains("hosts.toml") && out.contains("exports.toml"), "{out}");
    assert!(out.contains(&format!("laptop ({id})")) && out.contains("ghost (unknown alias)"), "{out}");
    assert!(out.contains("p: ") && out.contains(home.join("p").to_str().unwrap()));
}
```

`crates/irsync/tests/live_exports_e2e.rs` (reuse the daemon-spawn and stdout-parsing code of `isolated_e2e.rs`; factor it into `tests/common/process.rs` if that is cleaner):

```rust
/// Start a daemon with no exports, then `irsync add` and `irsync allow`
/// with the daemon running; the next sync succeeds without a restart.
#[test]
fn add_and_allow_take_effect_without_restart() {
    // tmp/server/home: HOME for the daemon-side commands and the daemon;
    // tmp/client/home: HOME for the client. Both share no files.
    // 1. server: write tmp/server/home/.config/irsync/irsync.toml with
    //    [daemon] relay_mode="disabled", portmap=false, [daemon.discovery] all false.
    // 2. start `irsync daemon --listen 127.0.0.1:0` with HOME=server; read id and port;
    //    assert stderr (captured to a file) contains "no exports defined".
    // 3. client: `irsync id` -> client id.
    // 4. client sync `-a --address 127.0.0.1:PORT src/ <daemon-id>:data/` -> fails (exit != 0).
    // 5. server: `irsync add data:tmp/server/data`; `irsync allow data <client-id>`.
    // 6. same client command -> exit 0 and tmp/server/data/a.txt == "hello".
    // 7. server: `irsync revoke <client-id>`; same client command -> exit != 0.
    // 8. stop the daemon.
}
```

Write the body following that outline; assert each exit code and file.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p irsync --test cli id_and_show_parse 2>&1 | head`
Expected: no `Id`/`Show` variants.

- [ ] **Step 3: Implement `id`, `show`, the startup warning; switch `isolated_e2e.rs` to `irsync id`**

- [ ] **Step 4: Run the full suite**

Run: `cargo test --workspace 2>&1 | grep -E "^test result|FAILED|panicked"`
Expected: all green, including `live_exports_e2e`.

- [ ] **Step 5: Commit**

```bash
git add -A crates/irsync
git commit -m "feat(cli): irsync id and show; daemon warns when it has no exports; live add/allow e2e

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 10: Documentation

**Files:**
- Modify: `README.md` (Install unchanged; Quick start rewritten around the spec's six-line example; new sections "Files" with the three TOML examples from the spec marked optional, "Managing exports and hosts" listing the commands; Flags table: `-F` → `--config`, add `--hosts`; Subcommands paragraph; Known limitations: drop the authorized_keys and agent bullets, add "daemon settings need a restart; exports and hosts do not"; the loopback note: client and daemon in one account have distinct identities)
- Modify: `docs/daemon.md` (file locations and lookup, `--show-id` as a synonym for `irsync id --daemon`, the per-connection read and fail-closed rule, lazy indexes and the removed-export rule, systemd and launchd units passing `--config /etc/irsync/irsync.toml --hosts /etc/irsync/hosts.toml --exports /etc/irsync/exports.toml`, isolated-operation TOML under `[daemon]`)
- Modify: `docs/superpowers/specs/2026-10-17-irsync-design.md` (one paragraph at the top of "Identity and authentication" and of "Addressing and configuration" pointing to the 2026-10-23 design)
- Modify: `docs/IRSYNC_VS_RSYNC.md`, `crates/stress/README.md` where they describe the old files

- [ ] **Step 1: Rewrite the README sections and `docs/daemon.md` to match the shipped behaviour**

Verify each command line in the docs against the binary: run every `irsync …` example from the README Quick start in a temp `HOME` on loopback (two homes, `--address`, as `live_exports_e2e` does) and confirm they succeed in that order.

- [ ] **Step 2: Check for leftovers**

Run: `grep -rn "authorized_keys\|irsyncd.toml\|IRSYNC_CONFIG\|\.ssh/id_ed25519\|--foreground\|Host \*\|IdentityFile" README.md docs crates --include='*.md' --include='*.rs' --include='*.toml' | grep -v "superpowers/specs\|superpowers/plans" | grep -v "tests/cli.rs"`
Expected: no output.

- [ ] **Step 3: Commit**

```bash
git add README.md docs crates/stress/README.md
git commit -m "docs: zero-config quick start, the three config files, and the management commands

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```
