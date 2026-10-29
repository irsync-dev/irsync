# Telemetry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Instrument irsync once (`tracing` spans + per-session meters published through the `metrics` crate) and feed a `-v` session summary, a debug HTTP port, and OTLP export from that single set of signals, with client and daemon linked in one trace.

**Architecture:** A new `crates/irsync/src/telemetry/` module owns everything. Hot paths only touch a per-session `Meter` (relaxed atomics, a no-op when telemetry is off); a 1 s publisher copies process-global meter totals into `metrics` counters, while per-file/per-phase histograms call `metrics::histogram!` directly. One layered `tracing` subscriber carries the existing fmt logger plus optional timing, flame and OpenTelemetry layers, each with its own filter. The client's trace context rides in `Hello`.

**Tech Stack:** Rust, tokio, `tracing` 0.1 / `tracing-subscriber` 0.3, `metrics` 0.24, `metrics-util` 0.20, `metrics-exporter-prometheus` 0.18 (`default-features = false`), `opentelemetry` / `opentelemetry_sdk` / `opentelemetry-otlp` 0.33, `tracing-opentelemetry` 0.34 (these versions were checked to resolve to a single `opentelemetry` 0.33.1).

**Spec:** `docs/superpowers/specs/2026-10-29-telemetry-design.md`

## Global Constraints

- All sinks off by default; with none on, no metrics recorder is installed and no layer other than fmt exists.
- Telemetry failures (collector down, port in use, bad endpoint) warn once on stderr and never change a sync's result or exit code.
- No path or session-id metric labels. Labels: `side` (`client`/`daemon`), `role` (`sender`/`receiver`), and on the daemon `export`; plus the metric-specific label named below.
- No per-chunk or per-frame spans. Per-file spans are `debug` level and exported only with `IRSYNC_TRACE_FILES=1`.
- `--stats` and the `sent .. received ..` / `total size is ..` trailer stay byte-for-byte unchanged.
- Debug port: a bare port means `127.0.0.1:<port>`; never binds a non-loopback address unless one is given explicitly.
- OTLP shutdown flush is bounded at 2 s.
- OTLP export is behind cargo feature `otel`, on by default. Without it, `[telemetry]`/`OTEL_EXPORTER_OTLP_*` warn once: `irsync: built without OpenTelemetry support; ignoring [telemetry]`.
- Service names: `irsync` (client), `irsync-daemon` (daemon). Resource attributes: `service.version` (package version), `irsync.endpoint_id`.
- **Deviation from the spec, forced by the `metrics` API (u64 counters):** busy time is `irsync_busy_microseconds_total{activity}`, not `irsync_busy_seconds_total`.
- No backward compatibility for protocol or config is needed (project memory).
- `cargo fmt --all --check` and `cargo clippy --all-targets -- -D warnings` stay clean, with and without `--no-default-features`.

## Review Focus

1. **A dead or unreachable collector**: a 1-second sync with `OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:9` must finish in about its normal time (≤ 2 s extra) with exit code 0. Test in Task 8.
2. **The debug port address is already in use**: the sync runs normally, with one warning and exit code 0, and it doesn't wait for a signal (there's no port to keep open). Test in Task 7.
3. **`-v` with several sources** (a push of `a b c` to one destination) prints one summary per session, in order, each before the trailer. Test in Task 4.
4. **macOS `ru_maxrss` is in bytes, Linux's is in KiB**: peak RSS must not be off by 1024×. Unit test in Task 3.
5. **A misbehaving debug HTTP client** (a header that never ends, or more than 8 KiB) must not stall other requests or the sync. Test in Task 7.

---

## File Structure

```
crates/irsync/src/telemetry/
  mod.rs        Side, Role, enabled flag, re-exports; `phase()` helper
  meter.rs      Activity, Counter, Meter, Timed guard, Usage snapshot, globals
  process.rs    ProcessSample: getrusage + open fds, cross-platform
  sampler.rs    per-session 1 s sampler (process + QUIC) into the Meter
  publish.rs    1 s task: global meters -> `metrics` counters
  init.rs       TelemetryConfig, init(), Telemetry handle, shutdown
  timing.rs     TimingLayer: per-span-name latency histograms, text render
  flame.rs      FlameLayer: folded self-time stacks, bounded
  http.rs       hand-rolled HTTP/1.1 debug server
  otel.rs       (cfg otel) OTLP tracer/meter providers, propagation helpers
  otel_bridge.rs (cfg otel) `metrics::Recorder` forwarding to an OTel Meter
crates/irsync/src/cli/progress/summary.rs   -v session summary renderer
```

Modified: `Cargo.toml` (workspace deps), `crates/irsync/Cargo.toml`, `crates/irsync/src/lib.rs`, `src/main.rs`, `src/cli/mod.rs`, `src/cli/progress/mod.rs`, `src/config/{file,settings}.rs`, `src/manage/config.rs`, `src/session/{mod,engine,reconnect,sender_role,receiver,planner,recv_io,recv_delete,control}.rs`, `src/{hasher,send,recv,scan,delete,daemon}.rs`, `proto/irsync/v1/common.proto`, `.github/workflows/ci.yml`, `README.md`, `docs/daemon.md`, new `docs/telemetry.md` and `docs/telemetry/compose.yaml`.

---

### Task 1: Meter core

**Files:**
- Create: `crates/irsync/src/telemetry/mod.rs`, `crates/irsync/src/telemetry/meter.rs`
- Modify: `crates/irsync/src/lib.rs` (add `pub mod telemetry;`)
- Test: unit tests in `meter.rs`

**Interfaces:**
- Produces (in `telemetry`):
  - `pub enum Side { Client, Daemon }` and `pub enum Role { Sender, Receiver }`, each with `fn label(self) -> &'static str`
  - `pub fn set_enabled(on: bool)` and `pub fn enabled() -> bool`, backed by a global `AtomicBool`, default false
