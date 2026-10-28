# Single Config File Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One `irsync.toml` holding settings, hosts, exports and grants, still reading split `hosts.toml`/`exports.toml` when present, every command editing only the file `--config` names, and `irsync config get|set|unset|list`.

**Architecture:** A merged parser (`config/file.rs`) deserializes the whole file once with spans, hands the `[hosts]`, `[exports]` and `[all]` parts to the existing table validators, and applies the one-home rule against sibling split files; every reader goes through `Config::load`. The nouns keep their `toml_edit` editing but on one document saved by `save_config`, which validates with the merged parser. `manage/config.rs` adds key-path get/set/unset/list over the same document with a small kind table for typing values and defaults.

**Tech Stack:** Rust 2021, clap 4, `toml` 1 (`Spanned`), `toml_edit` 0.25 (`Key::parse` for dotted keys), serde.

**Spec:** `docs/superpowers/specs/2026-10-28-single-config-design.md`

## Global Constraints

- No table changes its name, layout or schema: `[hosts.*]`, `[exports.*]`, `[all]`, `[client]`, `[daemon]`, `[daemon.discovery]`, top-level `identity` and `state_dir`.
- "Each table has exactly one home": a split file present means its tables must not be in `irsync.toml`; errors name both files.
- "irsync edits exactly one file": no `--hosts` or `--exports` flag anywhere; split files are found beside the config file; editing a table a split file owns is refused with the spec's message.
- `config set` validates by key type and the file parser only; a bad value leaves the file untouched.
- `KEY` is parsed with TOML key grammar (quoted segments for dots); `list` prints such segments quoted.
- Fail closed for the daemon on any error in the merged file; one strict loader for every command.
- No backward compatibility is owed to the three-file layout beyond still reading split files.
- Before every commit: `cargo fmt --all` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean.

## Review Focus

- A `[hosts]` table in `irsync.toml` next to an existing, even empty, `hosts.toml` must fail naming both files, not silently pick one. Pinned in Task 1 (`one_home_rule_names_both_files`).
- `config set hosts.home.endpoint_id nope` must refuse and leave the file byte-identical, including when the `[hosts.home]` table did not exist before. Pinned in Task 5 (`set_with_a_bad_value_leaves_the_file_untouched`).
- `config get daemon.listen` on a file that does not set it must print nothing and exit 0, since its default is an empty array; `config get client.parallel` unset must exit 1. Pinned in Task 4 (`get_prints_defaults_and_exits_1_for_unset_optional_keys`).
- An export named with a dot (`www.example.com`) must round-trip through `config list`, `get` and `set`. Pinned in Task 5 (`dotted_names_round_trip_quoted`).
- The daemon must pick up an export added to `irsync.toml` while it runs, exactly as it did from `exports.toml`. Pinned in Task 2 (`exports_file_is_read_on_every_connection`, retargeted to the merged file).

---

### Task 1: The merged parser and the one-home rule

**Files:**
- Create: `crates/irsync/src/config/file.rs`
- Modify: `crates/irsync/src/config/mod.rs` (`pub mod file;`), `crates/irsync/src/config/hosts.rs`, `crates/irsync/src/config/exports.rs`, `crates/irsync/src/config/settings.rs`, `crates/irsync/src/config/paths.rs`
- Test: `crates/irsync/tests/config_files.rs`

**Interfaces:**
- Produces:
  ```rust
  // config/paths.rs
  pub struct Paths { pub config: PathBuf, pub hosts: PathBuf, pub exports: PathBuf }  // hosts/exports = siblings of config
  impl Paths { pub fn resolve(config: Option<&Path>) -> Paths }                       // default: ~/.config/irsync/irsync.toml
  // config/file.rs
  pub struct Config {
      pub settings: Settings,
      pub hosts: Hosts,          // Hosts.path = the file that holds [hosts]
      pub exports: Exports,      // Exports.path = the file that holds [exports]/[all]
      pub split_hosts: Option<PathBuf>,    // hosts.toml when it was read
      pub split_exports: Option<PathBuf>,  // exports.toml when it was read
  }
  impl Config {
      /// Parses the merged text alone (no split files): the shape pass over the seven
      /// top-level keys, then the typed pass; used by `save_config` to validate a document.
      pub fn parse(text: &str, path: &Path) -> Result<Config, ConfigError>;
      /// `parse` of `paths.config` (missing = defaults), then the split files beside it,
      /// then the one-home rule.
      pub fn load(paths: &Paths) -> Result<Config, ConfigError>;
  }
  // config/hosts.rs, exports.rs: the table validators the merged parser calls
  pub(crate) type RawHosts = BTreeMap<String, Spanned<RawHost>>;
  pub(crate) fn hosts_from_raw(raw: RawHosts, text: &str, path: &Path) -> Result<BTreeMap<String, HostEntry>, ConfigError>;
  pub(crate) fn exports_from_raw(all: AllGrants, raw: BTreeMap<String, Spanned<toml::Value>>, text: &str, path: &Path) -> Result<Exports, ConfigError>;
  ```
  `Hosts::parse` and `Exports::parse` keep their signatures and schemas for split files; `Settings::parse(text, path)` becomes `Config::parse(text, path).map(|c| c.settings)`.

