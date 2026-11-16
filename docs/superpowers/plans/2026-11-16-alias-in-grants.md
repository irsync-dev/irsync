# Aliases Instead of EndpointIds in the Config: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every save of `irsync.toml` writes peers by their `[hosts]` alias, `host remove` refuses while grants name the host, and `hosts.toml` / `exports.toml` are no longer read.

**Architecture:** First remove the split files so `irsync.toml` is the only source of hosts and grants. Then add a pure rewrite function over the parsed document (`manage::grants::canonicalize`) and run it inside `ConfigLock::save`, the one place all 11 save sites go through. `host remove` runs the same rewrite, then checks the grant lists.

**Tech Stack:** Rust, `toml_edit` (format-preserving edits), `toml` + serde (loading), clap (CLI), mdbook (book).

**Spec:** `docs/superpowers/specs/2026-11-16-alias-in-grants-design.md`. Read it before starting.

## Global Constraints

- Branch: `alias-in-grants`. Issues: #38, parts of #37.
- A save never changes who has access at the time of the save. It only changes how a peer is spelled.
- Only EndpointIds are rewritten. An entry that is already an alias is never changed, even when another alias of the same id sorts earlier.
- Two aliases with the same id: the first in lexicographic order is written.
- Within one table, `rw` wins over `ro`.
- Comments and layout of the file are kept.
- Old specs and plans under `docs/superpowers/` are not edited.
- Commit messages: a short summary line, a blank line, then why. No attribution trailers of any kind.
- Everyday test run: `cargo test -q --workspace --all-features --no-fail-fast`. Before the last commit: `tools/ci.sh check`.
- Book text is plain and short, in the style of the pages it sits in.

## Review Focus

Each line has its test in the task named.

1. A `hosts.toml` left on a server after the upgrade: the admin must see the warning on stderr from a plain management command, not only in a daemon log. (Task 1)
2. A grant list that is not an array of strings (a number in the array, or `rw = "x"`): the rewrite skips it and does not panic; the save then fails or passes exactly as it does today. (Task 2)
3. An id written in another accepted spelling (for example upper-case hex, if `parse_endpoint_id` accepts it): still rewritten to the alias. (Task 2)
4. A multi-line grant array with a comment on each entry, where the rewrite creates a duplicate: the duplicate goes, the file still loads, and the comments of the entries that stay are kept. (Task 2)
5. An alias and the grants of its id set in the same save through `config set hosts.NAME.endpoint_id`: the grants are renamed in that save. (Task 3)

---

### Task 1: Remove `hosts.toml` and `exports.toml`

**Files:**
- Modify: `crates/irsync/src/config/paths.rs`, `config/file.rs`, `config/hosts.rs`, `config/exports.rs`
- Modify: `crates/irsync/src/manage/mod.rs`, `manage/show.rs`, `authz.rs`, `main.rs`, `cli/mod.rs`, `ssh/setup.rs`, `ssh/onboard.rs`
- Modify: `crates/stress/src/daemon.rs:534`
- Test: `crates/irsync/tests/config_files.rs`, `commands_e2e.rs`, and every test file that `grep -rl "hosts\.toml\|exports\.toml\|Hosts::parse\|Exports::parse\|ensure_editable\|SplitFile\|split_hosts\|split_exports\|exports_file" crates/` lists
- Book: `book/src/reference/configuration.md`, `command-line.md`, `management.md`, `files-and-state.md`, `book/src/how-to/change-exports.md`, `run-the-daemon.md`

**Interfaces:**
- Produces:
  - `Paths { pub config: PathBuf }` only. `Paths::resolve` is unchanged in signature.
  - `Config` without `split_hosts` and `split_exports`. `Config::load(paths)` reads `paths.config` only.
  - `pub fn ignored_split_files(config: &Path) -> Vec<PathBuf>` in `config/file.rs`: those of `hosts.toml`, `exports.toml` that exist in the config file's directory, in that order.
  - `pub fn first_sighting(path: &Path) -> bool` in `config/file.rs`: true the first time this process asks about `path`, false after. `Config::load` warns only when it is true.
  - `Hosts::path` and `Exports::path` stay (error messages use them); they are always the config file.
  - Removed: `Hosts::parse`, `Exports::parse` (the stand-alone split-file parsers), `manage::ensure_editable`, `manage::Table`, `ManageError::SplitFile`, `Snapshot::exports_file`.