- Produces (in `telemetry::meter`):
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Activity { Walk, Plan, Hash, Read, Send, Recv, Write, Place, Delete }`
    - `pub const ALL: [Activity; 9]`
    - `fn label(self) -> &'static str`: `walk`, `plan`, `hash`, `read`, `send`, `recv`, `write`, `place`, `delete`
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Counter { BytesLiteral, BytesMatched, BytesHashed, FilesHashed, DiskRead, DiskWrite, FsyncNanos }`, with `ALL`
  - `pub struct Meter`
    - `pub fn new(side: Side, role: Role, enabled: bool) -> Arc<Meter>`
    - `pub fn start(&self, a: Activity) -> Timed<'_>`: a guard that adds the elapsed time to `a` when dropped. When disabled it is a no-op and doesn't read the clock.
    - `pub fn add(&self, c: Counter, n: u64)`
    - `pub fn usage(&self) -> Usage`
    - `pub fn finish(&self)`: closes the idle clock and adds this session's idle time to the global meter once
  - `pub fn global(side: Side, role: Role) -> &'static Meter`: four statics, never "finished". `Meter::start` and `Meter::add` also update the matching global.
  - `#[derive(Clone, Debug, Default)] pub struct Usage`, with public fields:
    - `wall: Duration`
    - `busy: [Duration; 9]`, indexed by `Activity as usize`
    - `peer_wait: Duration`
    - `counters: [u64; 7]`
    - `process: ProcessUsage` and `net: NetUsage`: both `Default`, defined empty here and filled in by Task 3
  - `impl Usage`:
    - `fn busy(&self, a: Activity) -> Duration`
    - `fn count(&self, c: Counter) -> u64`
    - `fn add(&mut self, o: &Usage)`: sums, and takes the max for peaks

Idle accounting (the "peer_wait" of the spec): `peer_wait` is wall time with zero active `Timed` guards. Keep an `AtomicUsize` count of active guards. Only on a 0→1 or 1→0 transition, lock a `Mutex<IdleClock { since: Option<Instant>, total: Duration }>`. Under the lock, re-read the count: if it is 0 and `since` is `None`, start the clock; if it is above 0 and `since` is `Some`, add the elapsed time. Re-deciding under the lock makes racing transitions self-correcting. The clock starts at `Meter::new`.

- [ ] **Step 1: Write the failing tests** in `meter.rs`:
  - `disabled_meter_records_nothing`: `Meter::new(.., false)`; run a `start(Hash)` guard over a 5 ms sleep, then `add(DiskRead, 10)`. `usage().busy(Hash) == ZERO` and `count(DiskRead) == 0`.
  - `timed_guard_adds_busy_time`: an enabled meter, a guard over a 20 ms sleep. `busy(Hash) >= 20ms` and `< 200ms`.
  - `idle_is_time_with_no_guard_active`: an enabled meter; sleep 30 ms, a guard over 30 ms, `finish()`. `peer_wait` is in `[25ms, 100ms)`, and `wall >= 60ms`.
  - `overlapping_guards_do_not_count_as_idle`: two threads each hold a guard over the same 30 ms. `busy(Read) >= 60ms`, and `peer_wait` is less than 15 ms of the guarded window (measure `finish()` right after the joins).
  - `counters_reach_the_global_meter`: add 7 to `BytesLiteral` on an enabled client/sender meter. `global(Client, Sender).usage().count(BytesLiteral)` rises by exactly 7 (read before and after).

- [ ] **Step 2: Run them to verify they fail.** Run `cargo test -p irsync --lib telemetry::meter`. Expected: they fail to compile because `telemetry` doesn't exist yet.

- [ ] **Step 3: Implement `telemetry/mod.rs` and `telemetry/meter.rs`** per the Interfaces. Use `[AtomicU64; 9]` nanoseconds for busy time and `[AtomicU64; 7]` for counters, all `Relaxed`.

- [ ] **Step 4: Run the tests to verify they pass.** Same command. Expected: 5 passed.

- [ ] **Step 5: Commit**
```bash
git add crates/irsync/src/telemetry crates/irsync/src/lib.rs
git commit -m "feat(telemetry): per-session meter: busy time per activity, idle as peer wait, counters"
```

---

### Task 2: Meter the session's work

**Files:**
- Modify:
  - `crates/irsync/src/session/mod.rs`: add `Ctx.meter: Arc<Meter>`, `Outcome.usage: Usage`, and `Outcome::absorb` summing `usage`
  - `crates/irsync/src/session/engine.rs`: build the meter in `run_client_inner` (`Side::Client`) and `serve_daemon` (`Side::Daemon`); the role is `Sender` if `sends`; `enabled = telemetry::enabled()`
  - `run_role` must set `outcome.usage = ctx.meter.usage()` after `ctx.meter.finish()`, on both `Ok` and `Err`. On `Err` there is no Outcome; the meter is still finished so the globals get its idle time.
  - `crates/irsync/src/hasher.rs`: add `pub fn with_meter(self, m: Arc<Meter>) -> Self`; `Activity::Hash` around each file's read+hash in the worker, plus `Counter::BytesHashed` and `FilesHashed`
  - `crates/irsync/src/scan.rs`: `Activity::Walk` over `run_walk`, excluding time blocked in `tx.blocking_send` (drop and re-take the guard around it). `walk()` gains a `meter: Option<Arc<Meter>>` parameter.
  - `crates/irsync/src/session/planner.rs`: `Activity::Plan` around processing each merged entry, never across an `.await` on the listing channels
  - `crates/irsync/src/send.rs`:
    - `Activity::Read` and `Counter::DiskRead` around each `read_exact_at` (around line 505)
    - `Activity::Send` around stream writes and `finish` (around lines 379–447)
    - `Counter::BytesLiteral` for literal bytes written
  - `crates/irsync/src/session/recv_io.rs`:
    - `Activity::Recv` around `read_stream` and `read_range` reads
    - `Activity::Read` and `Counter::DiskRead` for `read_local` and basis reads (lines 539, 587, 622)
    - `Counter::BytesMatched` where chunks are satisfied locally
  - `crates/irsync/src/recv.rs`:
    - `Activity::Write` and `Counter::DiskWrite` in `write_chunk` and `copy_local`
    - `Activity::Place` for all of `finalize`
    - `Counter::FsyncNanos` and `histogram!("irsync_fsync_seconds")` around `sync_all` (line 963)
    - The `FileWriter` gets the meter from its opener (`open*` constructors take `Option<Arc<Meter>>`).
  - `crates/irsync/src/delete.rs`: `Activity::Delete` around `DeletePass::execute`'s removals (`set_meter(Arc<Meter>)`, like `set_activity`)
  - `sender_role.rs`, `receiver.rs`:
    - `histogram!("irsync_file_transfer_seconds")` from a file's first request or stream to its DONE
    - `histogram!("irsync_file_size_bytes")` per transferred file
    - `histogram!("irsync_hash_wait_seconds")` around role-side awaits of `hasher.digest`
    - Labels for all three: `side`, `role`
