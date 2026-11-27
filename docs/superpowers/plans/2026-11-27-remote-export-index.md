# Remote export set, rename and indexing: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A client with `admin = "rw"` can run `export set` and `export rename` remotely and start hashing an export's files on the server; any admin client can poll which indexing runs are going on and how the last one of each export ended. `irsync remote` gets the same commands.

**Architecture:** Two new `Change` ops go through the existing engine (`manage/admin.rs`). A new top-level admin request, `StartIndex`, is answered at once outside transactions; the daemon runs `warm::index_root` in the background and records each run in a run table (`daemon/indexing.rs`), which a new read, `ReadIndexing`, answers from. Runs started by `index_at_start` go through the same table. The CLI's one-shot `converse` learns to keep a session open, so `irsync remote S index NAME` can start a run and poll it.

**Tech Stack:** Rust, prost (`proto/irsync/v1/admin.proto`), iroh QUIC streams, tokio, clap.

**Spec:** `docs/superpowers/specs/2026-11-26-remote-export-index-design.md`. Background: `docs/superpowers/specs/2026-11-24-admin-service-design.md`.

## Global Constraints

- `PROTOCOL_VERSION` goes from 8 to 9 (`crates/irsync-proto/src/lib.rs`, and the pin in `crates/irsync-proto/tests/golden_bytes.rs`). No backward compatibility.
- `export set` and `export rename` are changes: in a transaction, all or nothing, `admin = "rw"`, the ACK line is what the local command prints.
- `StartIndex` needs `admin = "rw"` in the saved config, checked at the request; it never holds the config lock and never runs inside a transaction. It sees only the saved config: an export added but not committed is "no export".
- `ReadIndexing` needs `admin = "ro"` or `"rw"`, like the other reads, and answers from memory.
- At most one run per export **path** at a time. A second start answers "already running" with the run in progress.
- The run table keeps only the latest run per path, in memory, since the daemon started.
- The indexing status carries counts only, never the path of a file inside an export, for `ro` and `rw` alike. Failed files are named only in the daemon's log, one `warn` line each.
- A run goes on when the session or connection ends, and when its export is removed or renamed; it stops only when done or when the daemon stops.
- An `ro` client's `StartIndex`, an unknown export, or an index that cannot be opened is answered with `Refused`; the session stays open.
- Audit: `admin: WHO started indexing export NAME` (info) on a start; on the end of every run (requested or at start), the existing `export indexed` info line with its counts.
- No cap on runs across exports (my call in the plan): each run already uses `HASH_WORKERS`.
- Plain words in messages and the book; `~/...` for home paths; example hosts `myclient`/`myserver`; no attribution lines in commits.
- `cargo test -q --workspace --all-features --no-fail-fast` passes after every task; `tools/ci.sh check` and `tools/ci.sh typecheck` before the push. `reference/management.md` and `reference/command-line.md` feed the man page: run `python3 -m unittest discover -s tools/tests` after changing them.

## Review Focus

1. A `StartIndex` for an export whose index is opened for the first time, with `index_at_start = true`, must not start a second run beside the requested one; the requested run is the one reported, started by the client. Test in task 4.
2. A run whose folder is removed while it goes on (or that fails as a whole) must end in the table with `ended_at` set and `error` filled, never stay "running" forever. Test in task 3.
3. The CLI polling a run that is replaced meanwhile (another client starts a new run on the same path after it ended) must stop at the first poll that shows the path ended or started after its own run: match on path and `started_at`. Test in task 5.
4. Ctrl-C during `irsync remote S index NAME` leaves the run going and says so, exit 130. Test in task 5 (unix: send SIGINT to the child).
5. A file name with a newline or control character in a failed path must not split the daemon's log line: escape it as `engine::summary` does. Test in task 3.

---

### Task 1: `export set` and `export rename` as changes; protocol 9

