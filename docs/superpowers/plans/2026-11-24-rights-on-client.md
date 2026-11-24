# Rights on the client entry: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move `[all]` and `[immediate]` onto `[clients.NAME]` as `all_exports` and `all_paths` (`"rw"`|`"ro"`), rename `[daemon] immediate` to `allow_all_paths`, and add `irsync client set`.

**Architecture:** The new keys are added beside the old ones first (tasks 1 to 4 switch every user over), then the old tables are removed (task 5), so the workspace builds and passes after every task. The level a client gets through an automatic export is always its current `all_paths` level, never the level written in the export, so a change of `all_paths` takes effect at once in both directions.

**Tech Stack:** Rust, prost (config messages in `proto/irsync/config/config.proto`, never encoded), clap, toml/serde.

**Spec:** `docs/superpowers/specs/2026-11-24-rights-on-client-design.md` (and issue #284).

## Global Constraints

- Values are `"rw"` or `"ro"`; a key left out means no right. `client set` also takes `none`, which removes the key.
- Only named clients (`[clients.NAME]`) hold `all_exports` / `all_paths`.
- `allow_all_paths` defaults to `true`, as `immediate` did.
- An export's own `rw`/`ro` entry wins over `all_exports`; `all_exports` never reaches an automatic export (`auto = true`).
- No upgrade of a layout-2 file that still has `[all]`, `[immediate]` or `[daemon] immediate`: it is refused with a hint naming the new key. The layout-1 upgrade (`legacy_v1.rs`) converts them (task 5).
- The reader, writer, `client set` and listings take the rights from one list, so #203 adds `admin` as a third entry.
- Plain words in messages and the book; home paths written as `~/...`; no attribution lines in commits.
- `cargo test -q --workspace --all-features --no-fail-fast` passes after every task; `tools/ci.sh check` before the push.

## Review Focus

1. A client lowered `rw` → `ro` and raised back to `rw` writes again through an automatic export made while it was `ro` (the export lists it under `ro`). Test in task 2.
2. A push by an `all_paths = "ro"` client to a missing directory is refused and creates nothing. Test in task 2.
3. Two client names with the same id and different levels: `rw` wins, as in the export lists. Test in task 2.
4. With `allow_all_paths = false`, a client whose only right is `all_paths` is refused before any message is read. Test in task 2.
5. `client set` with a bad second pair (`all_exports=rw all_paths=yes`) leaves the file unchanged. Test in task 3.

---

### Task 1: The new keys in the config message, reader, writer and `config get/set`

**Files:**
- Modify: `proto/irsync/config/config.proto` (`ClientEntry`, `Daemon`, new enum)
- Modify: `crates/irsync/src/config/reader.rs` (`RawClientEntry`, `clients()`, `RawDaemon`)
- Modify: `crates/irsync/src/config/writer.rs` (`client_entry`, `daemon`)
- Modify: `crates/irsync/src/config/clients.rs` (`ClientEntry`)
- Modify: `crates/irsync/src/config/settings.rs` (`Daemon.allow_all_paths`)
- Modify: `crates/irsync/src/manage/config.rs` (`kind_of`, `CLIENT_ENTRY`, set/get of the new keys)
- Test: `crates/irsync/tests/config_files.rs`, `crates/irsync/tests/manage_config.rs`

**Interfaces:**
- Produces:
  - proto: `enum Access { ACCESS_UNSPECIFIED = 0; ACCESS_RW = 1; ACCESS_RO = 2; }`; `ClientEntry`: `optional Access all_exports = 6; optional Access all_paths = 7;`; `Daemon`: `optional bool allow_all_paths = 19;`.
  - `config::clients::ClientEntry { client_id: EndpointId, all_exports: Option<authz::Access>, all_paths: Option<authz::Access> }`.
  - `config::clients::RIGHTS: [Right; 2]` and `pub enum Right { AllExports, AllPaths }` with `fn key(self) -> &'static str` (`"all_exports"`, `"all_paths"`), `fn get(self, c: &pb::ClientEntry) -> Option<Access>`, `fn set(self, c: &mut pb::ClientEntry, a: Option<Access>)`. Reader, writer, `client set` and listings loop over `RIGHTS`.
  - `authz::Access`: `fn as_str(self) -> &'static str` (`"rw"`/`"ro"`) and `fn parse(s: &str) -> Option<Access>`; conversions to and from `pb::Access`.
  - `settings::Daemon.allow_all_paths: bool` (default `true`). For this task it is read from `allow_all_paths`, falling back to the old `immediate` key, so existing users keep working until task 5.

- [ ] **Step 1: Write the failing tests**

In `config_files.rs`:
- `client_rights_are_read_and_written_back`: a file with `[clients.laptop]` having `client_id`, `all_exports = "ro"` and `all_paths = "rw"` loads with `clients.get("laptop")` giving `all_exports == Some(Access::ReadOnly)` and `all_paths == Some(Access::ReadWrite)`; a load-save round trip gives the same text, keys in that order (client_id, all_exports, all_paths), with a trailing comment on `all_paths` kept.
- `a_client_right_other_than_rw_or_ro_is_refused`: `all_paths = "yes"` fails, with the message containing `client "laptop": all_paths must be "rw" or "ro"` and the line number.
- `allow_all_paths_defaults_to_on`: no `[daemon]` gives `settings.daemon.allow_all_paths == true`; `allow_all_paths = false` gives `false`.

In `manage_config.rs`:
- `config_set_takes_client_rights`: `config set clients.laptop.all_paths ro` then `config get` prints `ro`; `config set clients.laptop.all_paths maybe` is refused.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -q -p irsync --test config_files --test manage_config`
Expected: compile errors on `all_exports`, `all_paths` and `allow_all_paths`.

- [ ] **Step 3: Implement**

The reader checks the two values and nothing else (unknown keys are refused as today). In `manage/config.rs` the new keys are `Str` with the value checked by `Access::parse`; `["daemon", "allow_all_paths"]` is `Bool`.

- [ ] **Step 4: Run all tests**

Run: `cargo test -q --workspace --all-features --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

`git commit -m "Read and write all_exports and all_paths on clients"`, with a line on why: the server-wide rights are moving onto the client.

---

### Task 2: The permission check uses the client's rights

**Files:**
- Modify: `crates/irsync/src/authz.rs` (`Grants`, `Snapshot::from_files`, `Authorizer::add_auto_export`, both authorizers)
- Modify: `crates/irsync/src/manage/exports.rs:78` (`add_auto_export`)
- Modify: `crates/irsync/src/daemon.rs` (around 576 and `serve_immediate` / `add_immediate`, 735-830; the warning near 1182)
- Modify: `crates/irsync/src/resolve.rs` and test helpers in `crates/irsync/tests/common/mod.rs` where `Grants` is built
- Test: `crates/irsync/tests/authz_snapshot.rs`, `crates/irsync/tests/daemon_auth.rs`, `crates/irsync/tests/autoexport.rs`, `crates/irsync/tests/resolve.rs`, `crates/irsync/tests/pq_key_exchange.rs`

**Interfaces:**
- Consumes: `ClientEntry.all_exports`, `ClientEntry.all_paths`, `settings.daemon.allow_all_paths` (task 1).
- Produces:
  - `authz::Grants { exports: BTreeMap<String, Access>, all_exports: Option<Access>, all_paths: Option<Access> }` (`all` and `immediate` are gone; `is_empty`, `access`, `access_to` use `all_exports`).
  - `Snapshot::from_files(clients: &Clients, exports: &Exports, allow_all_paths: bool) -> Snapshot`.
  - `Authorizer::add_auto_export(&self, root: &Path, peer: EndpointId, access: Access) -> Result<(), AuthzError>`.
  - `manage::exports::add_auto_export(msg: &mut pb::Config, path: &Path, peer: EndpointId, access: Access) -> Result<String, ManageError>`: the peer goes in `ro` for `ReadOnly`, else `rw`.

`from_files` rules:
- `all_exports` and `all_paths` come from each client entry, keyed by its id; `all_paths` only when `allow_all_paths`. Two entries with one id: `rw` beats `ro`.
- The old `[all]` / `[immediate]` lists still count until task 5 (`[all]` as `all_exports`, `[immediate].rw` as `all_paths = rw`), merged with the same `rw` beats `ro` rule.
- After the export lists are applied: for each key, every automatic export in its `exports` map is set to its `all_paths` level, or removed when it has none.

Daemon: the automatic export path runs when `grants.all_paths.is_some()`. A push (`CLIENT_SENDS`) by a key whose `all_paths` is `ReadOnly` is refused before `auto_root` runs, with the same code and text as a push into a read-only export. The new export is added with the key's `all_paths` level.

- [ ] **Step 1: Write the failing tests**

In `authz_snapshot.rs`:
- `all_exports_reaches_every_hand_made_export`: a client with `all_exports = "ro"` gets `access("docs") == Some(ReadOnly)` and no access to an `auto` export.
- `an_exports_own_entry_wins_over_all_exports`: `all_exports = "rw"` and `[exports.docs] ro = ["laptop"]` → `access("docs") == Some(ReadOnly)`.
- `an_automatic_export_gives_the_current_all_paths_level`: an auto export listing the client under `rw`, client `all_paths = "ro"` → `ReadOnly`; the same export listing it under `ro` with `all_paths = "rw"` → `ReadWrite` (Review Focus 1).
- `an_automatic_export_counts_only_with_all_paths_allowed`: `allow_all_paths = false` → no access through it and `grants.all_paths == None`.
- `two_names_for_one_id_take_the_higher_level`: `a` with `all_paths = "ro"` and `b` with `all_paths = "rw"`, same id → `ReadWrite` (Review Focus 3).

In `daemon_auth.rs` (or `autoexport.rs`, whichever already drives the daemon with a config file):
- `all_paths_ro_pulls_any_path_and_makes_a_ro_export`: a pull of `~/data` succeeds; the config then has an auto export listing the client under `ro`.
- `all_paths_ro_cannot_push_and_creates_nothing`: a push to `~/new` (missing) is refused as read-only, `~/new` does not exist and the config is unchanged (Review Focus 2).
- `lowering_all_paths_takes_effect_on_existing_auto_exports`: push works with `rw`; after `client set laptop all_paths=ro` (or a direct edit) a second push to the same path is refused.
- `all_paths_alone_is_refused_when_not_allowed`: `allow_all_paths = false` and a client with only `all_paths` → the connection is closed `unauthorized` before the Hello (Review Focus 4).

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -q -p irsync --all-features --test authz_snapshot --test daemon_auth --test autoexport`
Expected: compile errors on the new `Grants` fields and `from_files` arguments.

- [ ] **Step 3: Implement** as in the rules above; update every `Grants { .. }` literal in tests and helpers.

- [ ] **Step 4: Run all tests**

Run: `cargo test -q --workspace --all-features --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

`git commit -m "Grant access from all_exports and all_paths on the client"`, with a why line: `all_paths = "ro"` is new, and a lowered level must apply to existing automatic exports.

---

### Task 3: `irsync client set`, `grant` without `all` and `immediate`, and the listings

**Files:**
- Modify: `crates/irsync/src/cli/mod.rs:520` (`ClientCmd::Set`), `:585` (`GrantCmd` help texts)
- Modify: `crates/irsync/src/main.rs:1845-1960` (`run_grant`), `:2030-2120` (`run_client`)
- Modify: `crates/irsync/src/manage/clients.rs` (`client_set`, `client_list`)
- Modify: `crates/irsync/src/manage/grants.rs` (`list_all`, `list_export`, `list_peer`; remove `grant_immediate` and `grant_remove_immediate` once task 4 no longer uses them, or leave them for task 4 to remove)
- Modify: `crates/irsync/src/manage/show.rs` (`all_block` goes; the clients section carries the rights)
- Modify: `crates/irsync/src/config/exports.rs:41` (`reserved` message)
- Test: `crates/irsync/tests/manage_clients.rs`, `crates/irsync/tests/manage_grants.rs`, `crates/irsync/tests/commands_e2e.rs`

**Interfaces:**
- Consumes: `Right`, `RIGHTS`, `Access::parse` (task 1); `Snapshot::from_files`, `Grants.all_paths` (task 2).
- Produces:
  - `manage::clients::parse_setting(s: &str) -> Result<(Right, Option<Access>), ManageError>`: `KEY=rw|ro|none`.
  - `manage::clients::client_set(msg: &mut pb::Config, name: &str, settings: &[(Right, Option<Access>)]) -> Result<(), ManageError>`: `NotFound` for an unknown client.
  - `ClientCmd::Set { name: String, #[arg(required = true, value_name = "KEY=VALUE")] settings: Vec<String> }`, help: "Set a client's server-wide rights: all_exports=rw|ro|none, all_paths=rw|ro|none".

Behaviour:
- `client set` parses every pair before it changes anything; prints `laptop: all_exports=rw all_paths=ro` (every right, `none` for an unset one) to stderr.
- `client list` (and the clients section of `show`): `name: id`, then ` all_exports=rw` / ` all_paths=ro` for each right that is set.
- `grant add all WHO` is refused: `"all" is not an export: give a client every export with: irsync client set NAME all_exports=rw` (`=ro` with `--ro`). `grant add immediate WHO`: `... any path with: irsync client set NAME all_paths=rw`. `grant remove immediate WHO`: `... irsync client set NAME all_paths=none`.
- `grant remove all WHO` keeps its meaning "every export's own list" (no longer `[all]`); its help says so.
- `grant list` begins with an `all_exports:` block and an `all_paths:` block (`rw:` and `ro:` lines of client names, as `grant_lines` writes them), then one block per export. `grant list PEER` shows `    all_paths: rw|ro` and marks access that comes from `all_exports` with ` (all_exports)` instead of ` (all)`.
- `client remove` takes the client's rights with it; rights alone never make it "in use".

- [ ] **Step 1: Write the failing tests**

In `manage_clients.rs`:
- `client_set_sets_and_removes_rights`: `[(Right::AllExports, Some(Access::ReadWrite)), (Right::AllPaths, Some(Access::ReadOnly))]` sets both; then `(Right::AllPaths, None)` removes the key from the written file.
- `client_set_on_an_unknown_client_is_refused`: `NotFound("nope")`.
- `parse_setting_takes_rw_ro_none`: `"all_paths=ro"` → `(AllPaths, Some(ReadOnly))`; `"all_paths=none"` → `(AllPaths, None)`; `"all_paths=yes"`, `"admin=rw"` and `"all_paths"` are `BadValue`.
- `client_list_shows_rights`: a line `laptop: <id> all_exports=rw all_paths=ro`.

In `commands_e2e.rs`:
- `client_set_with_a_bad_pair_changes_nothing`: `irsync client set laptop all_exports=rw all_paths=yes` exits non-zero and the file is byte-for-byte unchanged (Review Focus 5).
- `grant_add_all_points_to_client_set`: exits non-zero; stderr contains `irsync client set NAME all_exports=rw`.
- `grant_list_shows_client_rights`: after `client set laptop all_paths=rw`, `grant list` has `all_paths:\n    rw: laptop (<id>)`.

Update the existing tests in `manage_grants.rs` and `commands_e2e.rs` that use `grant add all`, `grant add immediate` and the `all:`/`immediate:` blocks to the new commands and output.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -q -p irsync --all-features --test manage_clients --test manage_grants --test commands_e2e`
Expected: compile errors on `client_set` / `parse_setting`, then failures on the listing text.

- [ ] **Step 3: Implement** as above.

- [ ] **Step 4: Run all tests**

Run: `cargo test -q --workspace --all-features --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

`git commit -m "Add irsync client set for a client's server-wide rights"`, with a why line: one command for all server-wide rights, which #203 extends with admin.

---

### Task 4: The SSH setup sets `all_paths = "rw"`

**Files:**
- Modify: `crates/irsync/src/ssh/setup.rs:385-520` (`grant_client`, `run`, `GrantOutcome` strings near 148 and 256)
- Modify: `crates/irsync/src/ssh/onboard.rs:360-375` (messages)
- Test: `crates/irsync/tests/ssh_setup.rs`, `crates/irsync/tests/onboard_e2e.rs`

**Interfaces:**
- Consumes: `client_set` (task 3), `settings.daemon.allow_all_paths` (task 1).
- Produces: `grant_client(...) -> anyhow::Result<bool>` unchanged in shape: true when it set `all_paths = "rw"` (the client had none or `"ro"`), false when it already had `"rw"`. `GrantOutcome::ImmediateOff` becomes `GrantOutcome::AllPathsOff`, written as `all_paths_off`.

Behaviour: `run` checks `allow_all_paths` where it checked `immediate`. On a recheck of a server that was not rebuilt, a client without `all_paths = "rw"` (none or `"ro"`) is reported `Missing` and nothing is saved: the owner lowered it on purpose. Onboard messages say "has no all_paths right on DEST any more" and "allow_all_paths is off on DEST".

- [ ] **Step 1: Write the failing tests**

In `ssh_setup.rs`:
- `setup_sets_all_paths_rw_on_the_client`: after `run`, the file has `[clients.<host>]` with `all_paths = "rw"` and no `[immediate]`; outcome `Added`.
- `a_second_setup_reports_already`: outcome `Already`, file unchanged.
- `a_recheck_after_lowering_to_ro_reports_missing`: `all_paths = "ro"`, recheck, existing identity → `Missing`, file still `"ro"`.
- `setup_with_all_paths_off_grants_nothing`: `allow_all_paths = false` → `AllPathsOff`, no client written.

Update the existing tests that look for `[immediate]` or `immediate_off`.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -q -p irsync --all-features --test ssh_setup --test onboard_e2e`
Expected: FAIL (the file still gets `[immediate]`).

- [ ] **Step 3: Implement**; then remove `grant_immediate` and `grant_remove_immediate` from `manage/grants.rs` if task 3 left them.

- [ ] **Step 4: Run all tests**

Run: `cargo test -q --workspace --all-features --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

`git commit -m "Let the SSH setup give its client all_paths"`.

---

### Task 5: Remove `[all]`, `[immediate]` and `[daemon] immediate`; convert them in the layout-1 upgrade

**Files:**
- Modify: `proto/irsync/config/config.proto` (drop `Config.all`, `Config.immediate`, `Daemon.immediate`; `GrantTable` goes if nothing else uses it)
- Modify: `crates/irsync/src/config/reader.rs` (`RawFile.all/immediate`, `immediate_problem`, `check_shape`, `RawDaemon.immediate`, the `rw`/`ro` hint at top level)
- Modify: `crates/irsync/src/config/writer.rs:60-70`, `:148`
- Modify: `crates/irsync/src/config/exports.rs` (`Exports.rw`, `.ro`, `.immediate_rw` go)
- Modify: `crates/irsync/src/config/comments.rs:408-409`, `crates/irsync/src/config/settings.rs` (fallback from task 1 goes)
- Modify: `crates/irsync/src/manage/config.rs` (`["all"]`, `["immediate"]`, `["daemon", "immediate"]` go), `crates/irsync/src/manage/grants.rs` (`grant_lists`, `grant_lists_mut`, `target`, `grant_remove`, `canonicalize` no longer see the tables)
- Modify: `crates/irsync/src/authz.rs` (the task-2 fallback to the old lists goes)
- Modify: `crates/irsync/src/config/legacy_v1.rs` (conversion)
- Test: `crates/irsync/tests/config_files.rs`, `crates/irsync/tests/config_upgrade.rs`, `crates/irsync/tests/config_message.rs`, `crates/irsync/tests/manage_doc.rs`, fixtures under `crates/irsync/tests/fixtures/config_v1/` and `config_corpus/`

**Interfaces:**
- Consumes: `Right` / `Right::set` (task 1).
- Produces: in `legacy_v1.rs`, `fn rights_onto_clients(config: &mut pb::Config, all: Option<pb::GrantTable-like lists>, immediate_rw: Vec<pb::Entry>) -> Vec<String>` (its own local types; `GrantTable` may stay private to this file). Returns the lines it could not convert, added to `Converted::dropped`.

Layout-2 reader messages (line number as usual):
- `[all]`: `[all] is not read any more: give a client all_exports = "rw" or "ro" under [clients.NAME] (irsync client set NAME all_exports=rw)`.
- `[immediate]`: the same with `all_paths`.
- `[daemon] immediate`: `[daemon] immediate is now allow_all_paths`.

Layout-1 conversion, after the hosts are split into servers and clients:
- Each entry of `[all].rw` / `[all].ro` / `[immediate].rw` that names a client, or is an id some client has, sets that client's `all_exports` / `all_paths` (`rw` beats `ro`).
- An id no client has becomes a client named `client-` + the first 10 hex digits of the id (with `-2`, `-3` when taken), with the comment `# made from a grant to this id in [all] or [immediate]` above it.
- Text that names nobody is dropped and reported in `dropped`.
- `[daemon] immediate` becomes `allow_all_paths`.

- [ ] **Step 1: Write the failing tests**

In `config_files.rs`:
- `old_all_table_is_refused_with_a_hint`, `old_immediate_table_is_refused_with_a_hint`, `old_daemon_immediate_is_refused_with_a_hint`: the messages above.

In `config_upgrade.rs`, update the expected `.v2.toml` fixtures and add:
- `immediate_only` upgrades to `[clients.laptop]` with `all_paths = "rw"` and `[daemon] allow_all_paths = true`, no `[immediate]`.
- `grants_by_name_and_id`: the `[all]` entries land as `all_exports` on their clients.
- `a_grant_to_an_unknown_id_becomes_a_client`: a v1 file with `[all] rw = ["<id>"]` and no host for it upgrades to `[clients.client-<10 hex>]` with `all_exports = "rw"`.
- The `.grants` files (who may reach what, before and after) stay the same: the conversion changes no one's access.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -q -p irsync --all-features --test config_files --test config_upgrade`
Expected: FAIL (old tables still read).

- [ ] **Step 3: Implement**; `rg -n '\\[all\\]|\\[immediate\\]|immediate_rw|grant_immediate|daemon.immediate' crates` finds nothing left outside `legacy_v1.rs` and its fixtures.

- [ ] **Step 4: Run all tests**

Run: `cargo test -q --workspace --all-features --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

`git commit -m "Stop reading [all] and [immediate]"`, with a why line: the rights live on the client now; the layout-1 upgrade converts them.

---

### Task 6: The book

**Files:**
- Modify: `book/src/reference/configuration.md` (Clients, Exports, the immediate-grant section renamed to all_paths, How grants combine, the example), `book/src/reference/management.md` (`client set`, `grant` without `all`/`immediate`), `book/src/reference/command-line.md`, `book/src/how-to/ssh-setup.md` if it names `[immediate]`, `book/src/how-to/pair.md`, `book/src/explanation/threat-model.md` ("The immediate grant is the whole account" → `all_paths = "rw"`; add that `all_paths = "ro"` reads the whole account, including `~/.ssh` and irsync's config), `book/src/explanation/identity-and-trust.md`, `book/src/introduction.md`, `book/src/tutorial/first-sync.md`, `book/src/how-to/troubleshooting.md`, `book/src/reference/remote-addresses.md`, `book/src/reference/files-and-state.md`, `book/src/contributing/netstress.md`

- [ ] **Step 1:** `rg -n 'immediate|\\[all\\]|grant add all' book/src` and rewrite each hit (keep "immediate" where it means something else, such as ACKs).
- [ ] **Step 2:** Run `python3 -m unittest discover -s tools/tests` (the man pages are built from some of these chapters). Expected: OK.
- [ ] **Step 3:** `tools/ci.sh check`. Expected: passes.
- [ ] **Step 4:** Commit: `git commit -m "Describe all_exports, all_paths and client set in the book"`.