- Test: `crates/irsync/tests/telemetry_usage.rs` (new)

**Interfaces:**
- Consumes: Task 1's `Meter`, `Activity`, `Counter`, `Usage`, `telemetry::set_enabled`.
- Produces: `Outcome.usage: Usage`, filled on both sides. `Ctx.meter`, which Tasks 3 and 5 use.

- [ ] **Step 1: Write the failing tests** using the harness in `tests/common`:
  - Setup: call `irsync::telemetry::set_enabled(true)`. A process-global flag is fine here: each tests file is its own binary.
  - `push_usage_counts_walk_hash_read_send`: push 20 files of 64 KiB to an empty daemon destination. The client outcome has:
    - `busy(Walk) > 0`, `busy(Read) > 0`, `busy(Send) > 0`
    - `count(DiskRead) >= 20*65536` and `count(BytesLiteral) == outcome.bytes_transferred`
    - `wall >= busy(Walk)`
  - `pull_usage_counts_recv_write_place`: pull the same tree. The client outcome has `busy(Recv) > 0`, `busy(Write) > 0`, `busy(Place) > 0`, and `count(DiskWrite) == 20*65536`.
  - `delta_counts_matched_bytes`: push, change 1 chunk in one file, push again. On the second run, `count(BytesMatched) > 0` and `count(BytesLiteral) < 65536 * 2`.
  - `disabled_usage_is_zero`: a separate test binary `tests/telemetry_off.rs` that never enables. Every field of `outcome.usage.busy` is zero and `wall` is zero.

- [ ] **Step 2: Run them to verify they fail.** Run `cargo test -p irsync --test telemetry_usage --test telemetry_off`. Expected: they fail to compile because `Outcome.usage` doesn't exist.

- [ ] **Step 3: Implement the wiring** per the Files list. If a constructor gains a meter argument, pass `None` at call sites that are outside a session (the `irsync warm`/index paths).

- [ ] **Step 4: Run the tests to verify they pass.** Same command, then the full suite: `cargo test -p irsync`. Expected: everything passes, and the existing tests are unchanged.

- [ ] **Step 5: Commit**
```bash
git add -A crates/irsync
git commit -m "feat(telemetry): meter walk, plan, hash, read, send, recv, write, place and delete per session"
```

---

### Task 3: Process and QUIC sampling

**Files:**
- Create: `crates/irsync/src/telemetry/process.rs`, `crates/irsync/src/telemetry/sampler.rs`
- Modify:
  - `telemetry/meter.rs`: fill in `ProcessUsage { cpu_user: Duration, cpu_sys: Duration, peak_rss: u64 /* bytes */, peak_fds: u64 }` and `NetUsage { rtt_min: Duration, rtt_max: Duration, rtt_avg: Duration, lost_packets: u64, sent_packets: u64, cwnd: u64 }`
  - `session/engine.rs` (`run_role`): spawn the sampler while the role runs; abort it on exit; fold its final sample into the usage
- Test: unit tests in `process.rs` and `sampler.rs`

**Interfaces:**
- Produces:
  - `#[derive(Clone, Copy, Debug, Default)] pub struct ProcessSample { pub cpu_user: Duration, pub cpu_sys: Duration, pub max_rss_bytes: u64, pub open_fds: Option<u64> }`
  - `pub fn sample() -> ProcessSample`, using `nix::sys::resource::getrusage(UsageWho::RUSAGE_SELF)`
    - `ru_maxrss` is in KiB on Linux and in bytes on macOS. Use `#[cfg(target_os = "macos")]` and expose `fn rss_bytes(raw: i64) -> u64` for testing.
    - Open fds: count the entries of `/proc/self/fd` on Linux and `/dev/fd` on macOS, minus the one used by the read itself.
  - `pub fn spawn(meter: Arc<Meter>, conn: iroh::endpoint::Connection) -> SamplerHandle`
    - Runs once a second, plus once at stop.
    - Records process CPU as a delta from spawn time, and tracks the peak fds.
    - Takes the RTT and congestion window from the best path in `conn.paths()`; if no path is available, use `conn.rtt` for the path the connection currently uses.
    - Records `histogram!("irsync_quic_rtt_seconds")` and the lost-packet delta via `counter!("irsync_quic_lost_packets_total").increment`.
  - `SamplerHandle::stop(self) -> (ProcessUsage, NetUsage)`
  - Spawned only when `telemetry::enabled()`.

- [ ] **Step 1: Write the failing tests:**
  - `rss_bytes_units`: on Linux `rss_bytes(2048) == 2048*1024`; on macOS `rss_bytes(2048) == 2048`.
  - `sample_sees_cpu_and_fds`: spin for 50 ms and open 3 temp files. `sample().cpu_user + cpu_sys` grew by at least 30 ms, and `open_fds` grew by at least 3 (Linux and macOS).
  - `tests/telemetry_usage.rs::push_usage_samples_process_and_rtt`: after one push (the sampler also samples at stop, so a short session works), the client outcome has `net.rtt_max > 0`, `rtt_min <= rtt_avg <= rtt_max`, `process.peak_fds > 0` and `process.peak_rss > 0`.