**Files:**
- Modify: `proto/irsync/v1/admin.proto` (`Change.op`: `ExportSet export_set = 9; ExportRename export_rename = 10;`, messages below)
- Modify: `crates/irsync-proto/src/lib.rs:31` (`PROTOCOL_VERSION = 9`), `crates/irsync-proto/tests/golden_bytes.rs:12`
- Modify: `crates/irsync/src/manage/admin.rs` (`apply_one`, `summary`)
- Test: `crates/irsync/tests/manage_admin.rs`

**Interfaces:**
- Produces (proto): `message ExportSet { string name = 1; repeated string settings = 2; } // "KEY=VALUE" as typed`, `message ExportRename { string old = 1; string new = 2; }`; Rust `Op::ExportSet(v1::ExportSet)`, `Op::ExportRename(v1::ExportRename)`.

- [ ] **Step 1: Write the failing tests** in `manage_admin.rs`:
  - `export_set_and_rename_apply_as_the_local_commands`: a config with export `photos` at `~/Photos` and the committer as admin; changes `ExportSet{name:"photos", settings:["read_only=true"]}` then `ExportRename{old:"photos", new:"pictures"}`. Results: `["photos: read_only=true index_at_start=false fsync=false", exports::renamed_line("photos", "pictures", path)]`; the message has one export `pictures` with `read_only == Some(true)` and the same path.
  - `a_bad_export_setting_nacks_its_change`: changes `[export_add ok, ExportSet{photos, ["colour=red"]}]` → `Nack{change: 2, ..}`, reason contains `is not a setting of an export`, message unchanged.
  - `export_rename_to_a_taken_name_nacks`: two exports, rename one onto the other → `Nack{change: 1, ..}`, message unchanged.
  - Extend `summaries_say_what_a_change_does`: `set export photos read_only=true`, `renamed export photos to pictures`.
- [ ] **Step 2: Run** `cargo test -q -p irsync --test manage_admin`. Expected: does not compile (`ExportSet` unknown).
- [ ] **Step 3: Implement.** Proto messages; bump the version and its pin. In `apply_one`: `ExportSet` parses every setting with `exports::parse_setting` before changing anything (as `ClientSet` does), then `exports::export_set`, result `exports::set_line(msg, &s.name).unwrap_or_default()`; `ExportRename` calls `exports::rename_export`, result `exports::renamed_line(&r.old, &r.new, path)`. `summary`: `format!("set export {} {}", s.name, s.settings.join(" "))` and `format!("renamed export {} to {}", r.old, r.new)`.
- [ ] **Step 4: Run** `cargo test -q -p irsync --test manage_admin` and `cargo test -q -p irsync-proto`. Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "Remote export set and export rename in the admin engine"`.

### Task 2: `irsync remote S export set|rename`

**Files:**
- Modify: `crates/irsync/src/cli/mod.rs` (`RemoteExportCmd`: `Set { name, settings }` and `Rename { old, new }`, doc comments as the local `ExportCmd`'s)
- Modify: `crates/irsync/src/remote.rs` (`plan`)
- Modify: `book/src/reference/management.md` ("Remote use": the two commands), `book/src/how-to/remote-admin.md` ("Run a command")
- Test: `crates/irsync/tests/remote_e2e.rs`, `crates/irsync/tests/cli.rs`

**Interfaces:**
- Consumes: `Op::ExportSet`, `Op::ExportRename` (task 1).

- [ ] **Step 1: Write the failing tests:**
  - `remote_e2e.rs` `remote_export_set_and_rename`: `setup(Some(ReadWrite))`; `remote myserver export add 'photos:~/Photos'`; `remote myserver export set photos read_only=true` prints (stderr) `photos: read_only=true index_at_start=false fsync=false`; `remote myserver export rename photos pictures` succeeds; the server's local `export list` shows `pictures` read_only and no `photos`.
  - `cli.rs`: `remote myserver export set photos read_only=true` parses to `RemoteExportCmd::Set`; `export set photos` with no setting is a usage error (`required = true`, as locally).
- [ ] **Step 2: Run** `cargo test -q -p irsync --test remote_e2e remote_export_set_and_rename --test cli`. Expected: FAIL (unknown subcommand).
- [ ] **Step 3: Implement** the two `RemoteExportCmd` variants and their `plan` arms (`Shape::Applied`); the book lines.
- [ ] **Step 4: Run** the same tests and `python3 -m unittest discover -s tools/tests`. Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "irsync remote export set and export rename"`.