- [ ] **Step 1: Write the failing tests**

In `crates/irsync/tests/config_files.rs`, replace the split-file tests with:

```rust
#[test]
fn split_files_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("irsync.toml");
    std::fs::write(&config, "").unwrap();
    std::fs::write(
        dir.path().join("hosts.toml"),
        format!("[hosts.box]\nendpoint_id = \"{}\"\n", id(1)),
    )
    .unwrap();
    std::fs::write(dir.path().join("exports.toml"), "[exports.p]\npath = \"/p\"\n").unwrap();
    let c = Config::load(&Paths::resolve(Some(&config))).unwrap();
    assert!(c.hosts.entries.is_empty());
    assert!(c.exports.exports.is_empty());
    assert_eq!(
        ignored_split_files(&config),
        vec![dir.path().join("hosts.toml"), dir.path().join("exports.toml")]
    );
}

#[test]
fn no_split_files_nothing_ignored() {
    let dir = tempfile::tempdir().unwrap();
    assert!(ignored_split_files(&dir.path().join("irsync.toml")).is_empty());
}

#[test]
fn a_split_file_is_reported_once_per_process() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("hosts.toml");
    let b = dir.path().join("exports.toml");
    assert!(first_sighting(&a));
    assert!(!first_sighting(&a));
    assert!(first_sighting(&b));
}
```

In `crates/irsync/tests/commands_e2e.rs`, using the file's existing way of running the binary with `--config`:

```rust
#[test]
fn split_file_warns_on_stderr_and_is_not_read() {
    // hosts.toml beside the config holds [hosts.box]
    // run: irsync host list --config <dir>/irsync.toml
    // assert: exit 0, stdout has no "box",
    //         stderr contains "hosts.toml" and "ignor"
}

#[test]
fn host_add_works_beside_a_split_hosts_file() {
    // hosts.toml exists; run: irsync host add box <id> --config ...
    // assert: exit 0 and irsync.toml now has [hosts.box]
    // (this was refused before)
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -q -p irsync --test config_files --test commands_e2e`
Expected: compile error (`ignored_split_files` not found).

- [ ] **Step 3: Remove the split-file support**

Delete everything in the Interfaces "Removed" list and the `hosts` / `exports` fields of `Paths`, with all their uses. The grep in **Files** finds them. Where `main.rs` (`run_config`, around lines 1439-1454 and 1497) and `manage/show.rs` (`owner(...)`, lines 89-96) branch on a split file, keep only the config-file branch.

- [ ] **Step 4: Add the warning**

In `Config::load`, for each path from `ignored_split_files(&paths.config)`, the first time this process sees that path, warn:

```
ignoring {path}: hosts.toml and exports.toml are no longer read; move its tables into {config}
```

Keep the seen paths in a process-wide `static` (`Mutex<HashSet<PathBuf>>` or equivalent). Use `tracing::warn!`. If the e2e test shows that `tracing` output does not reach stderr for a management command, print it the way the root-account warning does (commit `f9612d1`) so the admin always sees it.

- [ ] **Step 5: Fix the remaining tests**

Tests that built hosts or exports with `Hosts::parse` / `Exports::parse` use `Config::parse(text, path)` and take `.hosts` / `.exports`. Tests that wrote a split file only to get tables into a test put the tables in `irsync.toml`. Tests of the removed refusal (`SplitFile`, conflict errors) are deleted. `crates/stress/src/daemon.rs:534` loses its split-file assertion.

- [ ] **Step 6: Update the book and `--help`**

Remove every mention of the split files from the six book pages and from the three doc comments in `cli/mod.rs` (lines 501, 536, 788). Remove the "Split files" section of `reference/configuration.md` and the link to it in `files-and-state.md`. In `reference/configuration.md`, add one short paragraph: a `hosts.toml` or `exports.toml` beside the config file is ignored with a warning; its tables belong in `irsync.toml`.

- [ ] **Step 7: Run the tests**

Run: `cargo test -q --workspace --all-features --no-fail-fast`
Expected: all pass.
Run: `grep -rn "hosts\.toml\|exports\.toml" crates/ book/src`
Expected: only the warning, `ignored_split_files`, their tests, and the one book paragraph.

- [ ] **Step 8: Commit**

