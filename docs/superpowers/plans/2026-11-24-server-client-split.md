# Servers and Clients (Stage 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `[hosts]` is split into `[servers.NAME]` (what this machine dials) and `[clients.NAME]` (who it lets in); old files are read by a frozen `legacy_v1` reader and rewritten as `config_version = 2` on the first save, with the same access before and after.

**Architecture:** The config message loses `Host` and gains `ServerEntry` and `ClientEntry`. `config/reader.rs` becomes the version 2 reader; the stage 1 reader is copied to `config/legacy_v1.rs` and fills the new message by the conversion rules. `reader::read` picks the reader. The typed `Config` gets `servers` and `clients` in place of `hosts`; dialling uses servers, grants use clients. `irsync host` becomes `irsync server` and `irsync client`. `ConfigLock::save` refuses a conversion while an older daemon runs.

**Tech Stack:** Rust, prost (via `crates/irsync-proto`), `toml` and `toml_edit` in the readers and the writer, `semver` (already in `Cargo.lock`) for the daemon's version.

**Spec:** `docs/superpowers/specs/2026-11-22-config-message-and-server-client-split-design.md`, section "Stage 2". Stage 1 (PR #258) is merged. The PR for this plan closes #202.

## Global Constraints

- No config key and no command mixes a dialled identity with a granted one. The "`client_id`, else `endpoint_id`" rule exists only in `config/legacy_v1.rs`.
- An old file works unchanged after the upgrade, before and after it is rewritten, and grants the same access both times (`authz::Snapshot` equal).
- No comment is lost by a conversion or an edit.
- `config/legacy_v1.rs` is a copy of the stage 1 reader with its own serde structs and its own error texts. It may share only the parsers of single values (`duration`, `parse_endpoint_id`, `HostPort`, `UrlText`-style checks) and the comment walker (see Task 3). Nothing outside `config/` changes when a later layout change adds `legacy_v2`.
- The error cases of `crates/irsync/tests/config_files.rs` keep their messages and line numbers: their files have `[hosts]` and so go through `legacy_v1`.
- The current reader and the writer know `config_version`; the writer writes it as the first key when the message sets it, and every save sets it to `CONFIG_VERSION = 2`.
- A server and a client may have the same name. Both follow `valid_alias`.
- Grant lists (`rw`, `ro`, `[all]`, `[immediate]`) resolve against clients only.
- No `unsafe`. After a `Cargo.lock` change, run `tools/sync-fuzz-lock.sh` and commit both lock files.
- Everyday run: `cargo test -q --workspace --all-features --no-fail-fast`. Before the push: `tools/ci.sh check`.
- Commit messages: a short summary line, a blank line, then why, if it is not obvious. No attribution lines or trailers of any kind.

## Review Focus

Inputs the spec implies but does not spell out. Each has its test in the task named.

1. **One id in two old entries**: `[hosts.a]` has `endpoint_id = X` and no `client_id`, `[hosts.b]` has `client_id = X`, grants name both. After conversion both names resolve to X, `alias_of(X)` is `a` (first in order), and the snapshot equals the old one. Task 3, `shared_id_keeps_its_grants`.
2. **A name granted only in `[immediate]`** (or only in `[all]`) still gets its client entry. Task 3, `every_grant_list_counts_for_keep_the_access`.
3. **Comments on a bare `[hosts]` header, or on `hosts = { … }`,** land on the first converted entry, server or client. Task 3, `group_comments_go_to_the_first_entry`.
4. **`config_version` with a wrong type or value** (`"2"`, `2.0`, `0`, `-1`, `3`): a clear error at its line; never a silent fallback to `legacy_v1`. Task 5, `bad_config_version_values`.
5. **A daemon lock whose holder cannot be told** (record being written, `pid = 0`, or a version string that does not parse): the conversion is refused, as for an old daemon, and the file is left as it was. Task 6, `unknown_daemon_version_refuses_conversion`.

---

## File Structure

| File | Responsibility |
|---|---|
| `proto/irsync/config/config.proto` | `ServerEntry`, `ClientEntry`, `Config.servers`, `Config.clients`, `Config.config_version`; `Host` removed in Task 5. |
| `crates/irsync/src/config/names.rs` (new) | `valid_alias`, `short_host_name`, `invalid_name`: the name rules both tables share. |
| `crates/irsync/src/config/servers.rs` (new) | Typed `ServerEntry`, `Servers` (`get`, `by_ssh`). |
| `crates/irsync/src/config/clients.rs` (new) | Typed `ClientEntry`, `Clients` (`get`, `alias_of`, `resolve_peer`). |
| `crates/irsync/src/config/legacy_v1.rs` (new) | The stage 1 reader, frozen, filling the version 2 message. |
| `crates/irsync/src/config/reader.rs` | The version 2 reader, and `read`, which picks a reader. |
| `crates/irsync/src/config/comments.rs` | Places for `servers`/`clients`; a hook so `legacy_v1` can move `hosts` places. |
| `crates/irsync/src/config/writer.rs` | Writes `config_version`, `[servers.*]`, `[clients.*]`. |
| `crates/irsync/src/config/hosts.rs` | Deleted in Task 5. |
| `crates/irsync/src/manage/servers.rs`, `manage/clients.rs` (new) | Edits on the message for `server …` and `client …`. `manage/hosts.rs` is deleted in Task 5. |
| `crates/irsync/src/manage/mod.rs` | `ConfigLock` records what it read; `save` sets the version, prints conversion notes, guards against an old daemon. `PeerSpec` resolves against clients. |
| `crates/irsync/src/cli/mod.rs`, `src/main.rs` | `server` and `client` nouns; hidden `host`; lookups by role. |
| `crates/irsync/src/ssh/setup.rs`, `ssh/onboard.rs` | Write a client on the server, a server on the client. |
| `crates/irsync/tests/fixtures/config_v1/*.toml` + `*.v2.toml` (new) | Conversion corpus. |
| `crates/irsync/tests/config_upgrade.rs` (new) | Conversion, reader choice, snapshot equality, end to end. |

In this plan `pb` means `irsync_proto::config`.

---

### Task 1: The new tables in the message and the typed view

Additive: `Host` and `Config.hosts` stay until Task 5.

**Files:**
- Modify: `proto/irsync/config/config.proto`
- Create: `crates/irsync/src/config/names.rs`, `servers.rs`, `clients.rs`
- Modify: `crates/irsync/src/config/mod.rs`, `hosts.rs` (use `names.rs`), `file.rs`
- Test: `crates/irsync/tests/config_message.rs`

**Interfaces:**
- Produces:
  - `pb::ServerEntry { comment, key_comments, tail, name: String, endpoint_id: Vec<u8>, relay, relay_mode, congestion, pq_key_exchange, address, address_set, parallel, reconnect, dial_retries, dht, mdns, ssh, ssh_aliases, ssh_aliases_set }`: the dial keys exactly as on `Host`; `endpoint_id` is plain `bytes` (empty means missing).
  - `pb::ClientEntry { comment, key_comments, tail, name: String, client_id: Vec<u8> }`
  - `pb::Config`: `optional uint32 config_version`, `repeated ServerEntry servers`, `repeated ClientEntry clients`.
  - `config::names::{valid_alias(&str) -> bool, short_host_name(&str) -> Option<String>, invalid_name(&str) -> String}`
  - `config::servers::ServerEntry { endpoint_id: EndpointId, relay, relay_mode, congestion, pq_key_exchange, address, parallel, reconnect, dial_retries, dht, mdns, ssh, ssh_aliases }` (the `HostEntry` fields, `endpoint_id` not optional, no `client_id`); `Servers { path: PathBuf, entries: BTreeMap<String, ServerEntry> }` with `from_message(&[pb::ServerEntry], &Path) -> Result<Servers, ConfigError>`, `get(&str) -> Option<&ServerEntry>`, `by_ssh(&str) -> Result<Option<&str>, Vec<String>>`.
  - `config::clients::ClientEntry { client_id: EndpointId }`; `Clients { path, entries }` with `from_message(&[pb::ClientEntry], &Path)`, `get`, `alias_of(&EndpointId) -> Option<&str>` (first in lexicographic order), `resolve_peer(&str) -> Result<EndpointId, ConfigError>` (a name first, then an id).
  - `file::Config` gains `servers: Servers` and `clients: Clients`.

- [ ] **Step 1: Write the failing tests** in `config_message.rs`:

```rust
#[test]
fn servers_and_clients_build_the_typed_view() {
    let msg = pb::Config {
        servers: vec![pb::ServerEntry { name: "nas".into(), endpoint_id: id(1).as_bytes().to_vec(), ..Default::default() }],
        clients: vec![pb::ClientEntry { name: "nas".into(), client_id: id(2).as_bytes().to_vec(), ..Default::default() }],
        ..Default::default()
    };
    let c = Config::from_message(&msg, Path::new("irsync.toml")).unwrap();
    assert_eq!(c.servers.get("nas").unwrap().endpoint_id, id(1));
    assert_eq!(c.clients.get("nas").unwrap().client_id, id(2));
    assert_eq!(c.clients.alias_of(&id(2)), Some("nas"));
    assert_eq!(c.clients.alias_of(&id(1)), None);
    assert_eq!(c.clients.resolve_peer("nas").unwrap(), id(2));
}

#[test]
fn a_server_needs_an_endpoint_id_and_a_client_a_client_id() {
    let p = Path::new("irsync.toml");
    let s = pb::Config { servers: vec![pb::ServerEntry { name: "nas".into(), ..Default::default() }], ..Default::default() };
    assert_eq!(Config::from_message(&s, p).unwrap_err().to_string(), "irsync.toml: server \"nas\": needs an endpoint_id");
    let c = pb::Config { clients: vec![pb::ClientEntry { name: "lap".into(), ..Default::default() }], ..Default::default() };
    assert_eq!(Config::from_message(&c, p).unwrap_err().to_string(), "irsync.toml: client \"lap\": needs a client_id");
}

#[test]
fn names_are_checked_and_unique_per_table() {
    // "all", "*", "a:b", an id: invalid in both tables; the same name twice in one table: an error.
}
```

  Fill in the third test with one case per rule. The duplicate messages are `duplicate server name "x"` and `duplicate client name "x"`; an invalid name is `invalid name "all": must be non-empty, contain no ':' or '/', and be neither all, * nor an EndpointId`.

- [ ] **Step 2: Run them.** `cargo test -p irsync --test config_message servers_and_clients a_server_needs names_are`: fails, as `ServerEntry` doesn't exist yet.
- [ ] **Step 3: Add the messages and fields to `config.proto`.** Use new field numbers: `config_version = 15; servers = 16; clients = 17`.
- [ ] **Step 4: Move `valid_alias`, `short_host_name` to `names.rs`**, and add `invalid_name`. `hosts.rs` keeps `invalid_alias` (its own text) and re-exports nothing. Update `ssh/client.rs`, `ssh/setup.rs`, `ssh/onboard.rs` imports.
- [ ] **Step 5: Write `servers.rs` and `clients.rs`** from `hosts.rs`: `ServerEntry::from_message` is `HostEntry::from_message` without `client_id`, with messages prefixed `server {name:?}: `; `by_ssh` is `Hosts::by_ssh` where every entry has a daemon id. `ConfigError::UnknownAlias` gets a sibling `UnknownClient { name, path }`: `unknown client {name:?} (and not an EndpointId); clients are defined in {path}`.
- [ ] **Step 6: Run the tests**: they pass, and so does the whole `cargo test -q -p irsync`.
- [ ] **Step 7: Commit**: `Add servers and clients to the config message`.

---

### Task 2: Writer and comments for the new tables

Still additive: nothing reads `[servers]` yet, so the tests build messages by hand and use `comments::attach_with` (below) on hand-written text.

**Files:**
- Modify: `crates/irsync/src/config/writer.rs`, `comments.rs`
- Test: `crates/irsync/tests/config_message.rs`

**Interfaces:**
- Consumes: Task 1's message.
- Produces:
  - `writer::write`: `config_version = N` as the first top-level key when set; `[servers.NAME]` then `[clients.NAME]` after `[telemetry]`, before `[exports.*]` (`[hosts.*]` stays in between until Task 5). Server keys in the order of `ServerEntry`, client keys: `client_id`.
  - `comments::Place` and `comments::Spot` become `pub(crate)`. `comments::attach_with(text, path, msg, map: &dyn Fn(&pb::Config, Place) -> Place)` runs `map` on each place before it is resolved; `attach` is `attach_with` and the identity. `table()` knows `["servers", n]` (arrays `address`, `ssh_aliases`) and `["clients", n]`; `redirect` treats `servers` and `clients` as groups like `hosts`.

- [ ] **Step 1: Write the failing tests**: `writer_writes_servers_then_clients`, which builds a message with `config_version = Some(2)`, one server with `address` and `ssh`, one client, each with a table comment and a key comment, and compares the written text with:

```toml
config_version = 2

# the nas
[servers.nas]
endpoint_id = "<id 1>"
address = ["nas.lan:4433"]
ssh = "me@nas"

[clients.laptop]
client_id = "<id 2>" # my laptop
```

  Then `comments_of_new_tables_round_trip`: attach the comments of that text to a message holding the same values (built by hand), and check that it equals the one written.
- [ ] **Step 2: Run them**: they fail.
- [ ] **Step 3: Implement** the writer functions `server(&pb::ServerEntry) -> Keys` and `client(&pb::ClientEntry) -> Keys`, the `config_version` key, and the comment places.
- [ ] **Step 4: Run** the new tests and the whole `config_message` test file: they pass.
- [ ] **Step 5: Commit**: `Write servers and clients, and keep their comments`.

---

### Task 3: `legacy_v1`, the old layout into the new message

Not wired into loading yet: `legacy_v1::read` is called by its tests only.

**Files:**
- Create: `crates/irsync/src/config/legacy_v1.rs` (from `reader.rs` as it is now)
- Create: `crates/irsync/tests/fixtures/config_v1/` with, for each case, `NAME.toml` and `NAME.v2.toml`
- Create: `crates/irsync/tests/config_upgrade.rs`

**Interfaces:**
- Consumes: Task 1's message, Task 2's `comments::attach_with`.
- Produces:
  - `legacy_v1::read(text: &str, path: &Path) -> Result<Converted, ConfigError>`
  - `pub struct Converted { pub config: pb::Config, pub dropped: Vec<String> }`: `config.config_version == Some(2)`, `config.hosts` empty; `dropped` is one note per `[hosts.N]` that lost dial keys: `[hosts.N] had a client_id and no endpoint_id: dropped relay, address (they only apply to a server)`, naming the keys it had, in file order.

Conversion, for each `[hosts.N]` in file order:

| Old entry | New entries |
|---|---|
| `endpoint_id` (with or without `client_id`) | server `N`: `endpoint_id`, every dial key, `ssh`, `ssh_aliases` |
| `client_id` | client `N`: `client_id` |
| only `endpoint_id`, `N` named in any `rw`/`ro` of `[all]`, `[immediate]` or an export | also client `N` with `client_id` = that `endpoint_id`, and the comment line `# was the endpoint_id of [hosts.N]; kept as a client so the grants naming N still apply` above its table |
| only `client_id`, with dial keys | client `N`; the dial keys go into `dropped` |

Comments: the comment and tail of `[hosts.N]` go to server `N` when there is one, else to client `N`; the comments of the `client_id` key go to client `N`; other key comments go with their key, to the server. `legacy_v1` passes `attach_with` a `map` that does this on `Place`s, and sends a place of the group `["hosts"]` to the first entry made from the first host.

- [ ] **Step 1: Copy `reader.rs` to `legacy_v1.rs`.** Rename `read` to `read_v1_values` (private), keep every serde struct and every error text, and add `read` as above. Its `check_shape` refuses `servers`, `clients` and `config_version` as unknown keys, as the stage 1 reader did. Task 5 picks the reader before this one ever sees them.
- [ ] **Step 2: Write the corpus**, one pair per row of the table above, plus `both_ids.toml`, `grants_by_name_and_id.toml`, `comments_everywhere.toml` (a comment on every table, every key, every array entry), `shared_id.toml` (Review Focus 1), `immediate_only.toml` (Review Focus 2) and `group_comments.toml` (a bare `[hosts]` header with comments, Review Focus 3). Each `.v2.toml` is the text the writer must produce. It starts with `config_version = 2`.
- [ ] **Step 3: Write the tests** in `config_upgrade.rs`:

```rust
/// Every file of the corpus converts to the text beside it.
#[test]
fn v1_corpus_converts_to_its_v2_text() { /* for each NAME.toml: write(&legacy_v1::read(..)?.config) == NAME.v2.toml */ }

/// Every comment line of each old file is in the converted one.
#[test]
fn conversion_keeps_every_comment() {}

/// The grants of each old file, read the stage 1 way, equal those of the converted file.
#[test]
fn conversion_keeps_the_access() {}

#[test]
fn shared_id_keeps_its_grants() {}

#[test]
fn every_grant_list_counts_for_keep_the_access() {}

#[test]
fn group_comments_go_to_the_first_entry() {}

#[test]
fn dial_keys_of_a_client_only_host_are_named() {
    let c = legacy_v1::read("[hosts.phone]\nclient_id = \"<id>\"\nrelay = \"https://r.example/\"\naddress = [\"p:1\"]\n", p).unwrap();
    assert_eq!(c.dropped, ["[hosts.phone] had a client_id and no endpoint_id: dropped relay, address (they only apply to a server)"]);
}
```

  For `conversion_keeps_the_access`: the old access is computed as `Config::parse` of the old text gives it today (`Snapshot::from_files(&hosts, …)`, still available until Task 5). The new access is the same `Snapshot` over `Clients` built from the converted message. Compare the grants of every id named anywhere in the file. In Task 5, when `Hosts` goes, the old side becomes a set of expected `(id, Grants)` pairs written next to each corpus file, as `NAME.grants` lines `<id> <export>=rw|ro …`. Generate those files in this task from the old code, so the expectation is fixed before the old code goes.
- [ ] **Step 4: Run them**: they fail until `read` converts.
- [ ] **Step 5: Implement** the conversion and the comment map.
- [ ] **Step 6: Run** `cargo test -p irsync --test config_upgrade`: it passes.
- [ ] **Step 7: Commit**: `Read the old [hosts] layout into servers and clients`.

---

### Task 4: Edits on servers and clients

Additive: pure functions on the message, beside `manage/hosts.rs`.

**Files:**
- Create: `crates/irsync/src/manage/servers.rs`, `manage/clients.rs`
- Test: `crates/irsync/tests/manage_servers.rs`, `manage_clients.rs` (new; Task 5 deletes `manage_hosts.rs`)

**Interfaces:**
- Consumes: Task 1's message; `grants::{lists_naming, remove_named, rename_named}` as they are.
- Produces:
  - `servers::ServerFlags`: `HostFlags` renamed.
  - `servers::server_add(msg, name, id: EndpointId, flags: &ServerFlags) -> Result<Option<EndpointId>, ManageError>`: `host_add` on `msg.servers` (returns the previous id when it changed).
  - `servers::server_add_ssh_alias(msg, name, dest)`, `server_remove(msg, name) -> Result<(), ManageError>` (no grants to check), `server_rename(msg, old, new) -> Result<(), ManageError>` (touches no grant), `server_names(msg) -> Vec<String>`, `server_list(msg) -> Vec<String>` (`name: id`, then indented keys as `host_list` prints them).
  - `clients::client_add(msg, name, id) -> Result<Option<EndpointId>, ManageError>` (previous id when it changed), `client_remove(msg, name, force) -> Result<usize, ManageError>` and `client_rename(msg, old, new) -> Result<usize, ManageError>` (what `host_remove` and `host_rename` do with grants), `client_names(msg)`, `client_list(msg) -> Vec<String>` (`name: id`).
  - `ManageError`: `ServerExists(String)` `server {0:?} already exists`, `ClientExists(String)` `client {0:?} already exists`, `ClientInUse { name, lists }` with the text of `HostInUse` reworded for a client. `HostExists` and `HostInUse` go in Task 5.

- [ ] **Step 1: Port the tests** of `manage_hosts.rs` to the two new files, one test per behaviour: add creates and updates; an invalid name is refused; remove of a server ignores grants; remove of a granted client is refused without `force` and takes the grants with it with `force`; rename of a client renames grants; rename to an existing name is refused; rename to itself changes nothing; list output (`server_list` must not print `config_version`).
- [ ] **Step 2: Run them**: they fail.
- [ ] **Step 3: Implement** both files.
- [ ] **Step 4: Run them**: they pass.
- [ ] **Step 5: Commit**: `Add server and client edits on the config message`.

---

### Task 5: Switch over

The large step: version 2 becomes the file format, `Host` goes, every user moves to servers or clients. The tree compiles again only at Step 9. Run `cargo check -p irsync --all-features --tests` after each step from Step 4 on, to see what is left.

**Files:**
- Modify: `config.proto` (remove `Host`, `Config.hosts`), `config/reader.rs`, `comments.rs`, `writer.rs`, `file.rs`, `mod.rs`; delete `config/hosts.rs`
- Modify: `manage/mod.rs`, `grants.rs`, `config.rs`, `show.rs`, `exports.rs`; delete `manage/hosts.rs`
- Modify: `authz.rs`, `net/endpoint.rs`, `main.rs`, `ssh/setup.rs`, `ssh/onboard.rs`
- Tests: `config_files.rs`, `config_message.rs`, `authz_snapshot.rs`, `manage_*.rs`, `cli.rs`, `commands_e2e.rs`, `onboard_e2e.rs`, `ssh_setup.rs`, `net.rs`, `manage_doc.rs`, fixtures; delete `manage_hosts.rs`

**Interfaces:**
- Consumes: Tasks 1–4.
- Produces:
  - `reader::CONFIG_VERSION: u32 = 2`.
  - `reader::read(text, path) -> Result<Read, ConfigError>` with `pub struct Read { pub config: pb::Config, pub version: u32, pub dropped: Vec<String> }`. `version` is the file's own (1 for `legacy_v1`, else 2). `reader::read_values` keeps its role for the current layout.
  - `file::Config { settings, servers, clients, exports }`; `Config::parse` goes through `reader::read`.
  - `ConfigLock::load(&mut self) -> Result<pb::Config, ManageError>`, which records the `version` and `dropped` it read. `ConfigLock::save` sets `config_version = Some(CONFIG_VERSION)` and, when the file was version 1, prints each `dropped` note to stderr as `irsync: note: {note}` after a successful save. `load_message(path)` returns `reader::read(..)?.config`.
  - `Snapshot::from_files(clients: &Clients, exports: &Exports, immediate: bool)`; `grants::canonicalize(msg, &Clients)`; `PeerSpec::parse(who, &Clients, &Servers)`; `PeerSpec::written(&Clients)`; `show::*` over `&Clients`; `manage::canonicalize_without(msg, path, name)` over clients.
  - `net::endpoint::client_endpoint_opts(.., host: Option<&ServerEntry>, ..)`.

Which reader (`reader::read`), from a shape pass over the top-level keys:

| `config_version` | `[hosts]` | `[servers]`/`[clients]` | Reader or error |
|---|---|---|---|
| `2` | yes | any | error at `[hosts]`: `[hosts] is not read since config_version 2: hosts are now [servers.NAME] (to connect to) and [clients.NAME] (to let in)` |
| `2` | no | any | current |
| `1` | any | any | `legacy_v1` |
| absent | yes | no | `legacy_v1` |
| absent | yes | yes | error at `[hosts]`: `this file has [hosts] and also [servers] or [clients]: move each [hosts.NAME] into [servers.NAME] (its endpoint_id and how to reach it) or [clients.NAME] (its client_id)` |
| absent | no | any | current |
| above 2 | any | any | error at the key: `config_version N: this file was written by a newer irsync, which reads up to config_version 2; upgrade irsync` |
| not an integer, or below 1 | any | any | error at the key: `config_version: expected 1 or 2, found …` (the TOML type, or the number) |

- [ ] **Step 1: Write the failing tests** in `config_upgrade.rs`: `reader_choice` (one case per table row) and `bad_config_version_values` (`"2"`, `2.0`, `0`, `-1`, `3`; Review Focus 4). Add the current reader's own cases to `config_message.rs`: `[servers]`/`[clients]` read with their comments; `config_version` read; a server without `endpoint_id` and a client without `client_id` are errors at their line.
- [ ] **Step 2: The current reader.** In `reader.rs`: `RawFile` drops `hosts`, gains `config_version: Option<Spanned<toml::Value>>`, `servers` and `clients` (`BTreeMap<String, Spanned<RawServer>>`, `… Spanned<RawClient>`). Write `servers()` and `clients()` after `hosts()`, in name order for checks and file order for entries. The bare-table hint in `check_shape` becomes `unknown table {name:?}; servers live under [servers.{name}], clients under [clients.{name}]` (by `endpoint_id`/`client_id`), and the exports hint stays. Then `read` as in the table.
- [ ] **Step 3: Remove `Host`** from the proto and every `hosts` place in `comments.rs` and `writer.rs`. `legacy_v1` keeps its own `RawHost` and fills `servers`/`clients` only.
- [ ] **Step 4: Typed view.** `file::Config` drops `hosts`; delete `config/hosts.rs` (`HostEntry::peer_id` goes with it).
- [ ] **Step 5: Grants and authz.** Move `Snapshot::from_files`, `canonicalize`, `PeerSpec`, `show` and `canonicalize_without` to `Clients`. `PeerSpec::parse`: a name in clients is `Alias`. A name that is only in servers is `ManageError::NotAClient(name)`: `{name:?} is a server here, not a client; to grant that machine, add it as a client: irsync client add {name} ID`.
- [ ] **Step 6: `config get/set/unset/list`.** In `manage/config.rs`, `kind_of` drops `hosts.*` and knows `config_version` (read-only), `servers.NAME`, `servers.NAME.{the dial keys, endpoint_id}`, `clients.NAME` and `clients.NAME.client_id`. A `hosts` path is `ManageError::UnknownKey` with the text `unknown key {key:?}; hosts are now servers.NAME and clients.NAME`. `config set config_version …` is refused: `config_version is set by irsync`. Unsetting `clients.NAME` or its `client_id` is refused while grants name it, as `host remove` was; unsetting `servers.NAME` always goes through.
- [ ] **Step 7: Loading for edits.** `ConfigLock` as in Interfaces. Every `lock.load()` caller gets `let mut lock`.
- [ ] **Step 8: `main.rs`, `ssh/`, `net/endpoint.rs`.** Do the mechanical move only; the new commands come in Task 7, the SSH rules in Task 8.
  - `connect_for` and `resolve_target` look the name up in `servers`. A name that is in `clients` but not in `servers` is `unknown` for the flow and keeps today's "saved as a client only" wording (spec: the flow stays and writes a server beside the client). `by_ssh` searches servers. Errors that named `config.hosts.path` name `paths.config`.
  - `run_host` calls the new edits as a stopgap: `host add` calls `server_add`, plus `client_add` when `--client-id` is given. `host remove` removes the server and the client (the client with `force` as given). `host rename` renames both. `host list` prints `server_list` then `client_list`.
  - `peer_ids` and `note_moved_grants` use clients.
  - `onboard.rs`: `host_add` becomes `server_add`, and `saved_host`, `saved_id`, `aliases_of` and `saved_ssh` read `msg.servers`.
  - `setup.rs`: `host_set_client` becomes `client_add`, and `check_client_name` reads `clients` (and `servers` only for its error text).
- [ ] **Step 9: Tests.** The tree compiles again. Move every test that wrote `[hosts.…]` for a current file to `[servers.…]`/`[clients.…]`, and every expected written text to the writer's version 2 form. Keep each test's case. `config_files.rs`: its error cases stay unchanged (they go through `legacy_v1`). Cases that read `.hosts` assert on `.servers`/`.clients` of the converted file. In `conversion_keeps_the_access`, compare against the `NAME.grants` files from Task 3.
- [ ] **Step 10: Run** `cargo test -q --workspace --all-features --no-fail-fast`: it passes. Also `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- [ ] **Step 11: Commit**: `Split [hosts] into [servers] and [clients] in the config file`, with a body saying that old files are read by `legacy_v1` and rewritten as version 2 on the first save.

---

### Task 6: No conversion under an older daemon

**Files:**
- Modify: `crates/irsync/src/manage/mod.rs`, `crates/irsync/Cargo.toml` (`semver = { workspace = true }`, adding it to `[workspace.dependencies]` at the version `Cargo.lock` has), `Cargo.lock`, `fuzz/Cargo.lock`
- Test: `crates/irsync/tests/daemon_lock.rs` or `config_upgrade.rs`

**Interfaces:**
- Consumes: `daemon_lock::probe(state_dir) -> Option<LockInfo>`; `LockInfo.version` is the daemon's `LONG_VERSION`, whose first word is its package version.
- Produces:
  - `manage::READS_CONFIG_V2: &str = "0.1.0-beta.7"`: the first release that reads version 2. If stage 2 ships in a later release, the release bump changes it; say so in a comment and in the PR.
  - `ManageError::OldDaemon { state_dir: PathBuf, pid: u32, version: String }`: `the irsync daemon running for {state_dir} (pid {pid}, version {version}) cannot read the file this would write (config_version 2, read from irsync {READS_CONFIG_V2} on); restart the daemon with this irsync, then run the command again`. When the version is not known, the text has `version unknown` in its place.
  - In `ConfigLock::save`: when the file read was version 1, before anything is written, `probe(settings.state_dir)` of the typed message. A holder passes when the first word of its version equals this build's `CARGO_PKG_VERSION`, or parses as semver `>= READS_CONFIG_V2`. Anything else, `pid == 0` included, is `OldDaemon`, and the file stays as it was.

- [ ] **Step 1: Write the failing tests**: `save_of_a_v1_file_is_refused_under_an_old_daemon` (write a lock record with `version=0.1.0-beta.6 (…)` and hold it with a second open file, as `daemon_lock.rs` tests do: save fails, the file bytes are unchanged); `save_goes_through_with_no_daemon_or_a_current_one`; `unknown_daemon_version_refuses_conversion` (`pid=0`, empty record, `version=garbage`; Review Focus 5); `a_v2_file_is_saved_under_any_daemon`.
- [ ] **Step 2: Run them**: they fail.
- [ ] **Step 3: Implement** the check.
- [ ] **Step 4: Run them**, then `tools/sync-fuzz-lock.sh`; they pass.
- [ ] **Step 5: Commit**: `Refuse to rewrite an old config while an older daemon runs`.

---

### Task 7: The `server` and `client` commands

**Files:**
- Modify: `crates/irsync/src/cli/mod.rs`, `src/main.rs`
- Test: `crates/irsync/tests/cli.rs`, `commands_e2e.rs`

**Interfaces:**
- Consumes: Task 4's edits.
- Produces:
  - `Command::Server(ServerArgs)` with `ServerCmd::{Add(ServerAddArgs), Remove { name }, Rename { old, new }, List}`. `ServerAddArgs` holds the flags of `HostAddArgs` without `--client-id`. Its doc lines say: `Manage the servers this machine connects to (edits irsync.toml)`.
  - `Command::Client(ClientArgs)` with `ClientCmd::{Add { name, id }, Remove { name, force }, Rename { old, new }, List}`: `Manage the clients this machine's daemon lets in (edits irsync.toml)`.
  - `Command::Host` stays, hidden (`#[command(hide = true)]`), takes any trailing arguments, and exits 1 with `irsync host: hosts are now two things: `irsync server` (machines this one connects to) and `irsync client` (machines its daemon lets in)`.
  - Output: `server add` prints `NAME: ID` or `NAME: OLD -> ID`. `client add` prints the same, and on a changed id with grants naming it, `irsync client add: note: the grants that name NAME (in LISTS) now apply to ID (was OLD)`. `server remove`: `removed server NAME from PATH`. `client remove`: `removed client NAME[ and N grants] from PATH`. Renames: `renamed server OLD to NEW in PATH` and `renamed client OLD to NEW[; updated N grants] in PATH`.
  - `show` prints `servers:` (the `server list` lines) and `clients:` (the `client list` lines) where `hosts:` was.
  - The help for `grant add` and `grant remove`: WHO is `a client name, an EndpointId, a public key line or a .pub file`.

- [ ] **Step 1: Write the failing tests**: one per command and outcome above, and `host_is_gone_with_a_pointer` (`irsync host add x ID` exits 1 with that text and does not sync), and `grant_add_names_a_server_only_name` (the `NotAClient` text).
- [ ] **Step 2: Run them**: they fail.
- [ ] **Step 3: Implement**: replace `run_host` and `host_add` with `run_server` and `run_client`.
- [ ] **Step 4: Run** the whole `cargo test -q -p irsync`: it passes.
- [ ] **Step 5: Commit**: `Replace irsync host with irsync server and irsync client`.

---

### Task 8: The SSH setup by role

**Files:**
- Modify: `crates/irsync/src/ssh/setup.rs`, `ssh/onboard.rs`
- Test: `crates/irsync/tests/ssh_setup.rs`, `onboard_e2e.rs`

**Interfaces:**
- Produces: `check_client_name(msg, path, client, name)` with exactly these outcomes:
  - `client` already has a name in `clients`: ok (that name is used).
  - `name` fails `short_host_name`: today's error.
  - `clients` has `name` with another id: `the name {name:?} is already used on this server, as [clients.{name}] for the client {short id}. If that client is gone, remove it there with `irsync client remove {name} -f`; else rename this machine`.
  - Grants name `name` and `clients` has no `name`: `grants on this server ({lists}) name {name:?}, and no [clients.{name}] says who that is. They are left from a client that was removed, or name a server; take {name:?} out of those lists there, or rename this machine`.
  - Else ok. A server named `name` does not matter.
- `grant_client` calls `client_add` and grants the name immediate access as before. The stderr notes about which id a host's grants follow are gone (`main.rs` `host add` notes went in Task 7).

- [ ] **Step 1: Rewrite the tests** in `ssh_setup.rs` for these five outcomes, with a sixth: `a_server_of_the_same_name_is_no_obstacle` (the server has `[servers.laptop]`; a client calling itself `laptop` gets `[clients.laptop]` beside it).
- [ ] **Step 2: Run them**: the changed texts fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -q -p irsync --test ssh_setup --test onboard_e2e`: they pass.
- [ ] **Step 5: Commit**: `Let the SSH setup write a client on the server and a server on the client`.

---

### Task 9: An old file through a running daemon

**Files:**
- Test: `crates/irsync/tests/config_upgrade.rs`. Build the daemon as `autoexport.rs` does.

- [ ] **Step 1: Write the test** `a_daemon_on_a_v1_file_converts_it_and_still_lets_clients_in`:
  1. The daemon starts on a version 1 file with `[immediate] rw = ["laptop"]` and `[hosts.laptop] client_id = <client>`, `[daemon] immediate = true`, and `[hosts.nas] endpoint_id = <other>`, granted on an export by name with no `client_id`.
  2. The client `<client>` pushes to a path no export covers: the daemon adds an automatic export.
  3. The file now starts with `config_version = 2` and has `[clients.laptop]`, `[servers.nas]` and `[clients.nas]`, plus the automatic export.
  4. A second push by the same client still works, and `Snapshot` of the new file equals that of the old one for both ids.
- [ ] **Step 2: Run it.** It should pass on Tasks 1–8. If it fails, fix the cause in the task's code it points to, not in the test.
- [ ] **Step 3: Commit**: `Test that a daemon on an old config converts it and keeps its clients`.

---

### Task 10: The book, the full check, the pull request

**Files:**
- Modify: `book/src/reference/configuration.md`, `management.md`, `command-line.md`, `remote-addresses.md`, `files-and-state.md`; `book/src/explanation/identity-and-trust.md`; `book/src/tutorial/first-sync.md`; `book/src/how-to/ssh-setup.md`, `run-the-daemon.md`, `direct-and-relay.md`, `troubleshooting.md`; `packaging/man/` if it names hosts.

- [ ] **Step 1: Update the pages.** Every `[hosts.N]` becomes `[servers.N]` or `[clients.N]` by what the example does, and every `irsync host …` becomes `irsync server …` or `irsync client …`. Configuration gets a short part, "Config versions", on `config_version`, on what happens to an old file (read as before; rewritten on the first change; a name granted by its daemon id becomes a client too, with a comment; dial keys of a client-only entry are dropped and named), and on the older daemon that blocks the rewrite. Write it in the book's style: short sentences, plain words.
- [ ] **Step 2: Check.** `rg -n "\[hosts|irsync host|hosts\." book/src packaging` finds nothing, except the Config versions part. `python3 -m unittest discover -s tools/tests` passes. `tools/build-man.sh "$SCRATCH"` builds.
- [ ] **Step 3: Full check.** `tools/ci.sh check` passes.
- [ ] **Step 4: Commit**: `Describe servers, clients and config versions in the book`.
- [ ] **Step 5: Push and open the PR** against `main`, with `Closes #202`. Its description says, for the release notes: under "Upgrading", that the first change to the config rewrites it as version 2, which older irsync cannot read, and that a running older daemon blocks the rewrite until it is restarted; under "New", the `server` and `client` commands, and that `irsync host` is gone; that `READS_CONFIG_V2` must match the release it ships in.