- [ ] **Step 2: Run them to verify they fail.** Run `cargo test -p irsync --lib telemetry::process telemetry::sampler` and `--test telemetry_usage`. Expected: they fail to compile.

- [ ] **Step 3: Implement** per the Interfaces.

- [ ] **Step 4: Run the tests to verify they pass.** Same commands. Expected: they pass.

- [ ] **Step 5: Commit**
```bash
git add -A crates/irsync
git commit -m "feat(telemetry): sample process cpu, rss, fds and QUIC rtt/loss per session"
```

---

### Task 4: `-v` session summary

**Files:**
- Create: `crates/irsync/src/cli/progress/summary.rs`
- Modify:
  - `session/mod.rs`: add `Event::SessionEnd(Arc<SessionSummary>)`
  - `main.rs` (the per-source loop in `sync`, around line 526): emit it after each session, success or failure; build the descriptor strings from the operands as typed
  - `session/reconnect.rs`: `run_with_reconnect` returns the reconnect count and the last connection kind through `Outcome` fields `reconnects: u32` and `conn: Option<ConnState>`
  - `cli/progress/mod.rs`: `Reporter::handle` prints the summary when `verbosity >= 1`, above the block (`self.line`)
  - `main.rs` (`run_sync`): `telemetry::set_enabled(true)` when `opts.verbose >= 1 && !opts.quiet`. Task 6 replaces this with `init`.
- Test: unit tests in `summary.rs`; `crates/irsync/tests/progress.rs` (extend)

**Interfaces:**
- Produces:
  - `pub struct SessionSummary { pub index: usize, pub of: usize, pub verb: &'static str /* "push" | "pull" */, pub from: String, pub to: String, pub reconnects: u32, pub conn: Option<ConnState>, pub usage: Usage, pub files_transferred: u64, pub files_hashed: u64 }`
  - `pub fn render(s: &SessionSummary, width: usize) -> Vec<String>` in `summary.rs`
  - `pub fn short_dur(d: Duration) -> String`: `0.4s`, `21.0s`, `2:47`, `1:02:03`. Below 60 s use one decimal; below one hour use `m:ss`; from one hour use `h:mm:ss` (the existing `clock`).

Exact layout. Lines are indented two spaces with a 10-column label. Show only activities with non-zero busy time, in `Activity::ALL` order, and wrap onto a continuation line indented to the value column (12 spaces) so nothing exceeds `width` (80 off a terminal). Sizes use `human()`; counts use `commas()`.
```
session 1/2  push  ./photos -> nas:/backup  wall 3:12  direct, 1 reconnect
  busy      walk 21.0s  hash 37.1s  read 1:02  send 1:48  delete 1.3s
  waiting   peer 44.0s
  transfer  1,882 files  3.41G literal  12.00G matched  17.77M/s
  hashing   2,107 files  15.30G  412.00M/s
  disk      read 15.40G  write 0  fsync 0.0s
  network   sent 3.52G  recv 41.00M  rtt 12ms (max 40ms)  lost 0.02%
  process   cpu 1:12 user, 0:31 sys  peak rss 312.00M  peak fds 214
```
- The transfer rate is literal bytes divided by wall time. The hashing rate is hashed bytes divided by busy hash time.
- "lost" is `lost_packets / sent_packets`. The `network` line is omitted when no RTT was sampled.
- With no reconnects the header ends in `direct` or `relay`; with N it is `, N reconnect(s)`.

- [ ] **Step 1: Write the failing tests:**
  - `summary.rs::renders_the_documented_layout`: a fixture `SessionSummary` whose values reproduce the block above. `render(.., 80)` equals those 8 lines exactly.
  - `summary.rs::lines_fit_width`: a fixture with every activity non-zero and huge values; for width 60 and 80, every line has `.chars().count() <= width`.
  - `summary.rs::short_dur_forms`: 0.4 s → `0.4s`, 59.94 s → `59.9s`, 167 s → `2:47`, 3723 s → `1:02:03`.
  - `tests/progress.rs::verbose_prints_one_summary_per_session` (Review Focus 3): push 3 sources with `-v` through a plain reporter. The output contains `session 1/3`, `session 2/3` and `session 3/3` in that order, all before the `sent ` trailer line.
  - `tests/progress.rs::no_summary_without_verbose`: `--stats` only; the output does not contain `session 1/`. The `--stats` block equals the existing expected text.

- [ ] **Step 2: Run them to verify they fail.** Run `cargo test -p irsync --lib cli::progress::summary` and `--test progress`. Expected: they fail to compile.

- [ ] **Step 3: Implement** per the Interfaces and the layout. The `Event` match in `Reporter::handle` is exhaustive; add the new arm.

- [ ] **Step 4: Run the tests to verify they pass.** Same commands, plus `--test progress_e2e --test progress_live`. Expected: they pass.

- [ ] **Step 5: Commit**
```bash
git add -A crates/irsync
git commit -m "feat(cli): -v prints a per-session time and resource summary"
```

---

### Task 5: Spans

**Files:**
- Modify:
  - `telemetry/mod.rs`: the `phase` helper
  - `session/reconnect.rs` (`run_with_reconnect`): the client `session` span, and `connect` / `reconnect` per attempt
  - `session/engine.rs`: `handshake` in `run_client_inner` and `serve_daemon`; the daemon `session` span in `serve_daemon`, created as `info_span!(parent: None, "session", ...)` so it never inherits an in-process parent; `run_role` runs instrumented by the current session span
  - `session/control.rs` (`spawn_listing`): `list`
  - `session/planner.rs`: `plan`
  - `sender_role.rs`, `receiver.rs`: `transfer`, opened lazily at the first file request sent or received and closed when the last file is acknowledged or the role ends; per-file `file` spans at `debug` level with children `hash`, `send`/`recv`, `place`
  - `session/recv_delete.rs`: `delete`
- Test: `crates/irsync/tests/telemetry_spans.rs` (new)

