# Admin service: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A client whose entry has `admin = "rw"` manages a server's exports, clients and grants over `irsync/1` (`admin = "ro"` reads them), with transactions, through `irsync remote SERVERNAME COMMAND`.

**Architecture:** `admin` is a third right in `config::clients::RIGHTS`. A new `Hello` direction `ADMIN` turns the connection's control stream into an admin session: the client sends changes, the server collects them, and `COMMIT` applies them all-or-nothing under the config lock with the same `manage` functions the local commands use (a pure engine in `manage/admin.rs`, driven by the daemon). Reads answer at once with structured data the CLI prints with the local printers.

**Tech Stack:** Rust, prost (wire messages in `proto/irsync/v1`, encoded; config messages in `proto/irsync/config`, never encoded), iroh QUIC streams, clap.

**Spec:** `docs/superpowers/specs/2026-11-24-admin-service-design.md` (read its "After #284" section).

## Global Constraints

- `admin = "rw"`: read, and change exports, clients and grants. `admin = "ro"`: read only. Left out: none. Only named clients hold it.
- `PROTOCOL_VERSION` goes from 6 (unreleased on main) to 7. No backward compatibility (no users yet).
- A transaction is all or nothing: a NACK means the config file is unchanged on disk.
- The config lock is held only while a commit is applied; a commit waits for it up to `authz::LOCK_WAIT` (2 s), then NACKs "busy".
- Checks run on the server at commit time, against the config loaded under the lock, including the committing client's own `admin = "rw"`. The final result must still give the committing client `admin = "rw"` (no removing or downgrading oneself remotely).
- Reads need `admin` `ro` or `rw` at the time of the read, answer from the saved config, and do not see pending changes.
- At most 100 changes per transaction (`ADMIN_MAX_CHANGES`); one more drops the transaction and answers NACK. An admin session idle for 10 minutes (`ADMIN_IDLE`) is closed. Closing the stream drops pending changes.
- A client without `admin` that sends an ADMIN Hello gets the plain refusal `Code::UNAUTHORIZED`, message `not allowed`: nothing about the server.
- NACK to an `admin = "rw"` client names the change (1-based) and the reason; change 0 means the whole transaction (busy, not allowed, self-protection, too many changes).
- Audit: one `info` line per applied change with the client's name; one `warn` line per NACK or refused session, with the name (else the short id) and no detail. Reads are not logged.
- Last commit wins; no version check on commit.
- Remote commands: `export list|add|remove`, `grant list|add|remove`, `client list|add|remove|rename|set`, `show`, `config get KEY`, `config list`. Not remote: `config set/unset`, `export update`, `id *`, `server *`.
- `irsync remote` sends one change and COMMIT per call; it dials with `connect_for` and never sets a server up.
- The SSH setup sets `admin = "rw"` together with `all_paths = "rw"`.
- Plain words in messages and the book; `~/...` for home paths; example hosts `myclient`/`myserver`; no attribution lines in commits.
- `cargo test -q --workspace --all-features --no-fail-fast` passes after every task; `tools/ci.sh check` before the push.

## Review Focus

1. A transaction whose last change fails leaves the file byte-for-byte unchanged and applies none of the earlier changes. Test in task 3.
2. A client that loses `admin` between opening the session and committing gets NACK 0 and nothing changes; a read after losing it is refused. Tests in tasks 3 and 4.
3. Removing, renaming or lowering the committing client itself (`client remove`, `client rename`, `client set me admin=ro`, `client add me OTHERID`) is NACKed. Test in task 3.
4. A client with only `admin` (no export grants) gets past the connection gate; a client with no rights at all is still closed before the Hello. Test in task 1.
5. An `export add` path that is relative, or that the local shell already expanded (`/home/me/Photos` when the local home is `/home/me`), is refused on the client with a hint to write `'~/Photos'`; the server refuses a relative path too. Tests in tasks 3 and 5.

---

### Task 1: The `admin` right

