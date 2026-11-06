# SDD ledger — plan: docs/superpowers/plans/2026-10-29-telemetry.md
Worktree: ../irsync-telemetry (branch telemetry, from main b9f3369; merged in 65d54df and removed)
Spec: docs/superpowers/specs/2026-10-29-telemetry-design.md

## Pre-flight scan
| Rows | Produces / consumes | Finding |
|---|---|---|
| T1→T2 | Meter, Timed, Activity, Counter, Usage, set_enabled | consistent |
| T1→T3 | Usage.process/net empty in T1, filled T3 | consistent |
| T2→T3 | run_role sets usage; T3 folds sampler into it there | consistent |
| T2→T6 | Meter updates globals; publish reads global() | consistent |
| T4→T6 | T4 set_enabled in run_sync; T6 replaces with init | consistent |
| T4→T7 | short_dur reused for timing units | consistent |
| T5→T6 | phase histogram needs recorder (none until T6: no-op) | consistent |
| T5→T9 | daemon session span parent None; T9 set_parent | consistent |
| T6→T7/T8 | layers None until filled | consistent |
| T8→T9 | inject/extract | consistent |
| T1 self | tests vs interfaces | consistent |
| T2 self | telemetry_off.rs relies on default-false flag | consistent |
| T3 self | sampler test in telemetry_usage.rs | consistent |
| T4 self | layout fixture vs layout | consistent |
| T5 self | test uses set_default on current-thread rt; spans created on spawn_blocking threads would be invisible | conflict -> Ruling 2 |
| T6 self | cli test expects exit code 1, but main uses Args::parse (clap exits 2) | conflict -> Ruling 1 |
| T7 self | client addr learned from stderr line printed after trailer | consistent |
| T8/T9 self | both in telemetry_otel.rs; global subscriber needed once | Ruling 3 |

Ruling 1: Task 6 `no_wait_requires_debug_addr` asserts exit code 2 (clap usage error, as Args::parse does today), not 1 — plan text was wrong about existing behaviour — cost if wrong: one assert.
Ruling 2: Task 5 tests install the capture layer with set_global_default in their own test binary (not set_default), so spans from any thread are seen — cost if wrong: test rework only.
Ruling 3: Task 9 propagation tests that need an in-process OTel subscriber go in their own binary tests/telemetry_trace.rs (global default set once via OnceLock); telemetry_otel.rs keeps the binary-driven T8 tests — cost if wrong: file naming only.

