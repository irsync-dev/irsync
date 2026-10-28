# Config CLI Grammar Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the bare `add`/`remove`/`allow`/`revoke` commands with `host`, `export` and `grant` nouns sharing `add`/`remove`/`list`, and move the two files they edit to `[hosts.*]`, `[all]` and `[exports.*]` tables.

**Architecture:** The file layouts change first, behind the existing command names, so every later task runs against the final files. Then the clap tree is rebuilt around three noun structs that carry the file flags as global arguments, and `main.rs` dispatches to the existing `manage` writers. `export list` and `grant list` are reporters built on the `show` report's helpers so a grant line is spelled one way everywhere.

**Tech Stack:** Rust, clap 4 derive, `toml` 1 (parsing, with `Spanned` for line numbers), `toml_edit` 0.25 (format-preserving writes), integration tests under `crates/irsync/tests/` that run the built binary with `HOME` pointed at a temp dir.

**Spec:** `docs/superpowers/specs/2026-10-28-config-cli-design.md`

## Global Constraints

- No new dependency; clap, `toml`, `toml_edit`, `ssh-key`, `dirs` only.
- Exit codes unchanged: a management command exits 0 on success, 1 on any error.
- Writes stay atomic (temp file in the same directory, then rename), a missing file is created with its directory, and a document that would not load is never saved (`manage::save_exports` / `save_hosts` keep parsing before writing).
- Comments in both files survive every edit (`toml_edit`); the existing injection tests in `tests/manage_exports.rs` keep passing against the moved code.
- The one reserved export name is `all`; `rw` and `ro` stop being reserved. The message, from `export add` and from the parser alike, is exactly: `"all" is reserved: it is the word `grant add all WHO` uses for every export` (the name printed with `{:?}`).
- Every error about a file names the file; parse errors carry the line.
- Old spellings are removed outright: no alias for `add`, `remove`, `allow`, `revoke`, and a top-level `[photos]` or `[home]` table is an error, not an empty file.
- Before each commit: `cargo fmt --all` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` are clean.

## Review Focus

1. An old-layout `exports.toml` (top-level `[photos]` with `path`) must fail with `unknown table "photos"; exports live under [exports.photos]` and the line, never parse as empty. Pinned in Task 1.
2. `export remove` of the last export leaves an empty `[exports]` table or none; the file must still load and `show` must still run. Pinned in Task 1.
3. `irsync host --hosts H list` and `irsync host list --hosts H` must both set the hosts path; `irsync --hosts H host list` must be a usage error. Pinned in Task 3.
4. `grant list ghost`, where `ghost` is neither an export, an alias nor an id, must fail naming all three possibilities and the hosts file, exit 1. Pinned in Task 5.
5. `export add /srv/all` (a bare path whose last component is `all`) must be refused with the reserved message, not written. Pinned in Task 1.

---

## File Structure

| File | Responsibility after this plan |
| --- | --- |
| `crates/irsync/src/config/exports.rs` | parse `[all]` and `[exports.*]`; `RESERVED`, `reserved()` |
| `crates/irsync/src/config/hosts.rs` | parse `[hosts.*]`; `valid_alias` |
| `crates/irsync/src/manage/exports.rs` | `add_export`, `remove_export`, `parse_add_target` under `doc["exports"]` |
| `crates/irsync/src/manage/grants.rs` (new) | `grant_add`, `grant_remove` (moved from `exports.rs`), `list_all`, `list_export`, `list_peer` |
| `crates/irsync/src/manage/hosts.rs` | `host_add`, `host_remove`, `host_list` under `doc["hosts"]` |
| `crates/irsync/src/manage/show.rs` | `report` composed from public `export_blocks`, `all_block`, `grant_lines` |
| `crates/irsync/src/manage/mod.rs` | `pub mod grants`; `ManageError::Reserved` text |
| `crates/irsync/src/cli/mod.rs` | `Command::{Host, Export, Grant}` with `HostArgs`, `ExportArgs`, `GrantArgs`; `FilesArgs` global |
| `crates/irsync/src/main.rs` | `run_host`, `run_export`, `run_grant` |
| `crates/irsync/src/session/reconnect.rs` | the `irsync grant add` hint |
| `crates/irsync/src/authz.rs` | comment and log label `all` instead of `*` |
| `crates/stress/src/daemon.rs` | writes `[exports.data]` |
| `crates/irsync/tests/manage_grants.rs` (new) | tests for `manage::grants` (grant tests move here from `manage_exports.rs`) |
| `README.md`, `docs/daemon.md`, `docs/superpowers/specs/2026-10-23-zero-config-cli-design.md` | the new commands and layouts |

---

### Task 1: `exports.toml` under `[all]` and `[exports.*]`

**Files:**
- Modify: `crates/irsync/src/config/exports.rs`
- Modify: `crates/irsync/src/manage/exports.rs`, `crates/irsync/src/manage/mod.rs`
- Create: `crates/irsync/src/manage/grants.rs`
- Modify: `crates/irsync/src/manage/show.rs:63-67` (the `all exports:` label)
- Modify: `crates/irsync/src/main.rs:710-807` (callers of the moved functions)
- Modify: `crates/irsync/src/authz.rs:24-26,70` (`*` wording in a comment and a log field)
- Modify: `crates/stress/src/daemon.rs:77-86`
- Test: `crates/irsync/tests/config_files.rs`, `tests/manage_exports.rs`, new `tests/manage_grants.rs`, fixtures in `tests/authz_snapshot.rs`, `tests/daemon_auth.rs:277,296,323`, `tests/isolated_e2e.rs:114,320`, `tests/commands_e2e.rs:55,66-68,199`

**Interfaces:**
- Consumes: nothing new.
- Produces:
  - `config::exports::RESERVED: [&str; 1] = ["all"]` and `pub fn reserved(name: &str) -> String` (the exact message from Global Constraints).
  - `config::exports::Exports { path, rw, ro, exports }` unchanged in shape; `rw`/`ro` now come from `[all]`.
  - `manage::exports::add_export(doc: &mut DocumentMut, name: &str, path: &Path, read_only: bool, warm_index: bool) -> Result<(), ManageError>` writes `doc["exports"][name]`.
  - `manage::exports::remove_export(doc: &mut DocumentMut, name: &str) -> Result<(), ManageError>`.
  - `manage::grants::grant_add(doc: &mut DocumentMut, export: Option<&str>, who: &PeerSpec, ro: bool, resolve: &dyn Fn(&str) -> Option<EndpointId>) -> Result<(), ManageError>`; `export = None` targets `[all]`.
  - `manage::grants::grant_remove(doc: &mut DocumentMut, export: Option<&str>, who: Option<&PeerSpec>, resolve: &dyn Fn(&str) -> Option<EndpointId>) -> Result<usize, ManageError>`; `export = None` walks `[all]` and every `[exports.*]`; `who = None` empties both arrays of the target. Same semantics as today's `allow`/`revoke`.

- [ ] **Step 1: Write the failing parser tests in `tests/config_files.rs`**

Rewrite every exports fixture to the new layout (`rw = [..]` at the top becomes `[all]\nrw = [..]`, `[photos]` becomes `[exports.photos]`) and replace `exports_reserved_table_name_is_error` and `exports_rw_ro_tables_are_reserved` with:

```rust
#[test]
fn exports_parse_all_table_and_exports_tables() {
    let t = "[all]\nrw = [\"admin\"]\n[exports.photos]\npath = \"/srv/photos\"\nrw = [\"laptop\"]\nro = [\"b2e1\"]\n[exports.docs]\npath = \"/srv/docs\"\nread_only = true\nwarm_index = true\n";
    let x = Exports::parse(t, p()).unwrap();
    assert_eq!(x.rw, vec!["admin"]);
    assert_eq!(x.exports["photos"].rw, vec!["laptop"]);
    assert!(x.exports["docs"].read_only && x.exports["docs"].warm_index);
    assert!(Exports::parse("[all]\n", p()).unwrap().rw.is_empty());
}