**Files:**
- Modify: `proto/irsync/config/config.proto` (`ClientEntry`: `optional Access admin = 8;`)
- Modify: `crates/irsync/src/config/clients.rs` (`Right::Admin`, `RIGHTS` grows to 3, `ClientEntry.admin`, the matches in `Right::of`, `Right::field`, `Right::field_mut`, `ClientEntry::from_message`)
- Modify: `crates/irsync/src/authz.rs` (`Grants.admin`, `is_empty`, `Snapshot::from_files`)
- Modify: `crates/irsync/src/ssh/setup.rs` (`grant_client` also sets `admin = "rw"`)
- Modify: `book/src/reference/configuration.md` (Rights on a client), `book/src/explanation/threat-model.md` (`admin = "rw"` is the whole account, like `all_paths = "rw"`), `book/src/reference/management.md` (`client set` takes `admin`)
- Test: `crates/irsync/tests/config_files.rs`, `crates/irsync/tests/manage_clients.rs`, `crates/irsync/tests/authz_snapshot.rs`, `crates/irsync/tests/daemon_auth.rs`, `crates/irsync/tests/ssh_setup.rs`

**Interfaces:**
- Produces: `Right::Admin` (key `"admin"`); `ClientEntry.admin: Option<Access>`; `Grants.admin: Option<Access>` (from the client entries keyed by id, `rw` beats `ro`); `Snapshot::admin(&self, id: &EndpointId) -> Option<Access>`.

The SSH setup's `grant_client` returns true when it changed either right (it sets `all_paths = "rw"` and `admin = "rw"`); false only when both were already `"rw"`. On a recheck, a client that lacks either is reported `Missing` and nothing is saved.

- [ ] **Step 1: Write the failing tests**
  - `config_files.rs` `admin_is_read_and_written_back`: `admin = "ro"` round-trips, after `all_paths` in key order; `admin = "yes"` is refused with `client "laptop": admin must be "rw" or "ro"`.
  - `manage_clients.rs` `client_set_takes_admin`: `parse_setting("admin=rw") == (Right::Admin, Some(ReadWrite))`; `client_list` shows ` admin=rw`.
  - `authz_snapshot.rs` `admin_alone_is_not_empty`: a client with only `admin = "ro"` → `!grants.is_empty()`, `grants.admin == Some(ReadOnly)`, no export access.
  - `daemon_auth.rs` `a_client_with_only_admin_passes_the_gate`: it is not closed before the Hello (a LIST_EXPORTS Hello gets an empty `ExportList`); a client with no rights is still closed `unauthorized`.
  - `ssh_setup.rs` `setup_sets_admin_with_all_paths`: after `run`, the client has `all_paths = "rw"` and `admin = "rw"`; a second run reports `Already`.
- [ ] **Step 2: Run them and see them fail.** `cargo test -q -p irsync --all-features --test config_files --test manage_clients --test authz_snapshot --test daemon_auth --test ssh_setup` → compile errors on `Right::Admin` / `admin`.
- [ ] **Step 3: Implement.** Update the book pages listed.
- [ ] **Step 4: Run all tests.** `cargo test -q --workspace --all-features --no-fail-fast` → PASS.
- [ ] **Step 5: Commit** `Add the admin right on the client entry` (why: the admin service checks it).

---

### Task 2: The wire messages and protocol version 7

**Files:**
- Create: `proto/irsync/v1/admin.proto`
- Modify: `proto/irsync/v1/common.proto` (`Hello.Direction`: `ADMIN = 4;` with a comment: the control stream becomes an admin session; `path` empty)
- Modify: `crates/irsync-proto/src/lib.rs` (`PROTOCOL_VERSION = 7`, include the new file in the build as the others are)
- Modify: `crates/irsync-proto/build.rs` if it lists proto files; `crates/pbwalk` if it walks every message type
- Modify: `crates/irsync/src/daemon.rs`: an ADMIN Hello is answered `UNAUTHORIZED` "not allowed" for now (task 4 serves it), and does not bump the test-hooks `hellos` counter (like LIST_EXPORTS and PROBE)
- Test: `crates/irsync-proto/tests/roundtrip.rs`, `crates/irsync-proto/tests/golden_bytes.rs`