- [ ] **Step 1: Write the failing tests** (append to `config_files.rs`)

```rust
use irsync::config::file::Config;
use irsync::config::paths::Paths;

fn merged(dir: &Path, text: &str) -> Result<Config, irsync::config::ConfigError> {
    std::fs::write(dir.join("irsync.toml"), text).unwrap();
    Config::load(&Paths::resolve(Some(&dir.join("irsync.toml"))))
}

#[test]
fn merged_file_holds_every_table() {
    let d = tempfile::tempdir().unwrap();
    let c = merged(d.path(), &format!(
        "state_dir = \"/var/lib/irsync\"\n[client]\nparallel = 4\n[daemon]\nlisten = [\"127.0.0.1:0\"]\n\
         [hosts.home]\nendpoint_id = \"{}\"\n[all]\nro = [\"home\"]\n[exports.photos]\npath = \"/srv/photos\"\nrw = [\"home\"]\n", id(1)))
    .unwrap();
    assert_eq!(c.settings.state_dir, PathBuf::from("/var/lib/irsync"));
    assert_eq!(c.settings.client.parallel, Some(4));
    assert_eq!(c.hosts.get("home").unwrap().endpoint_id, id(1));
    assert_eq!(c.exports.ro, ["home"]);
    assert_eq!(c.exports.exports["photos"].rw, ["home"]);
    assert_eq!((c.hosts.path.clone(), c.exports.path.clone()), (d.path().join("irsync.toml"), d.path().join("irsync.toml")));
    assert!(c.split_hosts.is_none() && c.split_exports.is_none());
}

#[test]
fn merged_shape_errors_keep_the_old_layout_hints() {
    let d = tempfile::tempdir().unwrap();
    for (text, needle) in [
        ("[home]\nendpoint_id = \"x\"\n", "unknown table \"home\"; hosts live under [hosts.home]"),
        ("[photos]\npath = \"/x\"\n", "unknown table \"photos\"; exports live under [exports.photos]"),
        ("rw = [\"a\"]\n", "unknown key \"rw\"; grants for every export live under [all]"),
        ("[daemon]\nnope = 1\n", "nope"),
        ("[exports.all]\npath = \"/x\"\n", "\"all\" is reserved"),
        ("[hosts.all]\nendpoint_id = \"x\"\n", "invalid host alias \"all\""),
    ] {
        let e = merged(d.path(), text).unwrap_err().to_string();
        assert!(e.contains(needle) && e.contains("irsync.toml"), "{text:?}: {e}");
    }
}

#[test]
fn split_files_beside_the_config_are_read() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("hosts.toml"), format!("[hosts.home]\nendpoint_id = \"{}\"\n", id(1))).unwrap();
    std::fs::write(d.path().join("exports.toml"), "[exports.p]\npath = \"/p\"\n").unwrap();
    let c = merged(d.path(), "[client]\nparallel = 2\n").unwrap();
    assert_eq!(c.hosts.get("home").unwrap().endpoint_id, id(1));
    assert!(c.exports.exports.contains_key("p"));
    assert_eq!(c.hosts.path, d.path().join("hosts.toml"));
    assert_eq!(c.exports.path, d.path().join("exports.toml"));
    assert_eq!(c.split_hosts, Some(d.path().join("hosts.toml")));
    // A missing config file with split files present still works.
    std::fs::remove_file(d.path().join("irsync.toml")).unwrap();
    let c = Config::load(&Paths::resolve(Some(&d.path().join("irsync.toml")))).unwrap();
    assert!(c.hosts.get("home").is_some());
}

#[test]
fn one_home_rule_names_both_files() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("hosts.toml"), "").unwrap();
    let e = merged(d.path(), "[hosts]\n").unwrap_err().to_string();
    assert!(e.contains("hosts are in") && e.contains("hosts.toml") && e.contains("remove [hosts] from") && e.contains("irsync.toml"), "{e}");
    std::fs::remove_file(d.path().join("hosts.toml")).unwrap();
    std::fs::write(d.path().join("exports.toml"), "").unwrap();
    for text in ["[all]\nrw = []\n", "[exports.p]\npath = \"/p\"\n"] {
        let e = merged(d.path(), text).unwrap_err().to_string();
        assert!(e.contains("exports.toml") && e.contains("irsync.toml"), "{text:?}: {e}");
    }
}

#[test]
fn a_split_file_accepts_only_its_own_tables() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("hosts.toml"), "[client]\nparallel = 1\n").unwrap();
    let e = merged(d.path(), "").unwrap_err().to_string();
    assert!(e.contains("hosts.toml") && e.contains("unknown table \"client\""), "{e}");
}

#[test]
fn missing_everything_is_the_defaults() {
    let d = tempfile::tempdir().unwrap();
    let c = Config::load(&Paths::resolve(Some(&d.path().join("irsync.toml")))).unwrap();
    assert_eq!(c.settings, irsync::config::settings::Settings::default());
    assert!(c.hosts.entries.is_empty() && c.exports.exports.is_empty());
}
```