#[test]
fn exports_rw_and_ro_are_valid_export_names() {
    for name in ["rw", "ro"] {
        assert!(Exports::parse(&format!("[exports.{name}]\npath = \"/x\"\n"), p()).is_ok());
    }
}

#[test]
fn exports_all_is_the_only_reserved_name() {
    let e = Exports::parse("\n[exports.all]\npath = \"/x\"\n", p()).unwrap_err().to_string();
    assert!(e.contains("\"all\" is reserved: it is the word `grant add all WHO` uses for every export"), "{e}");
    assert!(e.contains(":2:"), "{e}");
}

#[test]
fn exports_old_layout_names_the_new_location() {
    let e = Exports::parse("\n[photos]\npath = \"/x\"\n", p()).unwrap_err().to_string();
    assert!(e.contains("unknown table \"photos\"; exports live under [exports.photos]") && e.contains(":2:"), "{e}");
    let e = Exports::parse("rw = [\"a\"]\n", p()).unwrap_err().to_string();
    assert!(e.contains("unknown key \"rw\"; grants for every export live under [all]") && e.contains(":1:"), "{e}");
    let e = Exports::parse("[all]\npath = \"/x\"\n", p()).unwrap_err().to_string();
    assert!(e.contains("[all]") && e.contains("path"), "{e}");
    let e = Exports::parse("x = 1\n", p()).unwrap_err().to_string();
    assert!(e.contains("unknown key \"x\""), "{e}");
}
```

Update `exports_schema_errors_have_lines` so its `rw = 5` case becomes `[all]\nrw = 5\n` (still line 2 of the error) and its `x = 1` case asserts `unknown key "x"`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p irsync --test config_files`
Expected: the new tests fail (old layout accepted, new layout rejected as unknown keys).

- [ ] **Step 3: Rewrite `Exports::parse` in `crates/irsync/src/config/exports.rs`**