### Task 3: Live counts and the run table

**Files:**
- Modify: `crates/irsync/src/warm.rs` (`Progress`, `index_root_tracked`)
- Create: `crates/irsync/src/daemon/indexing.rs` (the run table, pure, with its unit tests)
- Modify: `proto/irsync/v1/admin.proto` (`IndexRun`, used by the table)
- Modify: `crates/irsync/src/daemon.rs` (`mod indexing;`, `Shared.runs`, `start_tasks` goes through `spawn_run`)
- Test: `crates/irsync/tests/warm.rs`, unit tests in `daemon/indexing.rs`, `crates/irsync/tests/admin_session.rs` (at-start runs, in task 4)

**Interfaces:**
- Produces (warm.rs):
  - `pub struct Progress { pub examined: AtomicU64, pub hashed: AtomicU64, pub bytes: AtomicU64, pub failed: AtomicU64 }` (`Default`), updated as `index_root` goes.
  - `pub async fn index_root_tracked(index: IndexHandle, root: &Path, workers: usize, dry_run: bool, progress: &Progress) -> io::Result<WarmReport>`; `index_root` calls it with a fresh `Progress`. `failed` counts the files that go into `errors` during the walk; the walk's own gaps are added at the end.
- Produces (proto): `message IndexRun { string export = 1; string path = 2; string started_by = 3; bool at_start = 4; uint64 started_at = 5; uint64 ended_at = 6; uint64 examined = 7; uint64 hashed = 8; uint64 bytes = 9; uint64 failed = 10; bool incomplete = 11; string error = 12; }` — `started_by` is the client's audit label, empty when `at_start`; `started_at`/`ended_at` in Unix seconds, `ended_at = 0` while running.
- Produces (daemon/indexing.rs):
  - `pub(super) enum StartedBy { Client(String), AtStart }`
  - `pub(super) struct Runs` (`Default`), a `Mutex<BTreeMap<PathBuf, Run>>` inside; `Run` holds export name, `StartedBy`, `started_at: SystemTime`, `ended_at: Option<SystemTime>`, `progress: Arc<Progress>`, `incomplete: bool`, `error: Option<String>`.
  - `pub(super) fn claim(&self, path: &Path, export: &str, by: StartedBy) -> Result<(Arc<Progress>, v1::IndexRun), v1::IndexRun>`: `Err(current)` when a run for `path` has no `ended_at`; else replaces the entry with a new run and returns its progress and its status.
  - `pub(super) fn finish(&self, path: &Path, result: &io::Result<WarmReport>)`: sets `ended_at`, `incomplete`, `error` (the `io::Error` text), and the final counts from the report.
  - `pub(super) fn list(&self) -> Vec<v1::IndexRun>` in path order.
- Produces (daemon.rs): `Shared.runs: indexing::Runs`; `fn spawn_run(shared: &Arc<Shared>, name: &str, export: &Export, index: IndexHandle, by: StartedBy) -> Result<v1::IndexRun, v1::IndexRun>` — claims, spawns `index_root_tracked` under `shared.tasks` (as `start_tasks` does today), logs `export indexed` with the counts and one `warn` per failed file (`export = name, "cannot index FILE: MSG"`, the file escaped like `manage::admin::summary` escapes), then `finish`es. `Ok` = started, `Err` = already running. `start_tasks` calls it with `StartedBy::AtStart` instead of spawning `index_root` itself.