**Interfaces:**
- Produces:
  - `pub enum PhaseKind { Connect, Handshake, List, Plan, Transfer, Delete, Reconnect }`, with labels `connect`, `handshake`, `list`, `plan`, `transfer`, `delete`, `reconnect`.
  - `pub fn phase(kind: PhaseKind, side: Side, role: Option<Role>) -> Phase`. `tracing` needs literal span names, so it matches `kind` to one `info_span!("<label>", ...)` per variant.
    - `Phase::span(&self) -> &tracing::Span`.
    - Dropping a `Phase` records `histogram!("irsync_phase_duration_seconds", "phase" => label, "side" => .., "role" => ..)` with the time since creation.
  - Span fields:
    - `session`: `session_id` (hex, recorded after the ack), `direction`, `side`, `peer`, `resumed`, `source_index`, plus `rtt_max_ms`, `cwnd`, `lost_packets`, `reconnects`, recorded at the end
    - `connect`: `attempt`, `path`
    - `file`: `path`, `size`, `kind` (`new`/`delta`/`copy`)

- [ ] **Step 1: Write the failing tests.**
  - Install a test layer for the whole test with `tracing::subscriber::set_default` on a current-thread runtime. It records `(span name, parent name, level)` on `on_new_span`, with the parent resolved from `ctx.span_scope` or explicit parents.
  - `push_span_tree`: one push of 5 files. The recorded set contains:
    - `session` (parent `None`) once for the client
    - `session` (parent `None`) once for the daemon
    - `handshake`, `list`, `plan`, `transfer` each with a `session` ancestor
    - no span named `chunk` or `frame`
  - `file_spans_are_debug`: every `file` span recorded has level `DEBUG`.
  - `reconnect_stays_in_one_session_span`: use the existing drop-after-N fault hook from `tests/resume.rs`; afterwards exactly one client `session` span exists, with at least 2 `connect` children.

- [ ] **Step 2: Run them to verify they fail.** Run `cargo test -p irsync --test telemetry_spans`. Expected: they fail.

- [ ] **Step 3: Implement** per the Files and Interfaces lists. Instrument spawned tasks with `.instrument(span.clone())`, and pass the span explicitly into `spawn_blocking` closures.

- [ ] **Step 4: Run the tests to verify they pass.** Same command, then `cargo test -p irsync`. Expected: they pass.

- [ ] **Step 5: Commit**
```bash
git add -A crates/irsync
git commit -m "feat(telemetry): session, phase and per-file spans on both sides"
```

---

### Task 6: Subscriber and recorder init, configuration, metric publishing

**Files:**
- Create: `crates/irsync/src/telemetry/init.rs`, `crates/irsync/src/telemetry/publish.rs`
- Modify:
  - `Cargo.toml` and `crates/irsync/Cargo.toml`: `metrics`, `metrics-util`, `metrics-exporter-prometheus` (`default-features = false`)
  - `main.rs`: replace `init_logging` / `init_logging_at`
    - Every command still gets the fmt layer with the same defaults it has today.
    - `run_sync` and `run_daemon` create the runtime first, then call `init` inside `rt.enter()`.
    - Both load `Config` first, best-effort; on error use `Settings::default()`. The real error still surfaces later, where it does today.
  - `cli/mod.rs`: `--debug-addr <ADDR>` and `--debug-port-no-wait` on the sync args. `SyncOptions.debug_addr: Option<SocketAddr>` and `debug_no_wait: bool`. A bare port parses to `127.0.0.1:port`, and `--debug-port-no-wait` without `--debug-addr` is a clap usage error (`requires = "debug_addr"`).
  - `config/settings.rs`:
    - `DaemonSettings.debug_addr: Option<String>`, parsed with the same bare-port rule
    - a new `TelemetrySettings { otlp_endpoint: Option<Url>, otlp_protocol: Option<OtlpProtocol /* grpc | http */> }`
    - `Settings.telemetry`
  - `config/file.rs`: accept a top-level `telemetry` table in the shape pass and `RawMerged`
  - `manage/config.rs`: register `daemon.debug_addr` (Str), `telemetry.otlp_endpoint` (Url) and `telemetry.otlp_protocol` (Str)
- Test: `crates/irsync/tests/config_files.rs`, `manage_config.rs`, `cli.rs` (extend); unit tests in `init.rs`