```bash
git add -A crates book
git commit -m "Stop reading hosts.toml and exports.toml

irsync.toml is the only config file now. A split file found beside it
is ignored with a warning. Decided in #37."
```

---

### Task 2: The rewrite function

**Files:**
- Modify: `crates/irsync/src/config/hosts.rs` (add `alias_of`)
- Modify: `crates/irsync/src/manage/grants.rs` (add `canonicalize`)
- Test: `crates/irsync/tests/manage_grants.rs`

**Interfaces:**
- Produces:
  - `impl Hosts { pub fn alias_of(&self, id: &EndpointId) -> Option<&str> }`: the first alias in lexicographic order whose `endpoint_id` is `id` (`entries` is a `BTreeMap`, so the first match in iteration order).
  - `pub fn canonicalize(doc: &mut DocumentMut, hosts: &Hosts) -> bool` in `manage/grants.rs`: rewrites as the spec's "The rewrite" section says; returns whether the document changed.

- [ ] **Step 1: Write the failing tests**

In `crates/irsync/tests/manage_grants.rs`. A helper builds hosts from text: `fn hosts(text: &str) -> Hosts { Config::parse(text, Path::new("irsync.toml")).unwrap().hosts }`, and `fn canon(text: &str) -> (String, bool)` parses `text` as the document, takes its hosts the same way, runs `canonicalize`, and returns the document text and the flag. `id(n)` already exists in the file. Tests, one per line:

```rust
// [hosts.box] = id(1); [all] rw = [id(1)]            -> rw = ["box"], true
fn id_becomes_alias()
// same in [immediate].rw, [exports.p].rw, [exports.p].ro, and inline
// `exports = { p = { path = "/p", rw = [id(1)] } }`
fn every_grant_list_is_visited()
// [hosts.zed] and [hosts.amy] both id(1); rw = [id(1)] -> ["amy"]
fn first_alias_in_order_wins()
// rw = ["zed"] with amy and zed both id(1)             -> unchanged, false
fn existing_alias_is_left_alone()
// rw = [id(2)] with no alias for id(2); rw = ["ghost"] -> unchanged, false
fn unknown_entries_are_left_alone()
// rw = ["box", id(1)]                                  -> ["box"]
fn duplicate_in_one_list_keeps_the_first()
// rw = [id(1)], ro = ["box"]                           -> rw ["box"], ro []
fn rw_wins_over_ro()
// rw = ["amy", "zed"], both id(1)                      -> unchanged
fn two_aliases_of_one_id_both_stay()
// "rw = [\n    \"<id1>\", # laptop\n]\n" -> same text with "box" for the id
fn comment_and_layout_are_kept()
// run twice: second run returns false and the text is equal
fn second_run_changes_nothing()
// Review Focus 2: rw = [1, "<id1>"] and rw = "x"       -> no panic; the
// string id in the mixed array is still rewritten; rw = "x" untouched
fn odd_shapes_do_not_panic()
// Review Focus 3: only if parse_endpoint_id accepts upper-case hex:
// rw = [id(1).to_string().to_uppercase()]              -> ["box"]
fn other_spelling_of_an_id_is_rewritten()
// Review Focus 4: a multi-line rw with a comment per entry holding
// "box", id(2) and id(1): id(1) goes as a duplicate; the result parses
// with Config::parse, and the comments of "box" and id(2) are still there
fn duplicate_removal_keeps_other_comments()
```

Each asserts on the exact resulting array contents (and on the full text where the comment says so).

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -q -p irsync --test manage_grants`
Expected: compile error (`canonicalize`, `alias_of` not found).

- [ ] **Step 3: Implement `Hosts::alias_of` and `canonicalize`**

Replace an entry's text in place and copy its decor over, as `manage::config::set` does for a value, so comments and spacing stay. Reuse `remove_where`'s care for the first entry's prefix when a duplicate goes. Apply duplicate removal per table after the rename: first within each list, then drop from `ro` what is in `rw`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -q -p irsync --test manage_grants`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/config/hosts.rs crates/irsync/src/manage/grants.rs crates/irsync/tests/manage_grants.rs
git commit -m "Add the rewrite of EndpointIds to aliases in grant lists