Also change the existing `Paths::resolve(..)` three-argument calls in this file (if any) to the one-argument form.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p irsync --test config_files`
Expected: compile error, `unresolved import irsync::config::file`.

- [ ] **Step 3: Implement**

`file.rs`: a `RawMerged` with `#[serde(default, deny_unknown_fields)]` and fields `identity: Option<PathBuf>`, `state_dir: Option<PathBuf>`, `client: ClientSettings`, `daemon: DaemonSettings`, `hosts: RawHosts`, `exports: BTreeMap<String, Spanned<toml::Value>>`, `all: AllGrants` (make `RawHost`, `AllGrants` `pub(crate)`). `Config::parse`: the shape pass over `BTreeMap<String, Spanned<toml::Value>>` in file order with the hint messages above (`hosts`, `exports`, `all`, `client`, `daemon` must be tables; `identity`, `state_dir` strings; anything else: table → the hosts hint, `rw`/`ro` → the grants hint, else `unknown key`), then `RawMerged` via `toml_err`, then `Settings` from the four settings fields with `expand_tilde` as today, `hosts_from_raw`, `exports_from_raw`. `Config::load`: `read_optional(paths.config)`, parse (defaults when missing), then for each sibling that exists: parse with `Hosts::parse`/`Exports::parse`; if the merged shape had the table, `ConfigError::File { path: config, line: Some(line of the table), message: format!("hosts are in {}; remove [hosts] from {}, or delete {0}", ...) }` (for exports: "exports and grants are in ..."). `Hosts.path`/`Exports.path` = the owning file. `Settings::parse` delegates to `Config::parse`. `paths.rs`: `resolve(config: Option<&Path>)` with siblings `config.parent().join("hosts.toml")` and `"exports.toml"`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p irsync --test config_files`
Expected: all pass (the earlier settings/hosts/exports tests included).

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/config crates/irsync/tests/config_files.rs
git commit -m "feat(config): one merged irsync.toml parser; split hosts.toml/exports.toml still read beside it; one-home rule"
```

---

### Task 2: One loader for every reader; the flags go away

**Files:**
- Modify: `crates/irsync/src/cli/mod.rs` (`FilesArgs { config }`; `DaemonArgs` loses `hosts`, `exports`; `SyncArgs`/`SyncOptions` lose `hosts`), `crates/irsync/src/main.rs` (`file_paths`, `connect_for`, `run_daemon`, `run_show`, `run_id`, every `Hosts::load`/`Exports::load`/`Settings::load`), `crates/irsync/src/authz.rs` (`FileAuthorizer::new(paths: Paths)` re-reading `Config::load`), `crates/irsync/src/daemon.rs` (`run_from_settings` warning names `paths.config`), `crates/irsync/src/manage/show.rs` (`report` takes `&Config`; prints `hosts: PATH (also read)` / `exports: PATH (also read)` lines after the config line when split files were read), `crates/stress/src/daemon.rs` (drop the two flags), `README.md` flag rows only if they break the build's doc tests (they do not; docs are Task 6)
- Test: `crates/irsync/tests/cli.rs`, `commands_e2e.rs`, `identity.rs`, `isolated_e2e.rs`, `daemon_auth.rs`, `authz_snapshot.rs`

**Interfaces:**
- Consumes: Task 1's `Config`, `Paths::resolve(Option<&Path>)`.
- Produces: `fn file_paths(f: &FilesArgs) -> Paths`; `FileAuthorizer::new(paths: Paths)`; `show::report(paths: &Paths, config: &Config, host_lines: &[String], client, daemon) -> String`.
- In this task the nouns still edit `paths.hosts` / `paths.exports` (the split files, which are read); Task 3 moves them.

- [ ] **Step 1: Write the failing tests**

`cli.rs`: in `daemon_flags`, `file_flags_are_accepted_after_the_noun_in_either_position`, `export_update_takes_the_noun_file_flags_in_either_position` and the test at line ~313, drop every `--hosts`/`--exports` and assert they are refused:

```rust
#[test]
fn hosts_and_exports_flags_are_gone() {
    for args in [
        &["daemon", "--hosts", "h"][..], &["daemon", "--exports", "e"], &["host", "list", "--hosts", "h"],
        &["grant", "list", "--exports", "e"], &["show", "--hosts", "h"], &["-a", "--hosts", "h", "a", "b:c"],
    ] {
        assert!(Args::try_parse_from([&["irsync"][..], args].concat()).is_err(), "{args:?}");
    }
}
```

`commands_e2e.rs`: `file_flags_after_the_noun_edit_the_named_files` becomes `config_flag_after_the_noun_names_the_file`: `irsync export add p:DIR --config C` and `irsync export --config C add ...` both write `[exports.p]` into the siblings of `C` for now (this task) and into `C` itself after Task 3; write the assertion as "`C` or its sibling `exports.toml` contains `[exports.p]`" so it holds across both tasks. A new test:

```rust
#[test]
fn show_names_split_files_it_read() {
    // home/.config/irsync/hosts.toml with [hosts.laptop]; no irsync.toml.
    // `show` prints "hosts: <path>/hosts.toml (also read)" and lists laptop; without the
    // split file the hosts line is "hosts: <config>" only.
}
```

`daemon_auth.rs` `exports_file_is_read_on_every_connection` and `index_open_failure_is_an_internal_error_reply`: `FileAuthorizer::new(Paths::resolve(Some(&d.path().join("irsync.toml"))))` and write the `[exports.data]` tables into `irsync.toml`. `authz_snapshot.rs` likewise. `identity.rs` and `isolated_e2e.rs`: drop the `--hosts`/`--exports` arguments (their files, when written, sit beside the config file already; if not, move them beside it).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p irsync --test cli hosts_and_exports_flags_are_gone`
Expected: FAIL (the flags still parse).