**Interfaces:**
- Produces (package `irsync.v1`), in `admin.proto`:

```proto
message AdminRequest { oneof msg { Change change = 1; Commit commit = 2; Read read = 3; } }
message Commit {}
message Change {
  oneof op {
    ExportAdd export_add = 1; ExportRemove export_remove = 2;
    GrantAdd grant_add = 3; GrantRemove grant_remove = 4;
    ClientAdd client_add = 5; ClientRemove client_remove = 6;
    ClientRename client_rename = 7; ClientSet client_set = 8;
  }
}
message ExportAdd { string target = 1; bool read_only = 2; bool warm_index = 3; } // "[NAME:]PATH", PATH absolute or ~
message ExportRemove { string name = 1; }
message GrantAdd { string export = 1; string who = 2; bool ro = 3; } // who: a client name, an EndpointId or a public key line
message GrantRemove { string export = 1; string who = 2; }          // "all" as in the CLI
message ClientAdd { string name = 1; bytes id = 2; }
message ClientRemove { string name = 1; bool force = 2; }
message ClientRename { string old = 1; string new = 2; }
message ClientSet { string name = 1; repeated string settings = 2; } // "KEY=VALUE" as typed
message Read {
  oneof what {
    ReadView view = 1;     // exports, clients, daemon facts: for export list, grant list, client list, show
    ReadConfig config = 2; // config get KEY / config list (empty key)
  }
}
message ReadView {}
message ReadConfig { string key = 1; }
message AdminReply { oneof msg { Ack ack = 1; Nack nack = 2; View view = 3; ConfigText config = 4; Error refused = 5; } }
message Ack { repeated string results = 1; } // one line per change, what the local command prints
message Nack { uint32 change = 1; string reason = 2; } // change 1-based; 0: the whole transaction
message View {
  bytes daemon_id = 1;
  bool allow_all_paths = 2;
  repeated ExportView exports = 3;
  repeated ClientView clients = 4;
}
message ExportView { string name = 1; string path = 2; bool read_only = 3; bool warm_index = 4; bool fsync = 5; bool is_auto = 6; repeated string rw = 7; repeated string ro = 8; }
message ClientView { string name = 1; bytes id = 2; map<string, string> rights = 3; } // key -> "rw" | "ro", only set rights
message ConfigText { repeated string lines = 1; }
```

- [ ] **Step 1: Write the failing tests.** `roundtrip.rs` `admin_messages_round_trip`: an `AdminRequest` with each `Change` variant, a `Commit` and each `Read` encodes and decodes equal; same for `AdminReply` variants. `golden_bytes.rs`: one golden for `AdminRequest{commit}` and one for `Nack{change: 2, reason: "x"}`, and the Hello golden with `protocol_version 7`.
- [ ] **Step 2: Run them and see them fail.** `cargo test -q -p irsync-proto` → unresolved types.
- [ ] **Step 3: Implement**; update every test that pins `5`.
- [ ] **Step 4: Run all tests** → PASS.
- [ ] **Step 5: Commit** `Add the admin wire messages; protocol version 7`.

---

### Task 3: The admin engine (pure, no network)

**Files:**
- Create: `crates/irsync/src/manage/admin.rs` (and `pub mod admin;` in `manage/mod.rs`)
- Test: `crates/irsync/tests/manage_admin.rs`

**Interfaces:**
- Consumes: `Right::Admin`, `Snapshot`-free lookups via `Clients::from_message` (task 1); the wire types (task 2); existing `manage::{exports, grants, clients, config, show}` functions.
- Produces:
  - `pub struct Committer { pub id: EndpointId }`
  - `pub fn admin_of(msg: &pb::Config, path: &Path, id: &EndpointId) -> Option<Access>` — the highest `admin` over client entries with that id.
  - `pub fn apply(msg: &mut pb::Config, path: &Path, home: Option<&Path>, who: &Committer, changes: &[v1::Change]) -> Result<Vec<String>, v1::Nack>` — checks `admin_of == Some(ReadWrite)` first (else NACK 0 `not allowed`), applies the changes in order on `msg`, then checks the end result (else NACK 0 `this would take away your own admin right`); on any NACK the caller drops `msg`. Returns one result line per change.
  - `pub fn view(msg: &pb::Config, daemon_id: EndpointId, allow_all_paths: bool) -> v1::View`
  - `pub fn config_text(msg: &pb::Config, key: &str) -> Result<v1::ConfigText, String>` (empty key: `config list`)
  - `pub fn summary(change: &v1::Change) -> String` — the audit text, such as `added export photos at ~/Photos`, `set laptop admin=ro`.