Keep the manual walk over `BTreeMap<String, Spanned<toml::Value>>`. Pop `all` (must be a table; deserialize into a private `#[serde(deny_unknown_fields)] struct AllGrants { rw: Vec<String>, ro: Vec<String> }` with defaults; an error there is prefixed `[all]: `) and `exports` (must be a table; each entry is an export as today, with `exports.all` refused via `reserved("all")` at the entry's span). Anything left is an error at its span: a table → `unknown table {name:?}; exports live under [exports.{name}]`; the keys `rw`/`ro` → `unknown key {key:?}; grants for every export live under [all]`; anything else → `unknown key {key:?}`. `RESERVED` becomes `["all"]`; `reserved` becomes `pub`.

- [ ] **Step 4: Run the parser tests to verify they pass**

Run: `cargo test -p irsync --test config_files`
Expected: PASS.

- [ ] **Step 5: Write the failing writer tests**

Create `tests/manage_grants.rs` by moving `allow_moves_between_arrays_replaces_other_spelling_and_keeps_comment`, `revoke_by_export_by_all_and_everywhere`, `a_second_allow_after_a_comment_keeps_the_comment_and_valid_toml`, `revoke_matches_a_dangling_alias_by_literal_text` and the `INJECTED` tests out of `tests/manage_exports.rs`, renaming `allow` to `grant_add` and `revoke` to `grant_remove` and every fixture to the new layout (top-level arrays become `[all]`, `[p]` becomes `[exports.p]`). Add:

```rust
#[test]
fn grant_add_to_all_creates_the_all_table() {
    let mut doc: DocumentMut = "[exports.p]\npath = \"/p\"\n".parse().unwrap();
    grant_add(&mut doc, None, &peer(1), false, &|_| None).unwrap();
    let s = doc.to_string();
    assert!(s.contains("[all]\nrw = [") && s.contains(&id(1).to_string()), "{s}");
    assert!(Exports::parse(&s, Path::new("e")).unwrap().rw == vec![id(1).to_string()]);
}
```

In `tests/manage_exports.rs`, rewrite the remaining tests to the new layout and add:

```rust
#[test]
fn add_writes_under_exports_as_a_dotted_header_and_refuses_all() {
    let mut doc = DocumentMut::new();
    add_export(&mut doc, "photos", Path::new("/srv/photos"), false, false).unwrap();
    assert_eq!(doc.to_string(), "[exports.photos]\npath = \"/srv/photos\"\n");
    let e = add_export(&mut doc, "all", Path::new("/srv/all"), false, false).unwrap_err();
    assert_eq!(e.to_string(), "\"all\" is reserved: it is the word `grant add all WHO` uses for every export");
    assert!(add_export(&mut doc, "rw", Path::new("/srv/rw"), false, false).is_ok());
}

#[test]
fn remove_last_export_leaves_a_loadable_file() {
    let mut doc: DocumentMut = "[all]\nrw = [\"x\"]\n[exports.a]\npath = \"/a\"\n".parse().unwrap();
    remove_export(&mut doc, "a").unwrap();
    let x = Exports::parse(&doc.to_string(), Path::new("e")).unwrap();
    assert!(x.exports.is_empty() && x.rw == vec!["x"]);
    assert!(matches!(remove_export(&mut doc, "all"), Err(ManageError::NotFound(_))));
}
```

- [ ] **Step 6: Run the writer tests to verify they fail**

Run: `cargo test -p irsync --test manage_exports --test manage_grants`
Expected: compile error (`manage::grants` missing), then assertion failures on the layout.

- [ ] **Step 7: Implement the writers**

In `manage/exports.rs`: `add_export` gets or creates `doc["exports"]` as a table with `set_implicit(true)` (so the file prints `[exports.photos]`, never a bare `[exports]` header) and writes the export table under it; `remove_export` removes from that table and returns `NotFound` when the name is not a table-like child. Move `target`, `who_id`, `entry_matches`, `remove_where`, `push_entry`, `allow`, `revoke_in`, `revoke` into the new `manage/grants.rs` as `grant_add` and `grant_remove`: `target(doc, Some(n))` is `doc["exports"][n]`, `target(doc, None)` is `doc.entry("all").or_insert(table())`; the everywhere walk of `grant_remove` visits `[all]` then every table under `exports`. Add `pub mod grants;` to `manage/mod.rs` and change `ManageError::Reserved` to `#[error("{}", crate::config::exports::reserved(.0))]`. In `main.rs`, `run_allow` and `run_revoke` call the moved functions. In `show.rs`, the label becomes `all:`. In `authz.rs`, the `Grants::all` comment and the `export.unwrap_or("*")` log field say `all`. In `crates/stress/src/daemon.rs`, the written text becomes `[exports.data]\npath = {}\nrw = [{}]\n`.

- [ ] **Step 8: Update the remaining fixtures**

`tests/authz_snapshot.rs` (all four tests), `tests/daemon_auth.rs` (three `[data]` writes), `tests/isolated_e2e.rs` (two `[m]` writes), `tests/commands_e2e.rs` (the expected file text at line 55 becomes `[exports.photos]\npath = ...`; the `rw:` reserved assertion at lines 66-68 becomes an `all:` target expecting `reserved` in stderr; the `[p]` fixture at line 199). Add to `tests/commands_e2e.rs`:

```rust
#[test]
fn add_of_a_bare_path_named_all_is_refused_and_writes_nothing() {
    // home/srv/all exists; `irsync add <home>/srv/all` exits 1 with the reserved message; exports.toml does not exist afterwards
}
```

- [ ] **Step 9: Run the whole crate's tests, fmt and clippy**

Run: `cargo test -p irsync && cargo test -p irsync-stress && cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: all PASS, no warnings.

- [ ] **Step 10: Commit**

```bash
git add -A crates/irsync crates/stress
git commit -m "feat(config): exports.toml keeps exports under [exports.*] and every-export grants under [all]"
```

---

### Task 2: `hosts.toml` under `[hosts.*]`

**Files:**
- Modify: `crates/irsync/src/config/hosts.rs:50-100`
- Modify: `crates/irsync/src/manage/hosts.rs`
- Test: `crates/irsync/tests/config_files.rs`, `tests/manage_hosts.rs`, fixtures in `tests/manage_doc.rs:46`, `tests/authz_snapshot.rs`, `tests/commands_e2e.rs:26`, `tests/isolated_e2e.rs` (the `[local]`-style fixture if any)

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces:
  - `config::hosts::valid_alias(s: &str) -> bool` additionally false for `all` and `*`.
  - `config::hosts::Hosts { path, entries }` unchanged in shape.
  - `manage::hosts::host_add`, `host_remove`, `host_list` unchanged in signature, operating on `doc["hosts"]`.

- [ ] **Step 1: Write the failing tests**

In `tests/config_files.rs`, rewrite every hosts fixture (`[home]` → `[hosts.home]`, `[laptop]` → `[hosts.laptop]`, `["a:b"]` → `[hosts."a:b"]`) and add:

```rust
#[test]
fn hosts_old_layout_names_the_new_location() {
    let e = Hosts::parse("\n[home]\nendpoint_id = \"x\"\n", p()).unwrap_err().to_string();
    assert!(e.contains("unknown table \"home\"; hosts live under [hosts.home]") && e.contains(":2:"), "{e}");
    let e = Hosts::parse("x = 1\n", p()).unwrap_err().to_string();
    assert!(e.contains("unknown key \"x\""), "{e}");
    assert!(Hosts::parse("[hosts]\n", p()).unwrap().entries.is_empty());
}
```

Extend `alias_validity` with `assert!(!valid_alias("all") && !valid_alias("*"))`.

In `tests/manage_hosts.rs`, rewrite fixtures and add:

```rust
#[test]
fn host_add_writes_a_dotted_header_and_refuses_all() {
    let mut doc = DocumentMut::new();
    host_add(&mut doc, "home", id(1), &HostFlags::default()).unwrap();
    assert_eq!(doc.to_string(), format!("[hosts.home]\nendpoint_id = \"{}\"\n", id(1)));
    assert!(matches!(host_add(&mut doc, "all", id(1), &HostFlags::default()), Err(ManageError::BadAlias(_))));
    assert_eq!(host_list(&doc), vec![format!("home: {}", id(1))]);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p irsync --test config_files --test manage_hosts`
Expected: FAIL on the new layout.

- [ ] **Step 3: Implement the parser and writers**

`Hosts::parse` runs two passes over the same text: a shape pass into `BTreeMap<String, Spanned<toml::Value>>` that rejects any top-level key but `hosts` (a table → `unknown table {name:?}; hosts live under [hosts.{name}]`; else `unknown key {key:?}`) and requires `hosts` to be a table; then the typed pass into a private `#[serde(default, deny_unknown_fields)] struct RawFile { hosts: BTreeMap<String, Spanned<RawHost>> }`, after which the per-alias loop is unchanged (spans from both passes refer to the same text). `valid_alias` adds `s != "all" && s != "*"`. In `manage/hosts.rs`, `host_add` gets or creates `doc["hosts"]` as an implicit table and writes `[hosts.ALIAS]` under it; `host_remove` and `host_list` read that table.

- [ ] **Step 4: Update the remaining fixtures and run everything**

`tests/manage_doc.rs:46`, `tests/authz_snapshot.rs` (the alias fixtures and the `FileAuthorizer` test's `hosts.toml` write), `tests/commands_e2e.rs:26`, and any other `endpoint_id =` fixture `grep -rn 'endpoint_id = ' crates/irsync/tests` finds.

Run: `cargo test -p irsync && cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add -A crates/irsync
git commit -m "feat(config): hosts.toml keeps aliases under [hosts.*]; all and * are not aliases"
```

---

### Task 3: The noun-verb command tree

**Files:**
- Modify: `crates/irsync/src/cli/mod.rs:132-238,300-328`
- Modify: `crates/irsync/src/main.rs:33-59,702-846`
- Modify: `crates/irsync/src/session/reconnect.rs:159-168,370-377`
- Test: `crates/irsync/tests/cli.rs:384-465`, `tests/commands_e2e.rs`, `tests/live_exports_e2e.rs:134,142-167`

**Interfaces:**
- Consumes: `manage::exports::{add_export, remove_export, parse_add_target}`, `manage::grants::{grant_add, grant_remove}`, `manage::hosts::{host_add, host_remove, host_list}` from Tasks 1 and 2.
- Produces, in `irsync::cli`:

```rust
pub enum Command { Daemon(DaemonArgs), Keygen { out: Option<PathBuf> }, Gc { dry_run: bool, log: LogArgs }, Index(IndexArgs),
                   Host(HostArgs), Export(ExportArgs), Grant(GrantArgs),
                   Id { daemon: bool, all: bool, files: FilesArgs }, Show { files: FilesArgs } }
pub struct HostArgs   { #[command(subcommand)] pub cmd: HostCmd,   #[command(flatten)] pub files: FilesArgs }
pub struct ExportArgs { #[command(subcommand)] pub cmd: ExportCmd, #[command(flatten)] pub files: FilesArgs }
pub struct GrantArgs  { #[command(subcommand)] pub cmd: GrantCmd,  #[command(flatten)] pub files: FilesArgs }
pub enum HostCmd   { Add(HostAddArgs), Remove { alias: String }, List }
pub enum ExportCmd { Add(ExportAddArgs), Remove { name: String } }          // List arrives in Task 4
pub enum GrantCmd  { Add { export: String, who: String, ro: bool }, Remove { export: String, who: String } }  // List arrives in Task 5
pub struct ExportAddArgs { pub target: String, pub ro: bool, pub warm_index: bool }   // was AddArgs, minus files
pub struct FilesArgs { pub config: Option<PathBuf>, pub hosts: Option<PathBuf>, pub exports: Option<PathBuf> }  // each #[arg(long, global = true, value_name = "PATH")]
```

`HostAddArgs` loses its `files` field. The help docs on each variant are the spec's one-line descriptions (`Manage host aliases (edits hosts.toml)`, `Manage exports (edits exports.toml)`, `Manage grants: who may use which export (edits exports.toml)`).

- [ ] **Step 1: Write the failing parse tests in `tests/cli.rs`**

Replace `management_commands_parse` and `host_subcommands_parse` with:

```rust
#[test]
fn nouns_share_add_remove_list() {
    use irsync::cli::{ExportCmd, GrantCmd, HostCmd};
    assert!(matches!(parse(&["export", "add", "photos:/srv/photos", "--ro", "--warm-index"]).cmd,
        Some(Command::Export(ref e)) if matches!(e.cmd, ExportCmd::Add(ref a) if a.target == "photos:/srv/photos" && a.ro && a.warm_index)));
    assert!(matches!(parse(&["export", "remove", "photos"]).cmd,
        Some(Command::Export(ref e)) if matches!(e.cmd, ExportCmd::Remove { ref name } if name == "photos")));
    assert!(matches!(parse(&["grant", "add", "photos", "laptop", "--ro"]).cmd,
        Some(Command::Grant(ref g)) if matches!(g.cmd, GrantCmd::Add { ref export, ref who, ro: true } if export == "photos" && who == "laptop")));
    assert!(matches!(parse(&["grant", "remove", "all", "all"]).cmd,
        Some(Command::Grant(ref g)) if matches!(g.cmd, GrantCmd::Remove { .. })));
    assert!(matches!(parse(&["host", "remove", "home"]).cmd,
        Some(Command::Host(ref h)) if matches!(h.cmd, HostCmd::Remove { ref alias } if alias == "home")));
    assert!(matches!(parse(&["host", "list"]).cmd, Some(Command::Host(ref h)) if matches!(h.cmd, HostCmd::List)));
    // host add keeps every flag
    let a = parse(&["host", "add", "home", "abc", "--relay", "https://r", "--address", "1.2.3.4:1", "--address", "h:2",
                    "--parallel", "8", "--reconnect", "2h", "--dht", "no", "--mdns", "yes"]);
    let Some(Command::Host(h)) = a.cmd else { panic!() };
    let HostCmd::Add(h) = h.cmd else { panic!() };
    assert!(h.alias == "home" && h.id == "abc" && h.address.len() == 2 && h.dht == Some(false) && h.mdns == Some(true));
}

#[test]
fn grant_positionals_are_always_two() {
    for args in [vec!["grant", "remove", "laptop"], vec!["grant", "remove"], vec!["grant", "add", "photos"],
                 vec!["grant", "remove", "a", "b", "c"]] {
        let mut full = vec!["irsync"]; full.extend(args);
        assert!(Args::try_parse_from(full).is_err());
    }
}

#[test]
fn old_bare_verbs_are_gone() {
    for v in ["add", "remove", "allow", "revoke"] {
        assert!(Args::try_parse_from(["irsync", v, "photos"]).is_err(), "{v}");
    }
}

#[test]
fn file_flags_are_accepted_after_the_noun_in_either_position() {
    let after_noun = parse(&["host", "--hosts", "/h", "--config", "/c", "list"]);
    let after_verb = parse(&["host", "list", "--hosts", "/h", "--config", "/c"]);
    for a in [after_noun, after_verb] {
        let Some(Command::Host(h)) = a.cmd else { panic!() };
        assert_eq!((h.files.hosts, h.files.config, h.files.exports), (Some("/h".into()), Some("/c".into()), None));
    }
    let a = parse(&["grant", "add", "photos", "laptop", "--exports", "/e"]);
    let Some(Command::Grant(g)) = a.cmd else { panic!() };
    assert_eq!(g.files.exports, Some("/e".into()));
    // Before the noun the flag belongs to a sync, which conflicts with a subcommand.
    assert!(Args::try_parse_from(["irsync", "--hosts", "/h", "host", "list"]).is_err());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p irsync --test cli`
Expected: compile errors on the new names.

- [ ] **Step 3: Rebuild the `Command` tree in `cli/mod.rs`**

As in the Interfaces block. `FilesArgs` keeps `#[derive(clap::Args)]` with each field `global = true`; it stays flattened on `Id` and `Show` (where `global` on a leaf is harmless). Nothing in `Args::try_parse_from` or `command_tree` needs to change; the help template already recurses.

- [ ] **Step 4: Rewrite the dispatch in `main.rs`**

`run_host(&HostArgs) -> i32`, `run_export(&ExportArgs) -> i32`, `run_grant(&GrantArgs) -> i32`, each matching on `cmd` and calling the existing bodies of `host_add`/`run_remove`-style closures with `file_paths(&args.files)`. `finish` labels are `host add`, `host remove`, `host list`, `export add`, `export remove`, `grant add`, `grant remove`. `grant add`: `export == "all"` maps to `None`. `grant remove`: `export == "all"` maps to `None`, `who == "all"` maps to `None`, the dangling-alias literal fallback from `run_revoke` is kept, and the stderr line stays `removed N entries`. Delete `run_add`, `run_remove`, `run_allow`, `run_revoke` and the old `run_host`.

- [ ] **Step 5: Change the hint in `session/reconnect.rs`**

`describe_close` prints `On the daemon's machine run: irsync grant add {export} {client}`; the unit test asserts `irsync grant add photos {id}` and, in the `other` case, `!other.contains("grant")`.

- [ ] **Step 6: Rewrite the end-to-end tests**

In `tests/commands_e2e.rs` every `["add", t]` becomes `["export", "add", t]`, `["allow", e, w]` becomes `["grant", "add", e, w]`, `["revoke", w]` becomes `["grant", "remove", "all", w]`, `["revoke", e, w]` becomes `["grant", "remove", e, w]`, `["remove", n]` becomes `["export", "remove", n]`; rename the tests accordingly (`revoke_removes_an_alias_that_left_hosts_toml` → `grant_remove_all_removes_an_alias_that_left_hosts_toml`, and so on). Add:

```rust
#[test]
fn file_flags_after_the_noun_edit_the_named_files() {
    // `irsync export --exports <tmp>/e.toml add p:<dir>` writes <tmp>/e.toml, and
    // `irsync export add p2:<dir> --exports <tmp>/e.toml` appends to it; ~/.config/irsync/exports.toml is never created
}
```

In `tests/live_exports_e2e.rs` the same renames, and the two hint assertions become `irsync grant add data {client_id}`.

- [ ] **Step 7: Run everything, fmt and clippy**

Run: `cargo test -p irsync && cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: all PASS.

- [ ] **Step 8: Commit**

```bash
git add -A crates/irsync
git commit -m "feat(cli): host, export and grant nouns with add, remove and list; file flags global per noun"
```

---

### Task 4: `export list` and the shared `show` sections

**Files:**
- Modify: `crates/irsync/src/manage/show.rs`
- Modify: `crates/irsync/src/cli/mod.rs` (`ExportCmd::List`), `crates/irsync/src/main.rs` (`run_export`)
- Test: `crates/irsync/tests/cli.rs`, `tests/commands_e2e.rs`

**Interfaces:**
- Consumes: `Command::Export(ExportArgs)` from Task 3.
- Produces, in `manage::show`:
  - `pub fn grant_lines(out: &mut String, rw: &[String], ro: &[String], hosts: &Hosts)` (today's private `grants`, made public): one `    rw: a, b (id)` line and one `    ro: ...` line, each only when non-empty.
  - `pub fn export_blocks(exports: &Exports, hosts: &Hosts) -> String`: one block per export, `name: path`, then `    read_only`, `    warm_index`, then `grant_lines`.
  - `pub fn all_block(exports: &Exports, hosts: &Hosts) -> String`: `all:\n` plus `grant_lines`, or the empty string when `[all]` grants nobody.
  - `report(...)` unchanged in signature, composed of the file lines, identities, hosts lines, `export_blocks`, `all_block`.
  - `ExportCmd::List` variant.

- [ ] **Step 1: Write the failing tests**

`tests/cli.rs`: extend `nouns_share_add_remove_list` with `parse(&["export", "list"])` matching `ExportCmd::List`.

`tests/commands_e2e.rs`:

```rust
#[test]
fn export_list_prints_the_export_blocks_of_show() {
    // host add laptop <id>; export add p:<dir> --ro; export add q:<dir2>; grant add p laptop; grant add all laptop --ro
    // show_out = stdout of `show`; list_out = stdout of `export list`
    // the export blocks are the lines of show_out after the line starting with "daemon identity:" and before the line "all:"
    // assert_eq!(list_out, those lines joined with "\n" + "\n")
    // assert!(list_out.starts_with("p: ") && list_out.contains("    read_only\n") && list_out.contains("    rw: laptop ("))
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p irsync --test cli --test commands_e2e export_list`
Expected: compile error on `ExportCmd::List`.

- [ ] **Step 3: Implement**

Split `report` into the three public helpers; add `ExportCmd::List` with doc `List the exports with their paths, flags and grants`; `run_export` loads `Hosts` and `Exports` through `file_paths` and prints `export_blocks` with `print!`.

- [ ] **Step 4: Run, fmt, clippy, commit**

Run: `cargo test -p irsync && cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: all PASS.

```bash
git add -A crates/irsync
git commit -m "feat(cli): export list prints the export blocks of show"
```

---

### Task 5: `grant list`

**Files:**
- Modify: `crates/irsync/src/manage/grants.rs`
- Modify: `crates/irsync/src/cli/mod.rs` (`GrantCmd::List`), `crates/irsync/src/main.rs` (`run_grant`)
- Test: `crates/irsync/tests/manage_grants.rs`, `tests/cli.rs`, `tests/commands_e2e.rs`

**Interfaces:**
- Consumes: `manage::show::grant_lines` from Task 4; `authz::Snapshot::from_files(&Hosts, &Exports)` and `Snapshot::grants(&EndpointId) -> Grants { exports: BTreeMap<String, Access>, all: Option<Access> }`; `Hosts::resolve_peer(&str) -> Result<EndpointId, ConfigError>`.
- Produces, in `manage::grants`:
  - `pub fn list_all(exports: &Exports, hosts: &Hosts) -> String`: `all:\n` plus `grant_lines` (the header always, even when empty), then for every export in `BTreeMap` order `NAME:\n` plus `grant_lines` (header always).
  - `pub fn list_export(exports: &Exports, hosts: &Hosts, name: &str) -> Option<String>`: the one block of `list_all` for `name`, or for `all`; `None` when no such export.
  - `pub fn list_peer(exports: &Exports, hosts: &Hosts, label: &str, id: &EndpointId) -> String`: `{label}:\n` then, for every export in order, when `Snapshot::grants(id)` gives it access: `    NAME: rw` or `    NAME: ro`, with ` (all)` appended when the access came from `Grants::all` rather than a per-export entry, and ` (read_only export)` appended when the export is `read_only` and the access would have been `rw` (then printed as `ro`). Nothing under the header when the peer is granted nothing.
  - `GrantCmd::List { target: Option<String> }`.
  - In `run_grant`, the `target` is tried as an export name (including `all`) first, then `hosts.resolve_peer`; the label is `{alias} ({id})` for an alias and `{id}` for a raw id; anything else fails with `{target:?} is not an export, an alias in {hosts path} or an EndpointId`.

- [ ] **Step 1: Write the failing unit tests in `tests/manage_grants.rs`**

```rust
fn fixture() -> (Hosts, Exports) {
    let h = Hosts::parse(&format!("[hosts.laptop]\nendpoint_id = \"{}\"\n", id(1)), Path::new("h")).unwrap();
    let e = Exports::parse(&format!(
        "[all]\nrw = [\"laptop\"]\n[exports.backups]\npath = \"/b\"\n[exports.docs]\npath = \"/d\"\nread_only = true\nrw = [\"laptop\"]\n[exports.photos]\npath = \"/p\"\nro = [\"{}\"]\n",
        id(2)), Path::new("e")).unwrap();
    (h, e)
}

#[test]
fn list_all_names_every_export_and_the_all_table() {
    let (h, e) = fixture();
    let s = list_all(&e, &h);
    assert_eq!(s, format!("all:\n    rw: laptop ({l})\nbackups:\ndocs:\n    rw: laptop ({l})\nphotos:\n    ro: {o}\n", l = id(1), o = id(2)));
    assert_eq!(list_export(&e, &h, "docs").unwrap(), format!("docs:\n    rw: laptop ({})\n", id(1)));
    assert_eq!(list_export(&e, &h, "all").unwrap(), format!("all:\n    rw: laptop ({})\n", id(1)));
    assert!(list_export(&e, &h, "nope").is_none());
}

#[test]
fn list_peer_marks_inherited_and_capped_grants() {
    let (h, e) = fixture();
    let s = list_peer(&e, &h, &format!("laptop ({})", id(1)), &id(1));
    assert_eq!(s, format!("laptop ({}):\n    backups: rw (all)\n    docs: ro (read_only export)\n    photos: rw (all)\n", id(1)));
    assert_eq!(list_peer(&e, &h, &id(2).to_string(), &id(2)), format!("{}:\n    photos: ro\n", id(2)));
    assert_eq!(list_peer(&e, &h, "x", &id(3)), "x:\n");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p irsync --test manage_grants list_`
Expected: compile error (functions missing).

- [ ] **Step 3: Implement the three reporters**

In `manage/grants.rs`, using `show::grant_lines` and `authz::Snapshot`. The `(read_only export)` cap is applied by the reporter (the authorizer applies it in `daemon.rs`, not in `Grants::access`).

- [ ] **Step 4: Run the unit tests to verify they pass**

Run: `cargo test -p irsync --test manage_grants`
Expected: PASS.

- [ ] **Step 5: Write the failing CLI and end-to-end tests**

`tests/cli.rs`: `parse(&["grant", "list"])` gives `GrantCmd::List { target: None }`; `parse(&["grant", "list", "photos"])` gives `Some("photos")`; `["grant", "list", "a", "b"]` is an error.

`tests/commands_e2e.rs`:

```rust
#[test]
fn grant_list_by_export_by_peer_and_for_an_unknown_name() {
    // host add laptop <id>; export add p:<dir>; export add q:<dir2> --ro; grant add p laptop; grant add q laptop
    // `grant list` stdout starts with "all:\n" and contains "p:\n    rw: laptop (" and "q:\n    rw: laptop ("
    // `grant list p` stdout == "p:\n    rw: laptop (<id>)\n"
    // `grant list laptop` stdout == "laptop (<id>):\n    p: rw\n    q: ro (read_only export)\n"
    // `grant list <id>` stdout starts with "<id>:\n"
    // `grant list ghost` exits 1, stderr contains "\"ghost\" is not an export, an alias in" and "hosts.toml" and "EndpointId"
}
```

- [ ] **Step 6: Run to verify they fail, implement, run to verify they pass**

Add `GrantCmd::List { target: Option<String> }` (doc: `List grants: all of them, one export's, or what one peer may reach`) and the dispatch described in Interfaces.

Run: `cargo test -p irsync && cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: all PASS.

- [ ] **Step 7: Commit**

```bash
git add -A crates/irsync
git commit -m "feat(cli): grant list, for every export, one export or one peer"
```

---

### Task 6: Documentation

**Files:**
- Modify: `README.md:30-60` (quick start), `README.md:88-170` (the files section, both examples, the "Managing exports and hosts" table, the paragraph on which commands take which flags)
- Modify: `docs/daemon.md:1-60,70-90,290-300`
- Modify: `docs/superpowers/specs/2026-10-23-zero-config-cli-design.md` (one paragraph at the top of "Commands", "hosts.toml" and "exports.toml" pointing at the 2026-10-28 spec)

**Interfaces:** none.

- [ ] **Step 1: Rewrite the README**

Quick start as in the spec's Intent section. The command table becomes nine rows (`host add|remove|list`, `export add|remove|list`, `grant add|remove|list`) plus `id` and `show`, with the `grant list` forms described in one row. The `hosts.toml` example uses `[hosts.home]` and `[hosts.laptop]`; the `exports.toml` example uses `[all]`, `[exports.photos]`, `[exports.docs]`. The "Files" paragraph says the three nouns, `show` and `id` take `--config`, `--hosts` and `--exports` after the noun. The "Resuming" and "Deletions" sections are untouched.

- [ ] **Step 2: Rewrite the daemon doc**

Every `irsync add` becomes `irsync export add`, `allow` becomes `grant add`, `revoke` becomes `grant remove`, `host add`/`host remove` stay; the system-wide example passes `$F` after the noun (`sudo -u irsync irsync export $F add photos:/srv/photos` or after the verb, one form used consistently); the `[local]` example near the end becomes `[hosts.local]`; the hint paragraph quotes `irsync grant add <export> <id>`.

- [ ] **Step 3: Add the pointer paragraphs to the 2026-10-23 spec**

One sentence each, at the top of its "Commands", "hosts.toml" and "exports.toml" sections: superseded by `2026-10-28-config-cli-design.md`.

- [ ] **Step 4: Verify no old spelling survives**

Run:

```bash
grep -n 'irsync add\|irsync allow\|irsync revoke\|irsync remove\|`allow`\|`revoke`\|^\[photos\]\|^\[docs\]\|^\[home\]\|^\[laptop\]\|^\[local\]\|all exports\|^rw = \|^ro = ' README.md docs/daemon.md
```

Expected: no output.

- [ ] **Step 5: Commit**

```bash
git add README.md docs/daemon.md docs/superpowers/specs/2026-10-23-zero-config-cli-design.md
git commit -m "docs: the host, export and grant commands and the [hosts.*], [all], [exports.*] layouts"
```