- [ ] **Step 3: Implement**

Remove the fields and flags; `file_paths` = `Paths::resolve(f.config.as_deref())`; in `main.rs` replace each `Settings::load`/`Hosts::load`/`Exports::load` triple with one `Config::load(&paths)?` and use its parts (`connect_for` takes `opts.config` only; `run_daemon` passes `Paths` on; `run_show` passes `&config` and `host_lines` from `manage::load_doc(&config.hosts.path)`). `FileAuthorizer` stores `Paths` and snapshots `Config::load`. `run_from_settings` warns "no exports defined in {config}; run irsync export add". `report`: after `file_line("config", ..)` and the client identity, `hosts: {paths.config}` or, when `config.split_hosts` is `Some(p)`, `hosts: {p} (also read)`; same for exports. Stress harness: drop the two `.arg`s (its `exports.toml` beside `irsync.toml` is still read).

- [ ] **Step 4: Run the whole suite**

Run: `cargo test --workspace`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates
git commit -m "feat(config): every command reads one Config; --hosts and --exports are gone; show names split files it read"
```

---

### Task 3: The nouns edit `irsync.toml`; a split file's table is refused

**Files:**
- Modify: `crates/irsync/src/manage/mod.rs` (`save_config`, `Table`, `ensure_editable`; delete `save_hosts`, `save_exports`), `crates/irsync/src/main.rs` (`run_host`, `run_export`, `run_grant`, `host_add`)
- Test: `crates/irsync/tests/manage_doc.rs`, `commands_e2e.rs`

**Interfaces:**
- Consumes: Task 1's `Config::parse`.
- Produces:
  ```rust
  pub enum Table { Hosts, Exports }   // Exports covers [exports] and [all]
  /// Err(ManageError::SplitFile { table, file }) when the sibling split file exists.
  pub fn ensure_editable(paths: &Paths, table: Table) -> Result<(), ManageError>;
  /// Validates `doc` with `Config::parse` (not `load`: the split-file rule was checked by `ensure_editable`), then `save_doc`.
  pub fn save_config(path: &Path, doc: &DocumentMut) -> Result<(), ManageError>;
  ```
  `ManageError::SplitFile { table: &'static str, file: PathBuf }` displays as `{table} are kept in {file}; irsync edits irsync.toml only: move the [{table}] table into irsync.toml, or edit {file} by hand` (table word `hosts` or `exports`).

- [ ] **Step 1: Write the failing tests**

`manage_doc.rs`:

```rust
#[test]
fn save_config_refuses_a_document_that_does_not_load() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("irsync.toml");
    let doc: DocumentMut = "[hosts.x]\nendpoint_id = \"nope\"\n".parse().unwrap();
    let e = irsync::manage::save_config(&path, &doc).unwrap_err();
    assert!(e.to_string().contains("refusing to save"), "{e}");
    assert!(!path.exists());
}

#[test]
fn ensure_editable_refuses_a_table_a_split_file_owns() {
    let d = tempfile::tempdir().unwrap();
    let paths = Paths::resolve(Some(&d.path().join("irsync.toml")));
    assert!(ensure_editable(&paths, Table::Hosts).is_ok());
    std::fs::write(&paths.hosts, "").unwrap();
    let e = ensure_editable(&paths, Table::Hosts).unwrap_err().to_string();
    assert!(e.contains("hosts are kept in") && e.contains("hosts.toml") && e.contains("edit") && e.contains("by hand"), "{e}");
    assert!(ensure_editable(&paths, Table::Exports).is_ok());
}
```

`commands_e2e.rs`: retarget every assertion that reads `.config/irsync/hosts.toml` or `exports.toml` after a noun to `.config/irsync/irsync.toml` (the round-trip test's expected text becomes `[exports.photos]\npath = ...\n` inside `irsync.toml`; `host add` then `export add` produce both tables in the one file). Add:

```rust
#[test]
fn nouns_refuse_a_table_kept_in_a_split_file() {
    // home/.config/irsync/hosts.toml exists (empty). `host add laptop ID` exits 1 with
    // "hosts are kept in" and "hosts.toml"; irsync.toml is not created. `export add p:DIR`
    // still works (exports are not split) and writes [exports.p] to irsync.toml.
    // Then with exports.toml present: `grant add p ID` and `export remove p` are refused.
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p irsync --test manage_doc`
Expected: FAIL to compile (`save_config`, `ensure_editable` missing).

- [ ] **Step 3: Implement**

`run_host`/`run_export`/`run_grant`: `ensure_editable(&paths, Table::..)?` first, then `load_doc(&paths.config)`, edit as today, `save_config(&paths.config, &doc)`; messages name `paths.config`. `host list` and `export list`/`grant list` read `Config::load` (no refusal: reading is fine). `show`'s `host_lines` come from the owning file's document (`config.hosts.path`).

- [ ] **Step 4: Run the suite**

Run: `cargo test --workspace`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates
git commit -m "feat(manage): host, export and grant edit irsync.toml only; a table kept in a split file is refused"
```

---

### Task 4: `irsync config get` and `config list`

**Files:**
- Create: `crates/irsync/src/manage/config.rs`
- Modify: `crates/irsync/src/manage/mod.rs` (`pub mod config;`), `crates/irsync/src/config/duration.rs` (`pub fn format(d: Duration) -> String`), `crates/irsync/src/cli/mod.rs` (`Command::Config(ConfigArgs)`, `ConfigCmd::{Get { key }, Set { key, values: Vec<String> }, Unset { key }, List}`), `crates/irsync/src/main.rs` (`run_config`, Get and List arms; Set/Unset arms `bail!("not implemented yet")` until Task 5)
- Test: `crates/irsync/tests/manage_config.rs` (new), `crates/irsync/tests/cli.rs`, `crates/irsync/tests/commands_e2e.rs`

**Interfaces:**
- Produces:
  ```rust
  // manage/config.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum Kind { Bool, Int, Duration, Str, Path, Url, Id, StrArray, AddrArray, Table }
  /// The kind of a key path, or None for an unknown key. Known: identity, state_dir (Path);
  /// client.{parallel: Int, reconnect: Duration, dht: Bool, mdns: Bool, relay: Url};
  /// daemon.{identity: Path, idle_timeout: Duration, dormant_session_timeout: Duration,
  /// listen: AddrArray, portmap: Bool, relay: Url, relay_mode: Str}; daemon.discovery.{dht,dns,mdns: Bool};
  /// hosts.X.{endpoint_id: Id, relay: Url, address: AddrArray, parallel: Int, reconnect: Duration, dht: Bool, mdns: Bool};
  /// exports.X.{path: Path, read_only: Bool, warm_index: Bool, rw: StrArray, ro: StrArray}; all.{rw,ro: StrArray};
  /// and the tables client, daemon, daemon.discovery, hosts, hosts.X, exports, exports.X, all.
  pub fn kind_of(path: &[String]) -> Option<Kind>;
  /// Parses KEY with TOML key grammar: `exports."www.example.com".path` -> ["exports","www.example.com","path"].
  pub fn parse_key(key: &str) -> Result<Vec<String>, ManageError>;    // ManageError::BadKey(String)
  /// The default of a settings key, formatted as `get` prints it; None when the key has no default (or is not a settings key).
  pub fn default_of(path: &[String]) -> Option<Vec<String>>;          // one element per output line
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum Got { Lines(Vec<String>), Unset }
  /// `get`: lines as the spec prints them (bare scalar; one array element per line; `k = v` per table key, nested keys dotted).
  pub fn get(doc: &DocumentMut, path: &[String]) -> Result<Got, ManageError>;   // Err for an unknown key
  /// `list`: every set key as `dotted.key = <toml value>`, file order, quoted segments where needed.
  pub fn list(doc: &DocumentMut) -> Vec<String>;
  ```
  `run_config` Get: `get` on the owning document (hosts/exports keys read the split file's document when `config.split_*` is set), then `default_of` when `Unset`; prints lines, exits 1 when nothing. List: the config document's lines, then each split file's.

- [ ] **Step 1: Write the failing unit tests** (`manage_config.rs`)

```rust
use irsync::manage::config::{default_of, get, kind_of, list, parse_key, Got, Kind};
use toml_edit::DocumentMut;

fn doc(s: &str) -> DocumentMut { s.parse().unwrap() }
fn p(s: &str) -> Vec<String> { parse_key(s).unwrap() }
fn id() -> String { iroh::SecretKey::from_bytes(&[5; 32]).public().to_string() }

#[test]
fn keys_parse_with_toml_grammar() {
    assert_eq!(p("daemon.listen"), ["daemon", "listen"]);
    assert_eq!(p("exports.\"www.example.com\".path"), ["exports", "www.example.com", "path"]);
    assert!(parse_key("a..b").is_err() && parse_key("").is_err());
}

#[test]
fn kinds_of_known_keys() {
    assert!(matches!(kind_of(&p("daemon.listen")), Some(Kind::AddrArray)));
    assert!(matches!(kind_of(&p("hosts.home.reconnect")), Some(Kind::Duration)));
    assert!(matches!(kind_of(&p("exports.photos.read_only")), Some(Kind::Bool)));
    assert!(matches!(kind_of(&p("hosts.home")), Some(Kind::Table)));
    assert!(kind_of(&p("daemon.nope")).is_none() && kind_of(&p("nope")).is_none());
}

#[test]
fn get_prints_scalars_arrays_and_tables() {
    let d = doc("[client]\nparallel = 4\n[daemon]\nlisten = [\"0.0.0.0:0\", \"[::]:0\"]\n[hosts.home]\nendpoint_id = \"abc\"\nrelay = \"https://r/\"\n[daemon.discovery]\nmdns = false\n");
    assert_eq!(get(&d, &p("client.parallel")).unwrap(), Got::Lines(vec!["4".into()]));
    assert_eq!(get(&d, &p("daemon.listen")).unwrap(), Got::Lines(vec!["0.0.0.0:0".into(), "[::]:0".into()]));
    assert_eq!(get(&d, &p("hosts.home")).unwrap(), Got::Lines(vec!["endpoint_id = \"abc\"".into(), "relay = \"https://r/\"".into()]));
    assert_eq!(get(&d, &p("daemon")).unwrap(), Got::Lines(vec!["listen = [\"0.0.0.0:0\", \"[::]:0\"]".into(), "discovery.mdns = false".into()]));
    assert_eq!(get(&d, &p("client.reconnect")).unwrap(), Got::Unset);
    assert_eq!(get(&d, &p("hosts.nope")).unwrap(), Got::Unset);
    assert!(get(&d, &p("daemon.nope")).is_err());
}

#[test]
fn defaults_for_settings_keys() {
    assert_eq!(default_of(&p("daemon.idle_timeout")), Some(vec!["30m".into()]));
    assert_eq!(default_of(&p("daemon.dormant_session_timeout")), Some(vec!["7d".into()]));
    assert_eq!(default_of(&p("daemon.portmap")), Some(vec!["true".into()]));
    assert_eq!(default_of(&p("daemon.listen")), Some(vec![]));
    assert_eq!(default_of(&p("daemon.relay_mode")), Some(vec!["default".into()]));
    assert_eq!(default_of(&p("identity")), Some(vec![irsync::config::settings::Settings::default().identity.display().to_string()]));
    assert_eq!(default_of(&p("client.parallel")), None);
    assert_eq!(default_of(&p("hosts.home.relay")), None);
}

#[test]
fn list_is_dotted_keys_in_file_order_with_quoting() {
    let d = doc("identity = \"/k\"\n[daemon]\nlisten = [\"a:1\"]\n[daemon.discovery]\ndht = false\n[exports.\"www.example.com\"]\npath = \"/w\"\n");
    assert_eq!(list(&d), vec![
        "identity = \"/k\"", "daemon.listen = [\"a:1\"]", "daemon.discovery.dht = false",
        "exports.\"www.example.com\".path = \"/w\"",
    ]);
}
```

And `duration::format`: `assert_eq!(format(Duration::from_secs(1800)), "30m"); format(7*86400) == "7d"; format(90) == "90s"; format(0) == "0s"`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p irsync --test manage_config`
Expected: compile error, `irsync::manage::config` missing.

- [ ] **Step 3: Write the failing CLI and end-to-end tests**

`cli.rs`:
```rust
#[test]
fn config_noun_parses() {
    use irsync::cli::ConfigCmd;
    assert!(matches!(parse(&["config", "get", "daemon.listen"]).cmd, Some(Command::Config(ref c)) if matches!(c.cmd, ConfigCmd::Get { ref key } if key == "daemon.listen")));
    assert!(matches!(parse(&["config", "set", "daemon.listen", "a:1", "b:2"]).cmd, Some(Command::Config(ref c)) if matches!(c.cmd, ConfigCmd::Set { ref values, .. } if values == &["a:1", "b:2"])));
    assert!(matches!(parse(&["config", "set", "all.rw"]).cmd, Some(Command::Config(ref c)) if matches!(c.cmd, ConfigCmd::Set { ref values, .. } if values.is_empty())));
    assert!(matches!(parse(&["config", "unset", "x"]).cmd, Some(Command::Config(_))));
    assert!(matches!(parse(&["config", "--config", "/c", "list"]).cmd, Some(Command::Config(ref c)) if c.files.config == Some("/c".into())));
    assert!(Args::try_parse_from(["irsync", "config"]).is_err());
    assert!(Args::try_parse_from(["irsync", "config", "get"]).is_err());
}
```

`commands_e2e.rs`:
```rust
#[test]
fn get_prints_defaults_and_exits_1_for_unset_optional_keys() {
    // empty home: `config get daemon.idle_timeout` -> "30m\n" exit 0; `config get daemon.listen` -> "" exit 0;
    // `config get client.parallel` -> "" exit 1; `config get daemon.nope` -> exit 1, stderr "unknown key";
    // after `host add laptop ID`: `config get hosts.laptop.endpoint_id` -> ID; `config get hosts.laptop` -> "endpoint_id = \"ID\"\n".
}

#[test]
fn list_covers_the_config_and_split_files() {
    // irsync.toml: [client] parallel = 3; hosts.toml: [hosts.laptop] endpoint_id = ID.
    // `config list` == "client.parallel = 3\nhosts.laptop.endpoint_id = \"ID\"\n".
}
```

- [ ] **Step 4: Implement**

`parse_key` via `toml_edit::Key::parse`. `kind_of` as a match over the path shape. `get`: walk the document (`as_table_like`), scalars via `Value` with strings unquoted, arrays per element, tables via a recursive dotted walk in document order; unknown key (per `kind_of`) → `ManageError::UnknownKey(String)` (`unknown key {0:?}`). `default_of` from `Settings::default()` with `duration::format` (largest of d/h/m/s that divides evenly). `list`: the same walk from the root. `run_config`: Get prints `Lines` joined by newlines, or `default_of`, else exit 1 silently; List prints the config document's lines then each split document's; Set/Unset `bail!("not implemented yet")`.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p irsync --test manage_config --test cli --test commands_e2e --lib duration`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates
git commit -m "feat(cli): irsync config get and config list over the dotted key namespace, with settings defaults"
```

---

### Task 5: `config set` and `config unset`

**Files:**
- Modify: `crates/irsync/src/manage/config.rs` (`set`, `unset`, `parse_value`), `crates/irsync/src/main.rs` (Set/Unset arms)
- Test: `crates/irsync/tests/manage_config.rs`, `crates/irsync/tests/commands_e2e.rs`

**Interfaces:**
- Consumes: Task 3's `ensure_editable`/`save_config`, Task 4's `kind_of`/`parse_key`.
- Produces:
  ```rust
  /// VALUE... in the key's kind: Bool `true`/`false`; Int; Duration as the file spells it; Str/Path/Url/Id as a string
  /// (Id checked with parse_endpoint_id); arrays one element per VALUE (zero values = empty array). A scalar kind
  /// with zero or several values, or a Table kind, is ManageError::BadValue { key, message }.
  pub fn parse_value(kind: Kind, values: &[String]) -> Result<toml_edit::Value, ManageError>;
  /// Sets the key, creating tables on demand (dotted headers, as the nouns write); Table kind is an error.
  pub fn set(doc: &mut DocumentMut, path: &[String], value: toml_edit::Value) -> Result<(), ManageError>;
  /// Removes the key or the whole table; ManageError::NothingToRemove when absent.
  pub fn unset(doc: &mut DocumentMut, path: &[String]) -> Result<(), ManageError>;
  ```
  `run_config` Set: `ensure_editable` for `hosts.*` / `exports.*` / `all.*` keys, `load_doc(config)`, `parse_value`, `set`, `save_config` (which refuses an invalid result naming the key: wrap `ManageError::Invalid` as `config set {key}: {inner}`). Unset likewise.

- [ ] **Step 1: Write the failing unit tests**

```rust
#[test]
fn values_are_typed_by_kind() {
    use irsync::manage::config::parse_value;
    assert_eq!(parse_value(Kind::Bool, &["true".into()]).unwrap().to_string().trim(), "true");
    assert_eq!(parse_value(Kind::Int, &["8".into()]).unwrap().to_string().trim(), "8");
    assert_eq!(parse_value(Kind::Duration, &["30m".into()]).unwrap().to_string().trim(), "\"30m\"");
    assert_eq!(parse_value(Kind::AddrArray, &["a:1".into(), "b:2".into()]).unwrap().to_string().trim(), "[\"a:1\", \"b:2\"]");
    assert_eq!(parse_value(Kind::StrArray, &[]).unwrap().to_string().trim(), "[]");
    for (kind, vals) in [(Kind::Bool, vec!["yes".to_string()]), (Kind::Int, vec!["x".into()]), (Kind::Duration, vec!["5".into()]),
                         (Kind::Bool, vec![]), (Kind::Bool, vec!["true".into(), "false".into()]), (Kind::Id, vec!["nope".into()])] {
        assert!(parse_value(kind, &vals).is_err(), "{kind:?} {vals:?}");
    }
    assert!(parse_value(Kind::Table, &["x".into()]).is_err());
}

#[test]
fn set_and_unset_edit_the_document_and_keep_the_rest() {
    let mut d = doc("# keep me\n[client]\nparallel = 4   # inline\n");
    set(&mut d, &p("client.parallel"), parse_value(Kind::Int, &["8".into()]).unwrap()).unwrap();
    set(&mut d, &p("hosts.home.endpoint_id"), parse_value(Kind::Id, &[id()]).unwrap()).unwrap();
    set(&mut d, &p("daemon.discovery.mdns"), parse_value(Kind::Bool, &["false".into()]).unwrap()).unwrap();
    assert_eq!(d.to_string(), format!("# keep me\n[client]\nparallel = 8   # inline\n\n[hosts.home]\nendpoint_id = \"{}\"\n\n[daemon.discovery]\nmdns = false\n", id()));
    unset(&mut d, &p("client.parallel")).unwrap();
    unset(&mut d, &p("hosts.home")).unwrap();
    assert!(!d.to_string().contains("parallel") && !d.to_string().contains("hosts"));
    assert!(matches!(unset(&mut d, &p("client.parallel")), Err(ManageError::NothingToRemove(_))));
    assert!(set(&mut d, &p("hosts.home"), toml_edit::Value::from(1)).is_err());
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p irsync --test manage_config`
Expected: compile error, `set`/`unset`/`parse_value` missing.

- [ ] **Step 3: Write the failing end-to-end tests** (`commands_e2e.rs`)

```rust
#[test]
fn set_with_a_bad_value_leaves_the_file_untouched() {
    // irsync.toml = "[client]\nparallel = 4\n". Each of: `config set hosts.home.endpoint_id nope`,
    // `config set exports.p.path ./x`, `config set client.parallel x`, `config set daemon.nope 1`,
    // `config set hosts.all.endpoint_id ID`, `config set hosts.home 1` exits 1 with the key named on stderr,
    // and the file bytes are unchanged afterwards.
}

#[test]
fn set_unset_and_the_nouns_share_one_file() {
    // `config set exports.p.path /tmp` then `config set exports.p.rw ID`: `grant list p` shows ID;
    // `config set all.rw` (no value) writes `rw = []`; `config unset exports.p` then `export list` is empty;
    // `config set daemon.listen 127.0.0.1:1 '[::1]:1'` then `config get daemon.listen` prints the two lines;
    // `config set` with hosts.toml present for a hosts.* key is refused with "hosts are kept in".
}

#[test]
fn dotted_names_round_trip_quoted() {
    // `config set exports."www.example.com".path /w`; `config list` contains `exports."www.example.com".path = "/w"`;
    // `config get exports."www.example.com".path` prints "/w"; `export list` shows www.example.com.
}
```

- [ ] **Step 4: Implement**

`parse_value`: by `Kind`, `toml_edit::Value` from the parsed scalar; `Id` via `parse_endpoint_id` and stored as its string; arrays as `Array` of strings. `set`: walk/create tables with `implicit(true)` on intermediate `hosts`/`exports` tables (as the nouns do) and a real table for the leaf's parent; `unset`: remove the item, then remove empty implicit parents only if they were implicit. `run_config`: as in Interfaces; a `save_config` refusal is reported as `irsync config set: {key}: {parser message}`.

- [ ] **Step 5: Run the suite, fmt and clippy**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`
Expected: all clean and green.

- [ ] **Step 6: Commit**

```bash
git add crates
git commit -m "feat(cli): irsync config set and unset, typed by key, validated by the file parser"
```

---

### Task 6: Docs

**Files:**
- Modify: `README.md` (Files section: one file, one TOML example merged from the three, the split-file paragraph, the `config` rows in the command table, the flags table row for `--config`, the "nouns accept --config" sentence), `docs/daemon.md` (the system-wide recipe without `$F`: `--config /etc/irsync/irsync.toml` only; a paragraph on keeping `hosts.toml`/`exports.toml` beside it by hand; the "read on every connection" note names `irsync.toml`), notes in `2026-10-23-zero-config-cli-design.md` (Files section) and `2026-10-28-config-cli-design.md` (Files section) pointing at this spec
- Test: none beyond `cargo test --workspace` (no doc tests touch these)

- [ ] **Step 1: Write the docs**

README command table rows: `irsync config get KEY`, `irsync config set KEY VALUE...`, `irsync config unset KEY`, `irsync config list`, with one quoted-key example and the sentence "`config` writes what you say, the nouns know what you mean".

- [ ] **Step 2: Run the suite**

Run: `cargo test --workspace`
Expected: green.

- [ ] **Step 3: Commit**

```bash
git add README.md docs
git commit -m "docs: one irsync.toml, split files read beside it, and the config noun"
```