Each change calls what the local command calls (see `crates/irsync/src/main.rs` `run_export`, `run_grant`, `run_client`): `exports::parse_add_target(target, home)` after refusing a PATH that is not absolute and does not start with `~` (reason `the path must be absolute or start with ~`), then `exports::add_export`; `grants::grant_add` / `grant_remove` with `PeerSpec::parse` against the message's clients and servers (and the same refusals of `all` / `immediate` as the CLI); `clients::client_add` / `client_remove` (canonicalize first, as the CLI does) / `client_rename` / `parse_setting` + `client_set`. Result lines are the stderr lines the local commands print. A failing change's reason is its `ManageError` text.

- [ ] **Step 1: Write the failing tests** (`manage_admin.rs`, all on a message built from a TOML string):
  - `a_transaction_applies_every_change_in_order`: `export add photos:/<tmpdir>` then `grant add photos laptop --ro` → `Ok` with 2 lines; the message has the export with `ro = ["laptop"]`.
  - `a_failing_last_change_applies_nothing`: `export add a:/<tmpdir>`, `client remove nobody` → `Err(Nack{change: 2, ..})` whose reason names `nobody`; the caller's copy is untouched (call `apply` on a clone and compare the original) (Review Focus 1).
  - `a_client_without_admin_rw_is_not_allowed`: committer with `admin = "ro"` → `Nack{change: 0, reason: "not allowed"}`; with no admin → the same.
  - `the_committer_cannot_take_its_own_admin_away`: each of `client remove me --force`, `client rename me other`, `client set me admin=ro`, `client set me admin=none`, `client add me <other id>` → `Nack{change: 0}` with `this would take away your own admin right`; `client set me all_paths=ro` is fine (Review Focus 3).
  - `export_add_needs_an_absolute_or_home_path`: `photos:Photos` → NACK 1 with `the path must be absolute or start with ~`; `photos:~/Photos` with `home` = a temp dir holding `Photos` → ok, path written as `~/Photos` (Review Focus 5).
  - `view_lists_exports_and_clients_with_rights`: `View` has the export with its `rw`/`ro` and each client with `rights` only for set keys.
  - `config_text_gets_and_lists`: `config_text(msg, "daemon.allow_all_paths")` → the same line `irsync config get` prints; `""` → the `config list` lines.
- [ ] **Step 2: Run them and see them fail.** `cargo test -q -p irsync --all-features --test manage_admin` → unresolved `manage::admin`.
- [ ] **Step 3: Implement.** Factor what `main.rs` does around the manage calls (parsing WHO, canonicalizing before `client remove`/`rename`, the result lines) into functions both `main.rs` and `admin.rs` call, rather than copying it; `main.rs` keeps its behaviour and output.
- [ ] **Step 4: Run all tests** → PASS (the existing `commands_e2e.rs` pins the local output).
- [ ] **Step 5: Commit** `Apply admin transactions all or nothing`.

---

### Task 4: The daemon serves admin sessions

**Files:**
- Create: `crates/irsync/src/daemon/admin.rs` if `daemon.rs` is a single file, else a sibling module `crates/irsync/src/admin_session.rs` (one module: the session loop)
- Modify: `crates/irsync/src/daemon.rs` (dispatch the ADMIN Hello to it after `version_check`)
- Test: `crates/irsync/tests/admin_session.rs` (drives a real daemon over the same helpers as `daemon_auth.rs`)