- [ ] **Step 1: Write the failing tests:**
  - `warm.rs` `progress_matches_the_report`: three files, one unreadable (unix, `chmod 000`, skip as root); after `index_root_tracked`, `progress.examined == 3`, `progress.hashed == r.hashed == 2`, `progress.failed == 1 == r.errors.len()`.
  - `indexing.rs` `a_second_claim_while_running_is_refused`: `claim(p, "photos", AtStart)` is `Ok`; a second `claim(p, "photos", Client("laptop"))` is `Err(run)` with `run.at_start` and `run.ended_at == 0`.
  - `a_claim_after_the_end_replaces_the_run`: claim, `finish(p, &Ok(report))`, claim again by `Client("laptop")` → `Ok`, and `list()` has one run, `started_by == "laptop"`, `ended_at == 0`.
  - `a_failed_run_ends_with_its_error`: claim, `finish(p, &Err(io::Error::other("gone")))` → `list()[0].ended_at > 0`, `error == "gone"`.
  - `two_paths_are_two_runs`: claims on two paths both `Ok`; `list()` in path order.
  - `a_control_character_in_a_failed_path_is_escaped`: the helper that builds the `warn` line text (`fn failed_line(file: &str, msg: &str) -> String` in `indexing.rs`) turns `"a\nb"` into `a\\nb`.
- [ ] **Step 2: Run** `cargo test -q -p irsync --test warm` and `cargo test -q -p irsync --lib daemon::indexing`. Expected: does not compile.
- [ ] **Step 3: Implement** `Progress` and `index_root_tracked`; `IndexRun`; `Runs`; `Shared.runs`, `spawn_run`; `start_tasks` through `spawn_run`.
- [ ] **Step 4: Run** the tests above and `cargo test -q -p irsync --all-features` (the daemon tests that use `index_at_start` still pass). Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "Track the daemon's indexing runs in a run table"`.

### Task 4: `StartIndex` and `ReadIndexing` on the admin session

**Files:**
- Modify: `proto/irsync/v1/admin.proto` (`AdminRequest`: `StartIndex start_index = 4;`; `Read.what`: `ReadIndexing indexing = 3;`; `AdminReply`: `IndexStarted index_started = 6; Indexing indexing = 7;`)
- Modify: `crates/irsync/src/daemon/admin.rs` (`serve_admin` loop, a new `start_index`, `answer` for `ReadIndexing`)
- Test: `crates/irsync/tests/admin_session.rs`

**Interfaces:**
- Consumes: `spawn_run`, `StartedBy`, `Shared.runs.list()`, `open_index`, `start_tasks` (task 3).
- Produces (proto): `message StartIndex { string export = 1; }`, `message ReadIndexing {}`, `message IndexStarted { bool already_running = 1; IndexRun run = 2; }`, `message Indexing { repeated IndexRun runs = 1; }`.
- Produces (daemon): `async fn start_index(shared: &Arc<Shared>, path: &Path, id: EndpointId, export: String) -> admin_reply::Msg`. Off the runtime: read the saved config, `admin_of != Some(ReadWrite)` → `Refused{Unauthorized, NOT_ALLOWED}` and a `warn` "admin: WHO: indexing refused"; `Config::from_message` and look the export up by name → else `Refused{BadRequest, "no export \"NAME\""}`. Then `open_index`; `spawn_run(.., StartedBy::Client(label))` **before** `start_tasks` when the index is fresh (so an `index_at_start` export's at-start run is the one refused, not the requested one); an `open_index` error → `Refused{BadRequest, "cannot open the index of export NAME"}` with the error logged. On `Ok`: info `admin: WHO started indexing export NAME`, reply `IndexStarted{already_running:false}`; on `Err`: `IndexStarted{already_running:true}` with the current run. None of these replies closes the session.
- `ReadIndexing` in `answer`: after the existing admin check, `Indexing{runs: shared.runs.list()}` (`answer` takes `&Shared` or the list, implementer's choice).

- [ ] **Step 1: Write the failing tests** in `admin_session.rs` (add `Session::start_index(&mut self, export: &str) -> admin_reply::Msg` and `Session::read_indexing(&mut self) -> Vec<IndexRun>`):
  - `an_admin_starts_indexing_and_polls_it`: export `photos` with two files; `start_index("photos")` → `IndexStarted{already_running:false}`, `run.started_by == "laptop"`; poll `read_indexing` (`within`, 50 ms apart) until `ended_at > 0`; then `examined == 2`, `hashed == 2`, `failed == 0`, `error == ""`.
  - `a_second_start_while_running_says_already_running`: an export with 1000 small files; two `start_index` back to back. The second is `already_running: true` with the first run's `started_at`, or, only if the first run had already ended by then, `already_running: false`. After the end, `read_indexing` has exactly one run for the path. (The no-double-run rule itself is pinned without timing by the unit tests of task 3.)
  - `a_read_only_admin_cannot_start_but_can_read`: `admin = "ro"` → `start_index` is `Refused{Unauthorized}`, then `read_indexing` answers (session still open).
  - `an_uncommitted_or_unknown_export_is_no_export`: send `export add` change, then `start_index` on it before COMMIT → `Refused{BadRequest}` with `no export`; after COMMIT `start_index` → started.
  - `an_index_at_start_run_shows_in_the_status`: export with `index_at_start = true` at daemon start → `read_indexing` eventually has a run with `at_start == true`, `started_by == ""`.
  - `a_requested_run_on_a_fresh_index_at_start_export_is_the_clients` (Review Focus 1): an export with `index_at_start = true` committed remotely after start (so its index is fresh), then `start_index` → `already_running: false`, `started_by == "laptop"`, and `read_indexing` has one run for the path.
  - `the_status_carries_no_file_paths`: one unreadable file (unix, not root); after the run, encode the `Indexing` reply and assert the file's name does not occur in its bytes; `failed == 1`.
- [ ] **Step 2: Run** `cargo test -q -p irsync --all-features --test admin_session`. Expected: does not compile.
- [ ] **Step 3: Implement** the proto messages and `start_index`; the `serve_admin` arm; the `ReadIndexing` read.
- [ ] **Step 4: Run** the same command. Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "Start and watch indexing runs over the admin session"`.