A pure function on the parsed document, for the save and for host
remove to share. Part of #38."
```

---

### Task 3: Run the rewrite on every save

**Files:**
- Modify: `crates/irsync/src/manage/mod.rs` (`ConfigLock::save`, `save_config`, add `canonicalize_doc`, `PeerSpec::written`)
- Modify: every caller of `lock.save` / `save_config`: `main.rs`, `authz.rs:226`, `ssh/setup.rs:295,312`, `ssh/onboard.rs:92`
- Test: `crates/irsync/tests/manage_doc.rs`, `commands_e2e.rs`, `ssh_setup.rs`, `autoexport.rs`
- Book: `book/src/reference/configuration.md`, `management.md`

**Interfaces:**
- Consumes: `grants::canonicalize(doc, hosts) -> bool`, `Hosts::alias_of` (Task 2).
- Produces:
  - `pub fn canonicalize_doc(doc: &mut DocumentMut, path: &Path) -> Result<bool, ManageError>` in `manage/mod.rs`: loads the hosts from the document itself (`Config::parse(&doc.to_string(), path)`, mapped to `ManageError::Invalid` as `save` does today) and runs `grants::canonicalize`.
  - `ConfigLock::save(self, doc: &mut DocumentMut) -> Result<(), ManageError>` and `save_config(path, doc: &mut DocumentMut)`: run `canonicalize_doc`, then the existing "would it load" check on the result, then write.
  - `impl PeerSpec { pub fn written(&self, hosts: &Hosts) -> String }`: the alias for an `Id` that has one, else `entry()`.

- [ ] **Step 1: Write the failing tests**

`crates/irsync/tests/manage_doc.rs`:

```rust
#[test]
fn save_writes_aliases_for_ids() {
    // doc: [hosts.box] endpoint_id = id(1); [all] rw = [id(1)]
    // lock_config(path), save(&mut doc)
    // assert: file on disk has rw = ["box"], and doc itself has it too
}

#[test]
fn save_of_a_file_that_would_not_load_writes_nothing() {
    // doc with [hosts.box] endpoint_id = "nonsense"
    // assert: Err(ManageError::Invalid { .. }), file unchanged, lock file gone
}
```

`crates/irsync/tests/commands_e2e.rs` (binary runs, `--config` in a temp dir):

```rust
// host add box <id1>; grant add all <id1>
// -> file has rw = ["box"]; stderr of grant add starts with "box: rw on all exports"
fn grant_add_by_id_writes_and_prints_the_alias()
// file has [all] rw = [id1]; then: host add box <id1>
// -> rw = ["box"]
fn host_add_renames_existing_grants()
// file has [hosts.box] = id1 and [all] rw = [id1]; then: config set client.parallel 4
// -> rw = ["box"]
fn unrelated_config_set_cleans_the_file()
// Review Focus 5: file has [all] rw = [id1]; then:
// config set hosts.box.endpoint_id <id1>   -> rw = ["box"]
fn alias_set_through_config_renames_grants()
// grant add all "ssh-ed25519 AAAA... mike@laptop" for a key whose id is box's
// -> rw holds "box" and the text "# mike@laptop" is still in the file
fn key_comment_stays_beside_the_alias()
```

`crates/irsync/tests/ssh_setup.rs`: with `[hosts.laptop]` = the client id in the server config, `setup::run` leaves `[immediate] rw = ["laptop"]`.

`crates/irsync/tests/autoexport.rs`: with `[hosts.laptop]` = the peer id, `FileAuthorizer::add_auto_export` writes `rw = ["laptop"]` in the new export.

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -q -p irsync --test manage_doc --test commands_e2e --test ssh_setup --test autoexport`
Expected: compile error on `save(&mut doc)`, then failures on the assertions.

- [ ] **Step 3: Implement**

Add `canonicalize_doc`, change `save` and `save_config`, and pass `&mut doc` at every call site. In `main.rs`, the `grant add` messages (lines ~1663-1676) print `spec.written(&hosts)` instead of `spec.entry()`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -q --workspace --all-features --no-fail-fast`
Expected: all pass. Existing tests that asserted an id in a file where the test also defines an alias for it now assert the alias.

- [ ] **Step 5: Update the book**

`reference/configuration.md`, where grant entries are described: irsync writes a peer by its `[hosts]` alias whenever it saves the file, also for ids already there; with two aliases for one id it writes the first in alphabetical order; a grant by alias follows the alias if the host entry later gets another id. `reference/management.md`: one sentence under `grant add` that an id with an alias is stored as the alias.

- [ ] **Step 6: Commit**

```bash
git add -A crates book
git commit -m "Write aliases instead of EndpointIds on every config save