**Interfaces:**
- Consumes: `manage::admin::{admin_of, apply, view, config_text, summary}` (task 3); `Snapshot::admin` (task 1); `authz::LOCK_WAIT`; `manage::lock_config`.
- Produces: `pub const ADMIN_MAX_CHANGES: usize = 100;` `pub const ADMIN_IDLE: Duration = Duration::from_secs(600);`; `async fn serve_admin(shared: Arc<Shared>, id: EndpointId, send: SendStream, recv: RecvStream, tap: StreamTap) -> Result<()>`.

Loop: read `AdminRequest` with `ADMIN_IDLE` as the read timeout (timeout or EOF: drop pending, close). `Change`: push; the 101st → NACK 0 `too many changes in one transaction (at most 100)` and clear. `Commit`: in `spawn_blocking`, take the lock (wait up to `LOCK_WAIT`, else NACK 0 `busy: the config is locked, try again`), `load`, `apply` on the loaded message, `save` only on `Ok`; reply `Ack` or `Nack`; log each change with `summary` at info (`admin: {name} {summary}`) or one warn line on NACK; clear. `Read`: `Config::load` fresh, check `admin_of` is `ro` or `rw` (else reply `refused` UNAUTHORIZED `not allowed`, warn, close), reply `View` or `ConfigText`. ADMIN Hello from a key whose `Snapshot::admin` is none: reply `refused` and close, warn line. The NACK reason for change N > 0 goes only to a key with `admin = "rw"` (it is, by then).

- [ ] **Step 1: Write the failing tests** (`admin_session.rs`):
  - `an_admin_adds_an_export_and_a_grant_in_one_commit`: two changes, COMMIT → `Ack` with 2 lines; the server's file has both.
  - `a_nack_leaves_the_file_unchanged`: second change fails → `Nack{change: 2}`; file bytes unchanged.
  - `the_session_goes_on_after_ack_and_nack`: NACK, then a good change + COMMIT on the same stream → `Ack`.
  - `closing_without_commit_changes_nothing`.
  - `a_client_without_admin_is_refused`: a key with only an export grant → `refused` UNAUTHORIZED `not allowed`, stream closed.
  - `admin_ro_reads_but_cannot_commit`: `Read{view}` → `View`; a change + COMMIT → `Nack{change: 0, reason: "not allowed"}`.
  - `losing_admin_mid_session`: open session; edit the server file to drop the client's admin; COMMIT → `Nack{change: 0}`; a following Read → `refused` (Review Focus 2).
  - `too_many_changes_drop_the_transaction`: 101 changes → `Nack{change: 0}` mentioning `100`; COMMIT afterwards with nothing pending → `Ack` with no lines; file unchanged.
  - `a_held_lock_makes_commit_busy`: hold `lock_config` on the server file from the test; COMMIT → `Nack{change: 0, reason starts with "busy"}` after about `LOCK_WAIT`.
  - `an_idle_session_is_closed` on tokio's paused clock if the helpers allow it; else mark `#[ignore = "slow: idle timeout"]` with `ADMIN_IDLE` read from the constant.