## Progress
Task 1: dispatched (base b9f3369)
Task 1: review 1 (dfb4b97): Needs fixes — Important: global meters' peer_wait double-counts (since=Some at build). Minor promoted: disabled meter usage() reports nonzero wall/peer_wait — Task 2's telemetry_off test asserts wall==0, so load-bearing.
Task 1: minor (deferred): counters_reach_the_global_meter exact-delta assert could race with later tests in the lib binary using (Client,Sender) global.
Task 1: minor (deferred): Usage::add must merge process/net once Task 3 fills them (carry into Task 3 dispatch).
Ruling 4: Task 2 adds the `metrics = "0.24"` workspace dependency (plan put it in Task 6, but Task 2 already calls histogram!) — histogram! without a recorder is a no-op, so nothing changes at runtime — cost if wrong: Task 6 dedupes a Cargo line.
Task 1: fix round 1/5 (2 addressed, 0 open; commits dfb4b97..ff610ff)
Task 1: complete (commits b9f3369..ff610ff, review clean)
Task 2: dispatched (base ff610ff)
Task 2: note: build_info test fails in the worktree on base too (pre-existing, environment) — check at final review
Task 2: review (69f3739): Approved, spec ✅. ⚠️ resolved below.
Ruling 5: daemon `export` label is added in Task 6 (Meter gains an optional export name set in serve_daemon; histograms and published counters carry it on the daemon) — brief for Task 2 named only side/role — cost if wrong: label rework in Task 6.
Ruling 6: the idle mutex taken on 0↔1 guard transitions (telemetry on only) is accepted; hot path with telemetry off is a branch only — cost if wrong: measurable overhead found in Task 10 benchmark, then switch to a lock-free idle clock.
Ruling 7: send/recv busy time includes network and flow-control waits on data streams; peer_wait is "no local work at all". Documented in Task 10 docs — cost if wrong: user wants a separate network-wait bucket later.
Task 2: minor (deferred): irsync_fsync_seconds has no side/role labels (recv.rs ~1018) — fold into Task 6 label work.
Task 2: minor (deferred): --bwlimit throttle sleep counts as peer wait (send.rs ~1136).
Task 2: minor (deferred): dead `|| inject` clause in DiskWrite condition (recv.rs ~880).
Task 2: minor (deferred): BytesMatched over-counts on hash-mismatch retries (sender_role.rs ~258, receiver.rs ~656).
Task 2: minor (deferred): symlink/special placement and MetaOnly under Plan, not Place (planner.rs ~628-644).
Task 2: minor (deferred): delta payload encoding counted as Send (send.rs ~1124).
Task 2: minor (deferred): global meter index derived from discriminants (meter.rs ~294-312); prefer explicit table.
Task 2: complete (commits ff610ff..69f3739, review clean)
Task 3: dispatched (base 69f3739)
Task 3: review (3c281b5): Approved, spec ✅. ⚠️ QUIC metric labels -> folded into Ruling 5 (Task 6 adds side/role/export labels to sampler metrics). ⚠️ conn.paths() at stop: accepted, fallback exists.
Task 3: minor (deferred): rtt_avg merge is a pairwise mean, not sample-weighted (meter.rs:139-143).
Task 3: minor (deferred): process-wide CPU/RSS summed across concurrent daemon sessions overcounts; document on ProcessUsage.
Task 3: minor (deferred): sampler.rs has no unit test (integration only); fd-growth test retries to absorb churn.
Task 3: complete (commits 69f3739..3c281b5, review clean)
Task 4: dispatched (base 3c281b5)
Ruling 8: Task 4 layout test uses rates computed by the stated rules (18.19M/s literal/wall, 422.30M/s hashed/hash-busy) instead of the plan example's inconsistent 17.77M/s / 412.00M/s — the rule is the requirement, the example was arithmetic slop — cost if wrong: two numbers in a fixture.
Task 4: review (ba699e0): Approved, spec ✅. ⚠️ usage populated: confirmed engine.rs sets it. ⚠️ clippy: implementer ran --all-features clean; --no-default-features meaningful from Task 8.
Task 4: minor (deferred): layout test computes rate lines with the same formula (partly tautological); hardcode 18.19M/s and 422.30M/s.
Task 4: minor (deferred): no test of the failed-session summary path (main.rs failure arm).
Task 4: minor (deferred): rows() mixes sep.len() bytes with char widths.
Task 4: complete (commits 3c281b5..ba699e0, review clean)
Task 5: dispatched (base ba699e0)
Task 5: note for Task 9: only run_with_reconnect creates the client session span; propagation tests must use Harness::run_reconnect, not run_client.
Task 5: review (e663390): Approved; spec ❌ items all minor, ruled below.
Ruling 9: `file.kind` is `new`/`delta` only; `copy` is dropped (the wire cannot tell the sender, and receiver-side local copies are per-chunk, not per-file) — cost if wrong: one field value later.
Ruling 10: sender `transfer` span ending at role end (includes the peer's delete-after wait) is accepted — brief allowed "or the role ends" — cost if wrong: sender transfer phase overstated with --delete-after.
Ruling 11: carry into Task 6 (it owns labels/init): phase() must not read the clock or record when telemetry is off; connect/reconnect phases pass the client's role instead of None ("none" label); irsync_fsync_seconds gets side/role labels; sampler QUIC metrics get side/role; daemon metrics get `export` (Ruling 5) — cost if wrong: Task 6 is larger.
Task 5: minor (deferred): spawn_reader body not re-indented after instrument wrap (control.rs); extract async fn.
Task 5: minor (deferred): connect span stays open until first path report (includes handshake).
Task 5: minor (deferred): record_outcome only on Ok (failed sessions lack reconnects/rtt fields).
Task 5: minor (deferred): daemon session records source_index=0; should be Empty.
Task 5: minor (deferred): run_client_inner records session_id via Span::current(); pass span explicitly.
Task 5: minor (deferred): no test for delete span.
Task 5: complete (commits ba699e0..e663390, review clean)
Task 6: dispatched (base e663390)
Task 6: note: implementer choices — publisher publishes own side only; process metrics side-only; reconnects_total emitted in reconnect loop; sessions_total counts per connection attempt; Prometheus upkeep every 5 s.
Task 6: review (436c550): Approved, spec ✅. ⚠️ deviations ruled:
Ruling 12: process metrics carry only `side` (a role label would double-count process-wide values) — cost if wrong: add a label later.
Ruling 13: the publisher publishes only the process's own side's globals — a process is either client or daemon — cost if wrong: none in production; in-process tests see one side.
Ruling 14: irsync_sessions_total counts connection attempts (a reconnecting client counts failed then ok); document in docs/telemetry.md — cost if wrong: dashboards misread failure rate; switch to per-session counting.
Ruling 15: irsync_reconnects_total emitted in the reconnect loop (run_role can't see reconnects) — cost if wrong: none.
Ruling 16: OTLP-only config enables telemetry without a recorder until Task 8 adds the bridge — transitional — cost if wrong: Task 8 must wire it (carried in dispatch).
Ruling 17: carry into Task 7: rename process_resident_memory_bytes -> process_max_resident_memory_bytes (value is peak RSS). Carry into Task 8: put the async OTLP flush inside the same 2 s-bounded shutdown future (init.rs shutdown).
Task 6: minor (deferred): files_total{result=failed} uses o.errors.len() which may include non-file errors.
Task 6: minor (deferred): init ignores failed subscriber install silently (try_init).
Task 6: minor (deferred): settings debug_socket parses twice; deserialize into SocketAddr.
Task 6: minor (deferred): manage_config test calls library set/get, not the binary.
Task 6: complete (commits e663390..436c550, review clean)
Task 7: dispatched (base 436c550)
Task 7: review (f0fdca7): Approved, spec ✅.
Task 7: minor (deferred, likely fix before merge): http accept loop spins on persistent accept errors (EMFILE) — sleep before continue (http.rs ~537).
Task 7: minor (deferred, likely fix before merge): no write deadline on responses; a non-reading client holds a task/fd forever (http.rs ~600).
Task 7: minor (deferred, likely fix before merge): SIGINT race — "Ctrl-C to exit" printed before signal handlers are installed (main.rs ~232).
Task 7: minor (deferred): flame parent self time undercounted when children run on other threads (flame.rs ~419).
Task 7: minor (deferred): timing percentiles can exceed max; clamp to max (timing.rs ~966).
Task 7: minor (deferred): daemon debug-port warning omits the address (main.rs ~277).
Task 7: minor (deferred): respond() catch-all reason phrase (http.rs ~589).
Task 7: minor (deferred): port_in_use test would hang rather than fail on regression (telemetry_debug.rs ~1332).
Task 7: complete (commits 436c550..f0fdca7, review clean)
Task 8: dispatched (base f0fdca7)
Note: build_info passes on main checkout; worktree failure likely stale vergen commit (rerun-if-changed on .git paths differ in a worktree). Verify at final review by touching crates/irsync/build.rs and re-running.
Task 8: review (93f9245): Approved, spec ✅. ⚠️ absolute-delta: global meters are monotonic process-lifetime totals, so fetch_max semantics hold. ⚠️ endpoint_id -> Ruling 18.
Ruling 18: irsync.endpoint_id cannot be a resource attribute (identity loads after init); Task 9 records the local endpoint id as an `endpoint_id` field on both sides' `session` spans instead — cost if wrong: attribute lives on spans, not resource.
Task 8: minor (deferred): empty OTEL_METRIC_EXPORT_INTERVAL skips the 10 s default (otel.rs providers).
Task 8: minor (deferred): shutdown warning can be lost if publish::once eats the 2 s budget.
Task 8: minor (deferred): resource test passes vacuously with OTEL_SERVICE_NAME set; inject env.
Task 8: minor (deferred): bridge gauge / mixed increment+absolute paths untested.
Task 8: minor (deferred): no-feature extract stub returns () — make it crate-private so misuse fails to compile.
Task 8: minor (deferred): rustls default provider installed process-wide only with OTLP; add invariant comment.
Task 8: complete (commits f0fdca7..93f9245, review clean)
Task 9: dispatched (base 93f9245)
Task 9: review (6579bbd): Approved, spec ✅. ⚠️ no-default-features clippy: controller ran it, clean. ⚠️ malformed traceparent path -> checked at final review.
Task 9: minor (deferred): serve_daemon has 8 args behind allow(too_many_arguments); bundle into a struct.
Task 9: minor (deferred): endpoint id string allocated per accepted connection (daemon.rs ~279).
Task 9: complete (commits 93f9245..6579bbd, review clean)
Task 10: dispatched (base 6579bbd)
Ruling 19: Task 10 overhead criteria are wall-time (plan text): sinks-off +0.4% vs main, all-on -3.3% vs off — met. All-on CPU +6.8% vs off is above 5% but not a stated criterion; documented, not blocking — cost if wrong: a profiling pass on the -v/sampler/exporter path.
Task 10: review (b9e37d2): Needs fixes — Important: docs/daemon.md lost the word 'Changing' (accidental edit). Minor: spec status line period. Fix round 1 dispatched.
Task 10: fix round 1/5 (1 addressed + 1 minor, 0 open; commits b9e37d2..886bb58) — 2-line docs diff verified inline by controller (diff shows only the restored word and the period).
Task 10: complete (commits 6579bbd..886bb58, review clean)
Final review: dispatched (b9f3369..886bb58)
Final review (886bb58): With fixes. Important: http accept spin; no write deadline/connection cap (fd exhaustion can fail syncs); SIGINT handler race; fmt log lines now carry ~300-char session span prefix.
Ruling 20: log prefix — keep the session span context on fmt lines (useful to tell concurrent daemon sessions apart) but record `endpoint_id` only when OTel export is on, and document the prefix in docs/telemetry.md — cost if wrong: daemon logs stay more verbose than before; switching fmt to events-only is a one-line change.
Ruling 21: the daemon's tracer provider uses an AlwaysOn sampler unless OTEL_TRACES_SAMPLER is set, so a client's sampled flag cannot suppress daemon traces; documented — cost if wrong: daemon exports traces the client meant to drop.
Final fix wave: dispatched (base 886bb58)
Final fix wave: commits 886bb58..3f7276e; re-review dispatched
Final fix wave: re-review all 10 addressed; minor: well_formed rejects higher-version traceparents with extra fields (harmless). Final review clean.

## Status check 2026-11-05

Every deferred minor above checked against the code at 3d94c43.

Closed:
- Final fix wave (886bb58..3f7276e): http accept spin, response write deadline, SIGINT race, daemon bind warning address, port_in_use hang, empty OTEL_METRIC_EXPORT_INTERVAL; log-line span prefix (Ruling 20).
- Since: Usage::add merges process/net (with a test); irsync_fsync_seconds carries side/role/export; dead `|| inject` removed (ac43e54).
- 6678359 fixed the eight that reported wrong values: timing percentiles above max; http catch-all reason phrase; files_total{failed} counting non-file errors; BytesMatched over-count on an in-session resend; --bwlimit sleep as peer wait; delta encoding as Send and symlink/special/MetaOnly (and mkdir) as Plan; flame self time with children on other threads; failed sessions lacking rtt/cwnd/lost_packets/reconnects.

Accuracy or docs, fixed the same day:
- rtt_avg merges weighted by rtt_samples (new NetUsage field) instead of a pairwise mean.
- ProcessUsage documents that its figures are process-wide, so concurrent daemon sessions' must not be summed.
- The daemon's session span leaves source_index empty; only the client records it.
- Shutdown uses one deadline for the whole and the OTLP flush, so the flush's warning is never cut off; it names the budget, not the time left.

Test gaps, closed the same day (all but one):
- -v summary layout test expects the hand-worked 18.19M/s and 422.30M/s.
- verbose_prints_a_summary_for_a_failed_session (isolated_e2e) covers the failure arm.
- delete_pass_has_its_span_under_the_receiving_session (telemetry_spans).
- bridge_mixes_increments_with_absolute_values_and_forwards_gauges (telemetry_otel).
- resource_with takes the service name, so the resource test checks the override whatever the environment; only the default name skips under OTEL_RESOURCE_ATTRIBUTES.
- stop_reports_the_process_and_the_connection (sampler.rs) checks real samples.
- config_sets_and_checks_the_telemetry_keys (commands_e2e) runs the binary.
- The meter's parent-summing tests use a parent of their own; session_meters_sum_into_the_process_wide_ones checks the link by pointer.
- a_session_that_gives_up_still_counts_its_reconnects (telemetry_spans).
- Left: process.rs sample_sees_cpu_and_fds retries, as other tests in the binary open and close descriptors (inherent to a process-wide count).
- Found meanwhile, fixed the same day: `config set` refusing a file that would not load named the file and the reason twice (ManageError::Invalid's reason is no longer an error source).

Code quality, fixed 2026-11-06:
- A failed subscriber install now warns on stderr ("logging and tracing off") instead of passing silently.
- run_client_inner, the reconnect loop and serve_daemon_inner take the `session` span and record on it, not on Span::current(); only the public run_client still takes the current span, as documented.
- debug_addr deserializes straight into a SocketAddr; debug_socket() is gone.
- control.rs: the reader's loop is an async fn (read_control), at normal indentation.
- The no-otel `extract` stub is gone, and otel::extract is private (only set_parent uses it).
- otel.rs notes that the rustls default-provider install is process-wide.
- The daemon builds the endpoint id string once, and again after a reload, not per accepted connection.
- Meter arrays: a const assertion pins Activity::ALL and Counter::ALL to their discriminants; counters are sized by COUNTERS; global() uses an explicit (side, role) table.
- summary rows() counts the separator in chars like the items.

Still open, code quality:
- serve_daemon: 8 args behind allow(too_many_arguments) (one of eight such functions; serve_daemon_inner joined them when it took the span).

Open by design: the connect span includes the handshake; well_formed() accepts only version-00 traceparents.