### Task 5: `irsync remote S index [NAME]`

**Files:**
- Modify: `crates/irsync/src/cli/mod.rs` (`RemoteCmd::Index { export: Option<String> }`, doc: "Hash an export's files on the server into its index, and wait until done. With no name, show the indexing runs since the daemon started.")
- Modify: `crates/irsync/src/remote.rs` (`RemoteRequest::StartIndex(String)`, `RemoteRequest::Indexing`; `RemoteOutcome::Indexed(IndexRun)`, `RemoteOutcome::Indexing(Vec<IndexRun>)`; `Shape::Indexed`, `Shape::IndexList`; `decode_reply`'s `what: Option<bool>` becomes `enum Expect { Ack, View, Config, IndexStarted, Indexing }`)
- Modify: `crates/irsync/src/main.rs` (`run_remote`: exit code from the outcome, Ctrl-C)
- Test: `crates/irsync/tests/remote_e2e.rs`, unit tests in `remote.rs`

**Interfaces:**
- Consumes: the proto of task 4.
- Produces: `const POLL: Duration = Duration::from_secs(1);` in `remote.rs`. `converse` keeps the stream open for `StartIndex`: send Hello and `StartIndex`, read `IndexStarted`; then every `POLL` send `ReadIndexing` and read `Indexing`, until the run with the same `path` and `started_at` has `ended_at > 0`, or the path shows a run with a later `started_at` (then report the last state seen as ended) — Review Focus 3. `REPLY_WAIT` applies to each reply, not to the whole wait. Other requests keep today's one-shot path.
- Printing (`print`): `Shape::Indexed` prints to stdout what the local command prints, `{path}: {examined} file(s), {hashed} hashed ({bytes} bytes)`; then to stderr `irsync: {failed} file(s) could not be read; the log on {server} names them` when `failed > 0`, `irsync: {server} could not list everything under {path}` when `incomplete`, `irsync: indexing {path} on {server} failed: {error}` when `error` is set. Exit 23 for `failed > 0 || incomplete`, 1 for `error`, else 0. `Shape::IndexList` prints one stdout line per run: `{export}: {path}: {state}, {examined} file(s), {hashed} hashed ({bytes} bytes)` plus `, {failed} failed` when `failed > 0`, plus ` (index_at_start)` or ` (by {started_by})`; `state` is `running`, `done`, `incomplete` or `failed: {error}`. Nothing printed when there are no runs. `-q` drops the stdout lines, as locally.
- Ctrl-C while waiting: stderr `irsync: indexing goes on on {server}; see irsync remote {server} index`, exit 130; the connection is closed, which does not stop the run.

- [ ] **Step 1: Write the failing tests:**
  - `remote.rs` unit `an_index_line_says_how_the_run_stands`: `IndexRun` values for running, done with 2 failed, failed with error `gone`, at start → the four lines above, exactly.
  - `remote_e2e.rs` `remote_index_waits_and_prints_the_summary`: `setup(Some(ReadWrite))`, two files in `~/Photos` on the server, `remote myserver export add 'photos:~/Photos'`, `remote myserver index photos` → exit 0, stdout ends `: 2 file(s), 2 hashed (N bytes)`; `remote myserver index` → one line starting `photos: ` with `done` and `(by granted)`.
  - `remote_index_exits_23_on_a_failed_file` (unix, not root): one `chmod 000` file → exit 23, stderr `1 file(s) could not be read; the log on myserver names them`, and the file's name is not in stdout or stderr.
  - `remote_index_of_an_unknown_export_is_an_error`: exit 1, stderr contains `no export "nope"`.
  - `read_only_admin_cannot_start_indexing`: `setup(Some(ReadOnly))` → `remote myserver index photos` fails, `remote myserver index` succeeds.
  - `ctrl_c_leaves_the_run_going` (unix): 2000 files; spawn `remote myserver index photos`, wait for the server's `started indexing` log line, send SIGINT → exit 130, stderr `indexing goes on on myserver`; then `remote myserver index` eventually shows `done`.
  - `a_replaced_run_ends_the_wait` (Review Focus 3, unit level in `remote.rs`): the function that decides from one `Indexing` reply whether the wait is over (`fn wait_over(mine: &IndexRun, runs: &[IndexRun]) -> Option<IndexRun>`) returns the last seen state when the path has a later `started_at`, and `None` while the same run has `ended_at == 0`.
- [ ] **Step 2: Run** `cargo test -q -p irsync --all-features --test remote_e2e` and `cargo test -q -p irsync --bin irsync remote`. Expected: FAIL.
- [ ] **Step 3: Implement** the CLI variant, `plan` arms, `Expect`, the long `converse` path with `wait_over`, the printers, the exit codes and Ctrl-C in `main.rs`'s `run_remote`.
- [ ] **Step 4: Run** the same commands. Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "irsync remote index: start an export's indexing and wait for it"`.

### Task 6: The book, and the full check

**Files:**
- Modify: `book/src/how-to/remote-admin.md` (a section "Index an export": start, wait, Ctrl-C, `index` with no name, counts only and the server's log)
- Modify: `book/src/how-to/seed-the-index.md` ("On the daemon, before a push": the remote way)
- Modify: `book/src/reference/management.md` ("Remote use": `index [NAME]`, its lines and exit codes 0, 1, 23, 130)
- Modify: `book/src/reference/command-line.md` (`irsync remote` commands)
- Modify: `book/src/reference/protocol.md` (the new messages, version 9)

- [ ] **Step 1: Write** the sections, in the book's style (short sentences, `myserver`, `~/Photos`).
- [ ] **Step 2: Run** `python3 -m unittest discover -s tools/tests`. Expected: OK.
- [ ] **Step 3: Run** `tools/ci.sh check` and `tools/ci.sh typecheck`. Expected: both pass.
- [ ] **Step 4: Commit** `git commit -m "Book: remote export set, rename and indexing"`.