- [ ] **Step 2: Run them and see them fail.** `cargo test -q -p irsync --all-features --test admin_session` → refused "not allowed" everywhere (task 2's stub).
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run all tests** → PASS.
- [ ] **Step 5: Commit** `Serve admin sessions in the daemon`.

---

### Task 5: `irsync remote SERVERNAME COMMAND`

**Files:**
- Modify: `crates/irsync/src/cli/mod.rs` (`Command::Remote(RemoteArgs)`; `RemoteArgs { server: String, #[command(subcommand)] cmd: RemoteCmd }`; `RemoteCmd { Export(RemoteExportCmd), Grant(GrantArgs), Client(ClientArgs), Show, Config(RemoteConfigCmd) }`; `RemoteExportCmd { Add(ExportAddArgs without --warm-index if it needs the local daemon key, else as is), Remove{name}, List }`; `RemoteConfigCmd { Get{key}, List }`)
- Create: `crates/irsync/src/remote.rs` (dial, send, print)
- Modify: `crates/irsync/src/main.rs` (dispatch; reuse `connect_for`)
- Test: `crates/irsync/tests/remote_e2e.rs` (real daemon and real CLI binary, as `commands_e2e.rs` / `sync_e2e.rs` do)

**Interfaces:**
- Consumes: wire types (task 2); the daemon (task 4); `connect_for`, `describe_dial`, `never_reached` in `main.rs`; the local printers `manage::show::export_blocks`, `manage::grants::{list_all, list_export, list_peer}`, `manage::clients::client_list`.
- Produces: `pub async fn run_remote(c: &Connected, req: RemoteRequest) -> anyhow::Result<RemoteOutcome>` where `RemoteRequest` is one change or one read; and `fn view_to_config(v: &v1::View) -> pb::Config` so the printers run unchanged.

Behaviour:
- Each call: open the connection, send `Hello{direction: ADMIN}`, then one `Change` + `Commit` (print the `Ack` lines to stderr, exit 0; a `Nack` prints `irsync remote: <reason>` and exits 1) or one `Read` (print like the local command to stdout).
- `grant add WHO` / `client add NAME ID`: a `.pub` file or key line is read on the client and sent as the key line / id; nothing else of the client's files is sent.
- `export add`: PATH must start with `/` or `~`; one that starts with the local home directory is refused before dialling: `/home/me/Photos looks like your own home: the shell expanded ~. Quote it: irsync remote myserver export add 'photos:~/Photos'`.
- `show`: the daemon identity, the clients with their rights, the exports.
- `refused` UNAUTHORIZED → `irsync remote: myserver does not let this client manage it (it needs admin = "rw" or "ro" in its [clients] entry)`, exit 1.
- `VERSION_MISMATCH` whose message says the daemon speaks a version below 7 → `myserver runs an older irsync without remote admin: update it there`; otherwise the usual version message.
- `irsync remote --help` lists the remote commands.

- [ ] **Step 1: Write the failing tests** (`remote_e2e.rs`):
  - `remote_export_add_then_list`: `irsync remote myserver export add 'photos:~/Photos'` → exit 0; `irsync remote myserver export list` prints what `irsync export list` prints on the server.
  - `remote_grant_list_matches_local`: output equals the server-side `irsync grant list` byte for byte.
  - `remote_client_set_admin_on_self_is_refused`: exit 1, stderr has `own admin right`.
  - `remote_without_admin_is_refused`: exit 1 with the message above.
  - `an_expanded_home_path_is_refused_before_dialling` (Review Focus 5): with HOME set to a temp dir, `export add photos:<that dir>/Photos` exits 1 with `the shell expanded ~` and the daemon logs no connection.
  - `config_set_is_not_a_remote_command`: `irsync remote myserver config set x y` → clap error.
- [ ] **Step 2: Run them and see them fail.** `cargo test -q -p irsync --all-features --test remote_e2e` → unknown subcommand `remote`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run all tests** → PASS.
- [ ] **Step 5: Commit** `Add irsync remote for managing a server from a client`.

---

### Task 6: The book

**Files:**
- Create: `book/src/how-to/remote-admin.md` (and its line in `book/src/SUMMARY.md` next to the other how-tos)
- Modify: `book/src/reference/command-line.md` (`irsync remote`), `book/src/reference/management.md` (which commands work remotely), `book/src/explanation/threat-model.md` (the admin service: who can reach it, the audit log), `book/src/how-to/ssh-setup.md` (the setup also gives `admin = "rw"`)

The how-to: give a client `admin` (`irsync client set myclient admin=rw` on the server, or the SSH setup), then `irsync remote myserver export add 'photos:~/Photos'`, `grant add`, `show`; why the quotes; what `ro` allows; that changes are logged on the server; that the server must run the same irsync version.

- [ ] **Step 1:** Write the pages.
- [ ] **Step 2:** `python3 -m unittest discover -s tools/tests` → OK.
- [ ] **Step 3:** `tools/ci.sh check` → passes.
- [ ] **Step 4:** Commit `Describe remote admin in the book`.