The save itself runs the rewrite, so grant, host, export, config,
ssh setup and the daemon's automatic exports all leave the same
clean file. Closes #38."
```

---

### Task 4: `host remove` refuses while grants name the host

**Files:**
- Modify: `crates/irsync/src/manage/grants.rs` (two helpers), `manage/hosts.rs` (`host_remove`), `manage/mod.rs` (error), `cli/mod.rs` (`HostCmd::Remove`), `main.rs` (`run_host`)
- Test: `crates/irsync/tests/manage_hosts.rs`, `commands_e2e.rs`
- Book: `book/src/reference/management.md`, `command-line.md`

**Interfaces:**
- Consumes: `manage::canonicalize_doc(doc, path)` (Task 3).
- Produces:
  - `pub fn lists_naming(doc: &DocumentMut, alias: &str) -> Vec<String>` in `grants.rs`: the tables with a grant entry whose text is `alias`, as `all`, `immediate`, `exports.NAME`, in that order, exports in file order, each once.
  - `pub fn remove_named(doc: &mut DocumentMut, alias: &str) -> usize` in `grants.rs`: removes those entries from every list (`[immediate]` included); the count.
  - `ManageError::HostInUse { alias: String, lists: Vec<String> }`, message: `host "{alias}" is named by grants in {lists joined by ", "}: remove those grants first, or pass --force`.
  - `pub fn host_remove(doc: &mut DocumentMut, alias: &str, force: bool) -> Result<usize, ManageError>`: `NotFound` as today; `HostInUse` when `lists_naming` is not empty and `force` is false; else removes the grants (with `force`) and the host, and returns the number of grants removed.
  - CLI: `HostCmd::Remove { alias: String, #[arg(short, long)] force: bool }`, help for the flag: `Also remove every grant that names the host.`

- [ ] **Step 1: Write the failing tests**

`crates/irsync/tests/manage_hosts.rs`:

```rust
// [hosts.box]; no grants                       -> Ok(0), host gone
fn remove_without_grants()
// [hosts.box]; [all] rw = ["box"]; [immediate] rw = ["box"];
// [exports.p] ro = ["box"]; force = false
// -> Err(HostInUse { lists == ["all", "immediate", "exports.p"] }),
//    doc text unchanged
fn remove_is_refused_while_grants_name_the_host()
// same doc, force = true -> Ok(3); host gone; no "box" left in any list;
// other entries of those lists still there
fn force_removes_the_grants_then_the_host()
// force = true on a missing alias -> Err(NotFound)
fn force_on_a_missing_host_is_not_found()
```

`crates/irsync/tests/commands_e2e.rs`:

```rust
// file: [hosts.box] = id1; [all] rw = [id1]  (the id, written by hand)
// host remove box        -> exit 1, stderr names "all" and "--force",
//                           file unchanged byte for byte
// host remove box -f     -> exit 0, no [hosts.box], rw is empty,
//                           stderr says 1 grant was removed
fn host_remove_counts_a_hand_written_id_as_a_grant()
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -q -p irsync --test manage_hosts --test commands_e2e`
Expected: compile error (`host_remove` takes 2 arguments).

- [ ] **Step 3: Implement**

In `run_host`, after `lock.load()`: `manage::canonicalize_doc(&mut doc, &paths.config)?`, then `host_remove(&mut doc, alias, *force)`, then save. On refusal nothing is saved (the lock drops). The success message adds the grant count when it is not zero, for example `removed host box and 1 grant from <file>`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -q --workspace --all-features --no-fail-fast`
Expected: all pass.

- [ ] **Step 5: Update the book**

`reference/management.md` under `host remove`: it refuses while any grant names the host and lists where; `--force` / `-f` removes those grants, then the host. `reference/command-line.md`: the new flag, if the page lists `host remove`.

- [ ] **Step 6: Full check**

Run: `tools/ci.sh check`
Expected: passes (formatting, clippy with and without OpenTelemetry, all tests including the slow ones, doc tests).

- [ ] **Step 7: Commit**

```bash
git add -A crates book
git commit -m "Refuse to remove a host that grants still name

Now that saves write aliases, most grants name a host, and removing
it would cut that peer's access without a word. --force removes the
grants first. Decided in #37."
```