**Interfaces:**
- Produces:
  - `pub struct TelemetryConfig { pub side: Side, pub log_default: String, pub summary: bool, pub debug_addr: Option<SocketAddr>, pub otlp: Option<OtlpConfig>, pub trace_files: bool }`
    - `OtlpConfig` is cfg-independent plain data: `endpoint: Url`, `protocol: OtlpProtocol`. Task 8 resolves it from the environment and `[telemetry]`.
    - `trace_files` is `IRSYNC_TRACE_FILES=1`.
  - `pub fn init<W: for<'a> MakeWriter<'a> + Send + Sync + 'static>(cfg: TelemetryConfig, writer: W) -> Telemetry`. Install the subscriber as `Registry` + fmt (with `EnvFilter`, as today, as a *per-layer* filter) + `Option<TimingLayer>` + `Option<FlameLayer>` + `Option<OtelLayer>`. Each optional layer gets a `Targets` filter: `irsync=info`, or `irsync=debug` when `trace_files` is set.
  - Metrics recorder:
    - With no debug port and no OTLP: none.
    - With exactly one of them: that one.
    - With both: `metrics_util::layers::FanoutBuilder`.
  - `telemetry::set_enabled(cfg.summary || cfg.debug_addr.is_some() || cfg.otlp.is_some())`
  - `pub struct Telemetry`:
    - `pub fn prometheus(&self) -> Option<&PrometheusHandle>`
    - `pub async fn start(&mut self) -> Option<SocketAddr>`: starts the publisher and the debug server; returns the bound address. Tasks 7 and 8 fill in the parts.
    - `pub async fn shutdown(self)`: flushes, bounded at 2 s in total
  - `publish::spawn() -> JoinHandle<()>`: once a second and once at shutdown, for each global meter (side, role), `counter!(..).absolute(..)` for:
    - `irsync_busy_microseconds_total{activity}`, including `activity="peer_wait"`
    - `irsync_bytes_literal_total`, `irsync_bytes_matched_total`, `irsync_bytes_hashed_total`, `irsync_files_hashed_total`
    - `irsync_disk_read_bytes_total`, `irsync_disk_write_bytes_total`
  - Session-level counters are emitted directly by `run_role` at session end:
    - `irsync_sessions_total{outcome}`
    - `irsync_files_total{result}`
    - `irsync_entries_listed_total{kind}`
    - `irsync_wire_bytes_total{dir}`
    - `irsync_deleted_total`
    - `irsync_reconnects_total{reason}` (client)
  - Gauges:
    - `irsync_sessions_active` (daemon): incremented and decremented in `serve_daemon`
    - `irsync_streams_open`: up and down where data streams open and close in `send.rs` and `recv_io.rs`
    - `irsync_hash_queue_depth`: up and down around queued hasher jobs in `hasher.rs`
  - Process metrics, set by the publisher each second from `process::sample()`: `process_cpu_seconds_total{mode}` (`user`/`sys`; a gauge in seconds, since a u64 counter can't hold fractional seconds), `process_resident_memory_bytes` and `process_open_fds`.
  - `describe_*!` with units for every metric, in `publish.rs`.

- [ ] **Step 1: Write the failing tests:**
  - `init.rs::recorder_choice`: `fn recorder_kind(cfg: &TelemetryConfig) -> Option<RecorderKind>` (a pure helper `init` uses; `enum RecorderKind { Prometheus, Otel, Fanout }`). It returns `None` with no sinks and for a `summary`-only config, `Some(Prometheus)` with only a debug address, `Some(Otel)` with only OTLP, and `Some(Fanout)` with both.
  - `config_files.rs::telemetry_table_parses`: `[telemetry] otlp_endpoint = "http://c:4318"` and `otlp_protocol = "http"` round-trip. An unknown key under `[telemetry]` is an error naming it.
  - `config_files.rs::daemon_debug_addr_bare_port`: `[daemon] debug_addr = "9464"` resolves to `127.0.0.1:9464`.
  - `manage_config.rs::set_telemetry_endpoint`: `irsync config set telemetry.otlp_endpoint http://c:4318`, then `get` prints it.
  - `cli.rs::no_wait_requires_debug_addr`: `irsync --debug-port-no-wait a b` exits with code 1, and its stderr mentions `--debug-addr`.

- [ ] **Step 2: Run them to verify they fail.** Run `cargo test -p irsync --lib telemetry::init` and `--test config_files --test manage_config --test cli`. Expected: they fail.

- [ ] **Step 3: Implement** per the Interfaces. Until Tasks 7 and 8 land, keep `TimingLayer`, `FlameLayer` and `OtelLayer` as `None`. The existing logging behaviour must not change; the whole suite proves that.

- [ ] **Step 4: Run the tests to verify they pass.** Run `cargo test -p irsync`. Expected: everything passes.

- [ ] **Step 5: Commit**
```bash
git add -A Cargo.toml Cargo.lock crates/irsync
git commit -m "feat(telemetry): one layered subscriber, optional metrics recorder, [telemetry] and debug-addr settings"
```

---

### Task 7: Debug HTTP port (Prometheus, timing, flame)

**Files:**
- Create: `crates/irsync/src/telemetry/{http,timing,flame}.rs`
- Modify:
  - `telemetry/init.rs`: enable the layers and the server when `debug_addr` is set
  - `main.rs` (`run_sync`): after the trailer and any error list, if the server is up and `!debug_no_wait`:
    - print to stderr `irsync: debug port open at {addr}; Ctrl-C to exit`
    - wait for SIGINT or SIGHUP (`tokio::signal`)
    - then return the sync's exit code unchanged
  - `daemon.rs` / `run_daemon`: start the server from `settings.daemon.debug_addr`, and print `debug port: {addr}` on stdout next to the `listening on:` lines (inside the existing `announce` closure, so it is printed again after a reload)
- Test: unit tests in each new file; `crates/irsync/tests/telemetry_debug.rs` (new, drives the binary like `tests/cli.rs`)

**Interfaces:**
- Produces:
  - `TimingLayer` (`tracing_subscriber::Layer`): on span close, records wall time (creation to close) and busy time (sum of enter→exit intervals) under the span name in a `Mutex<HashMap<&'static str, Hist>>`.
    - `Hist`: 64 log2 buckets of nanoseconds, plus count, sum and max.
    - `pub fn render(&self) -> String`: one header line `span count p50 p90 p99 max busy_total`, then one row per span name sorted by name. Percentiles are bucket upper bounds, formatted with `short_dur`-style units (`ms`/`s`).
  - `FlameLayer`: on each exit, adds that interval's *self* time (the interval minus the time children were entered within it) in microseconds to the folded key `root;…;leaf`, made of span names.
    - At most 10,000 distinct keys; further keys go to `[other]`.
    - `pub fn render(&self) -> String`: `key value\n` lines, sorted.
  - `pub async fn serve(addr: SocketAddr, routes: Routes) -> io::Result<(SocketAddr, JoinHandle<()>)>`, on `tokio::net::TcpListener`, with one task per connection:
    - HTTP/1.1 `GET` only; anything else gets 405. An unknown path gets 404.
    - The request head is capped at 8 KiB (431 above that) and must arrive within 5 s (then the connection closes).
    - Responses carry `Connection: close`, `Content-Type: text/plain; version=0.0.4` for `/metrics` and `text/plain; charset=utf-8` for everything else.
    - Routes:
      - `/`: lists the routes
      - `/metrics`: `PrometheusHandle::render()`
      - `/debug/timing`: `TimingLayer::render()`
      - `/debug/flame`: `FlameLayer::render()`
    - `Routes` holds `Option<PrometheusHandle>`, `Arc<TimingState>` and `Arc<FlameState>`, so the layers and the server share state.
  - A bind failure on the client: one warning `irsync: debug port {addr}: {err}`, no wait, the sync proceeds. On the daemon: a warning in the log, and the daemon runs.

- [ ] **Step 1: Write the failing tests:**
  - `timing.rs::records_count_and_max`: three spans named `x` of about 1, 2 and 4 ms. The rendered row for `x` has count `3` and max ≥ 4 ms.
  - `flame.rs::self_time_excludes_children`: an outer span of 10 ms containing an inner span of 6 ms. The `outer` value is about 4,000 µs (±2,000) and `outer;inner` is about 6,000 µs.
  - `flame.rs::caps_distinct_stacks`: call `FlameState::add(key: &str, micros: u64)` (the method the layer uses) with 10,050 distinct keys. `render()` has 10,001 lines, one of them `[other] 50` when each key added 1 µs.
  - `http.rs::routes_and_errors`: a server on `127.0.0.1:0`.
    - `GET /metrics` returns 200 with the Prometheus content type.
    - `POST /` returns 405 and `GET /nope` returns 404.
    - A 9 KiB header returns 431.
    - (Review Focus 5) A client that sends `GET / HTTP/1.1\r\n` and nothing more must not stop a concurrent `GET /metrics` from answering within 1 s.
  - `telemetry_debug.rs::client_waits_until_sigint`:
    - Run a local daemon (as `tests/cli.rs` does) and `irsync --debug-addr 127.0.0.1:0 -a src host:dst`. The bound address is learned from the stderr line.
    - After the trailer, `GET /metrics` contains `irsync_busy_microseconds_total{` and `irsync_files_total{`, and `GET /debug/timing` contains `session`.
    - The process is still running; send SIGINT; it exits with code 0.
  - `telemetry_debug.rs::no_wait_exits`: with `--debug-port-no-wait` the process exits by itself with code 0.
  - `telemetry_debug.rs::port_in_use_warns_and_continues` (Review Focus 2): bind the port first, then run with that `--debug-addr`. Exit code 0, one stderr line containing `debug port`, and the process does not wait.
  - `telemetry_debug.rs::daemon_debug_port`: with `[daemon] debug_addr = "127.0.0.1:0"`, the daemon prints `debug port: ADDR` on stdout next to `listening on:` lines. After one sync, `/metrics` contains `side="daemon"`.

- [ ] **Step 2: Run them to verify they fail.** Run `cargo test -p irsync --lib telemetry::timing telemetry::flame telemetry::http` and `--test telemetry_debug`. Expected: they fail.

- [ ] **Step 3: Implement** per the Interfaces.

- [ ] **Step 4: Run the tests to verify they pass.** Same commands, then `cargo test -p irsync`. Expected: they pass.

- [ ] **Step 5: Commit**
```bash
git add -A crates/irsync
git commit -m "feat(telemetry): debug HTTP port with /metrics, /debug/timing and /debug/flame"
```

---

### Task 8: OpenTelemetry export (feature `otel`)

**Files:**
- Create: `crates/irsync/src/telemetry/otel.rs`, `crates/irsync/src/telemetry/otel_bridge.rs` (both `#[cfg(feature = "otel")]`)
- Modify:
  - `Cargo.toml` (workspace) and `crates/irsync/Cargo.toml`: `[features] default = ["otel"]`, `otel = [dep:opentelemetry, ...]`. Dependency features:
    - `opentelemetry-otlp`: `default-features = false`, features `["trace", "metrics", "grpc-tonic", "tls-roots", "http-proto", "reqwest-blocking-client"]`
    - `opentelemetry_sdk`: `["trace", "metrics"]`, plus `testing` as a dev-dependency feature
    - `tracing-opentelemetry`: `default-features = false, features = ["metrics"]`
  - `telemetry/init.rs`:
    - resolve `OtlpConfig`; the environment wins over `[telemetry]`
    - enabled by `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` or `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT`, or by `[telemetry] otlp_endpoint`
    - the protocol comes from `OTEL_EXPORTER_OTLP_PROTOCOL` (`grpc`, `http/protobuf`), else `[telemetry] otlp_protocol`, else `http/protobuf`
    - plug in `OtelLayer` and the bridge recorder; without the feature, the warning from the Global Constraints
  - `.github/workflows/ci.yml`: add a step `cargo clippy -p irsync --no-default-features --all-targets -- -D warnings`
- Test: `crates/irsync/tests/telemetry_otel.rs` (new, `#![cfg(feature = "otel")]`)

**Interfaces:**
- Produces:
  - `pub fn providers(cfg: &OtlpConfig, side: Side, endpoint_id: Option<String>) -> anyhow::Result<OtelProviders>`
    - Batch span processor, and a periodic metric reader at a 10 s interval.
    - Must be built inside the tokio runtime context: tonic needs it.
    - The other standard `OTEL_*` variables (headers, service name override, sampler, timeouts) are read by the SDK builders' env support; don't re-implement them.
  - `OtelProviders::layer<S>(&self) -> OtelLayer<S>`, `OtelProviders::recorder(&self) -> OtelRecorder`, and `OtelProviders::shutdown(self, deadline: Duration)`. Shutdown runs on a blocking thread and is abandoned after `deadline`.
  - `pub struct OtelRecorder` implements `metrics::Recorder`:
    - counters map to OTel `u64` counters (`absolute` → the delta from the last value seen per key)
    - gauges map to OTel gauges
    - histograms map to OTel `f64` histograms
    - instruments are created once per key and cached in a `Mutex<HashMap<metrics::Key, …>>`
    - labels become attributes
  - `pub fn inject(span: &tracing::Span) -> (String, String)` → `(traceparent, tracestate)` and `pub fn extract(traceparent: &str, tracestate: &str) -> opentelemetry::Context`, using `TraceContextPropagator`. Task 9 uses these. Without the feature they are stubs that return empty strings or do nothing.

- [ ] **Step 1: Write the failing tests:**
  - `bridge_forwards_counters_and_histograms`:
    - Use an `opentelemetry_sdk` `InMemoryMetricExporter` meter provider and build an `OtelRecorder` on it.
    - `counter!("t_total").absolute(5)` then `absolute(8)`, and `histogram!("t_seconds").record(0.5)`; flush.
    - The exported sum for `t_total` is 8, and `t_seconds` has count 1 and sum 0.5.
  - `exports_spans_to_a_fake_http_collector`:
    - A `TcpListener` on `127.0.0.1:0` answers any `POST` with `200` and records the paths.
    - Run the binary with `OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:PORT` and `OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf` for one local-daemon push.
    - The collector saw at least one `POST /v1/traces` and at least one `POST /v1/metrics`, and the exit code is 0.
  - `dead_collector_does_not_delay_much` (Review Focus 1): `OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:9`. A one-file push exits 0, and its total time is at most 2.5 s more than the same push without the variable.
  - `no_default_features_warns` (a CI step rather than a cargo test): run `cargo run -p irsync --no-default-features -- ...` with the env var set; stderr contains `built without OpenTelemetry support`. Document this in `docs/telemetry.md` as a manual check.

- [ ] **Step 2: Run them to verify they fail.** Run `cargo test -p irsync --test telemetry_otel`. Expected: they fail.

- [ ] **Step 3: Implement** per the Interfaces.

- [ ] **Step 4: Run the tests to verify they pass.** Same command, then `cargo test -p irsync` and `cargo clippy -p irsync --no-default-features --all-targets -- -D warnings`. Expected: they pass and are clean.

- [ ] **Step 5: Commit**
```bash
git add -A Cargo.toml Cargo.lock crates/irsync .github/workflows/ci.yml
git commit -m "feat(telemetry): OTLP export of spans and metrics behind the default otel feature"
```

---

### Task 9: Trace propagation in `Hello`

**Files:**
- Modify:
  - `proto/irsync/v1/common.proto`: in `Hello`, add `string traceparent = 26;` and `string tracestate = 27;`, with a comment: W3C trace context; empty means no trace
  - `session/engine.rs`:
    - `run_client_inner` sets both fields from `otel::inject(&Span::current())`, which is the client `session` span
    - `serve_daemon` creates its `session` span with `parent: None` and, when the fields are non-empty and OTel export is on, calls `span.set_parent(otel::extract(..))` (`tracing_opentelemetry::OpenTelemetrySpanExt`)
  - `docs/protocol.md`: document the two fields
- Test: `crates/irsync/tests/telemetry_otel.rs` (extend)

**Interfaces:**
- Consumes: Task 8's `inject` and `extract`; Task 5's `session` spans.

- [ ] **Step 1: Write the failing tests:**
  - `daemon_session_is_child_of_client_session`: one in-process subscriber with the OTel layer on an `InMemorySpanExporter` provider; the harness runs client and daemon in-process. After one push, take the finished spans named `session`:
    - exactly two
    - the same `trace_id`
    - the daemon's `parent_span_id` equals the client's `span_id`
  - `no_trace_context_without_otel`: with no OTel layer installed, `Hello.traceparent` as sent is empty. Capture it through the harness's `hello()` path, or assert `otel::inject` on a span without OTel returns `("", "")`.
  - `reconnect_keeps_trace_id`: use the drop fault; every daemon `session` span (one per connection) has the client's `trace_id`.

- [ ] **Step 2: Run them to verify they fail.** Run `cargo test -p irsync --test telemetry_otel`. Expected: they fail.

- [ ] **Step 3: Implement** per the Files list. Regenerate the prost code through the existing `build.rs` path with `cargo build`.

- [ ] **Step 4: Run the tests to verify they pass.** Run `cargo test -p irsync`. Expected: everything passes.

- [ ] **Step 5: Commit**
```bash
git add -A proto crates docs/protocol.md
git commit -m "feat(proto): Hello carries the client's trace context; the daemon's session span is its child"
```

---

### Task 10: Overhead check and documentation

**Files:**
- Create: `docs/telemetry.md`, `docs/telemetry/compose.yaml` (an OTel collector plus Jaeger on 4317/4318/16686, and Prometheus scraping `host.docker.internal:9464`)
- Modify:
  - `README.md`: a short "Seeing where time goes" section pointing to `docs/telemetry.md`
  - `docs/daemon.md`: `debug_addr`, `[telemetry]`
  - `docs/superpowers/specs/2026-10-29-telemetry-design.md`: the status line now reads `implemented 2026-MM-DD; plan docs/superpowers/plans/2026-10-29-telemetry.md`; note the microseconds deviation
- Test: manual benchmark, with the results recorded in `docs/telemetry.md`

- [ ] **Step 1: Benchmark sinks off against `main`.** Build release binaries of `main` (in a worktree) and of this branch. Push a 20,000-file / 2 GiB tree, generated with `cargo run -p filegen` (see `crates/filegen` and `docs/FILEGEN_DECISIONS.md`), to a local daemon. Do it five times each, cold for the destination and warm for the index. Expected: the median wall time of the branch is within 2% of `main`. If not, profile and fix before continuing.

- [ ] **Step 2: Benchmark with all sinks on** (`-v`, `--debug-addr`, OTLP to the compose collector). Expected: within 5% of sinks-off. Record both tables in `docs/telemetry.md`.

- [ ] **Step 3: Write `docs/telemetry.md`**:
  - what each sink is and how to turn it on
  - the metric list with units and labels
  - the span tree
  - the busy-time and peer-wait meaning, and why the busy times can add up to more than the wall time
  - how to make a flamegraph: `curl -s localhost:PORT/debug/flame | inferno-flamegraph > f.svg`
  - running the compose file

- [ ] **Step 4: Run the full verification:** `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo clippy -p irsync --no-default-features --all-targets -- -D warnings && cargo test --workspace`. Expected: everything is clean and passes.

- [ ] **Step 5: Commit**
```bash
git add -A docs README.md
git commit -m "docs(telemetry): sinks, metrics, spans, overhead numbers and a local collector setup"
```
