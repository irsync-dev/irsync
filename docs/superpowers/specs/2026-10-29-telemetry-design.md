# Telemetry: one set of spans and metrics, three sinks

Date: 2026-10-29. Status: implemented 2026-10-29; plan docs/superpowers/plans/2026-10-29-telemetry.md.

## Intent

When a sync is slow, there is no way today to see where the time went —
walking, hashing, waiting on the peer, writing, deleting — or what it cost
in CPU, memory, disk I/O and wire bytes. The code logs through `tracing`
but has no spans; the only numbers are rsync's `--stats` block and the
`sent .. received ..` trailer, plus a few ad-hoc counters (`HashStats`,
`ListStats`, the QUIC connection's `stats()`).

The work instruments irsync **once** — `tracing` spans plus `metrics`
crate counters/gauges/histograms — and lets that single set of signals
feed any combination of three consumers:

1. **Debug HTTP port**: quick, local inspection of a running client or
   daemon with nothing else installed (`curl`, a browser, a local
   Prometheus scrape).
2. **OpenTelemetry (OTLP)**: enterprise-grade tracing and metrics into
   whatever collector the operator runs, with the client's and the
   daemon's sides of one session **linked in one trace**.
3. **`-v` session summary**: after each session the client prints a
   compact breakdown of where its time and resources went.

## Goals

- Answer "where did this slow sync spend its time" from either the `-v`
  summary (client side) or one OTel trace (both sides).
- Show resource costs per session: CPU (user/sys), peak RSS, disk bytes
  read/written, fsync time, wire bytes, QUIC RTT/congestion, open fds.
- Client and daemon share one trace: the client's trace context travels
  in `Hello`; the daemon's session span is its child. A resumed session
  (reconnect) stays in the same trace.
- Close to zero cost when every sink is off: no subscriber layers beyond
  the existing fmt logger, no metrics recorder installed.
- Debug port and OTel can run at the same time from the same signals.

## Non-goals

- Showing the daemon's numbers in the client's `-v` output (the daemon is
  visible through OTel and its own debug port).
- Replacing `--stats` or the rsync trailer; they stay byte-for-byte as is.
- Per-chunk or per-frame spans.
- Log export over OTLP (logs stay on stderr; span events may carry them
  to OTel traces through `tracing-opentelemetry` as it does by default).
- Alerting rules, dashboards, or a bundled collector config (a sample
  `docker compose` for local testing is fine, see below).
- Backward compatibility of the protocol or config (no users yet).

## Anti-goals

- Instrumentation that measurably slows a sync with sinks off, or by more
  than a few percent with all sinks on. Hot paths (chunk reads/writes,
  frame encode/decode, hashing inner loops) get counters/histograms only.
- A debug port reachable from the network by default.
- Telemetry failures (collector down, port in use) failing a sync: they
  warn once and the sync carries on.

## Time attribution (decided by the user)

Phases overlap: the planner merge-joins while both listings stream,
requests files mid-walk, and `--delete-during` deletes as it goes. So:

- **Traces** show the real intervals; `list`, `plan`, `transfer`,
  `delete` spans overlap and that is expected.
- **Metrics and the `-v` summary** show **busy time per activity**: time
  this side actually spent walking, planning, hashing, reading, sending,
  receiving/writing, placing (rename, metadata, fsync) and deleting,
  plus **time waiting on the peer** (blocked on the control stream with
  nothing local to do). These are accumulated across tasks, so with
  parallel workers they can exceed wall clock; the summary shows wall
  clock next to them and does not claim they add up.
- An activity that runs on the other side (e.g. `plan` on a push, where
  the daemon plans) is shown on this side as waiting, not as plan time.

The metric is `irsync_busy_seconds_total{activity}` (a counter; as built,
`irsync_busy_microseconds_total`, in microseconds, because the `metrics`
crate has only `u64` counters), with
`activity` in walk/plan/hash/read/send/recv/write/place/delete/peer_wait.


### Spans (`tracing`)

Names are stable and form the trace shape; fields in parentheses.

- `session` (session_id, direction, role, peer, resumed, source index) —
  root on the client; on the daemon, child of the remote context from
  `Hello`. One per source.
  - `connect` (attempt, conn path: direct/relay) — client only; one per
    (re)connect attempt.
  - `handshake` — Hello/HelloAck.
  - `list` (party) — this side's walk; ends at `ListingEnd`.
  - `plan` — the planner's merge/compare of both listings.
  - `transfer` — from the first data stream to the last ack.
    - `file` (path, size, kind: new/delta/copy) — one per transferred
      file, **at `debug` level** so it is off unless asked for (OTel
      filter, see Configuration). Sub-spans `hash`, `send`/`recv`,
      `place` (rename + metadata + fsync).
  - `delete` (mode) — each deletion pass.
  - `reconnect` (attempt, reason) — wait between attempts.

Existing `tracing` events inside these spans pick up the context for free.

### Metrics (`metrics` crate)

All carry labels `role` (sender/receiver) and `side` (client/daemon);
daemon metrics also carry `export`. No path or session-id labels
(cardinality); the session id goes on spans only.

Counters:
- `irsync_sessions_total{outcome}` (ok/partial/failed/max_delete)
- `irsync_entries_listed_total{kind}`
- `irsync_files_total{result}` (transferred/skipped/failed/created)
- `irsync_bytes_literal_total`, `irsync_bytes_matched_total`
- `irsync_bytes_hashed_total`, `irsync_files_hashed_total`
- `irsync_disk_read_bytes_total`, `irsync_disk_write_bytes_total`
- `irsync_wire_bytes_total{dir}` (tx/rx, QUIC level)
- `irsync_reconnects_total{reason}`
- `irsync_deleted_total`

Histograms (seconds unless stated):
- `irsync_phase_duration_seconds{phase}` (connect/handshake/list/plan/transfer/delete):
  span wall time, overlapping; for traces/dashboards, not the summary
- `irsync_file_transfer_seconds`, `irsync_file_hash_seconds`
- `irsync_hash_wait_seconds` (time a role waited on a hash result)
- `irsync_fsync_seconds`
- `irsync_file_size_bytes`

Gauges:
- `irsync_sessions_active` (daemon)
- `irsync_streams_open`, `irsync_hash_queue_depth`
(QUIC numbers are not gauges: a daemon serves several connections at
once and a process gauge would show whichever sampled last. *Call:* RTT
goes into `irsync_quic_rtt_seconds` as a histogram, sampled about once a
second per live connection; lost packets into
`irsync_quic_lost_packets_total` as a counter; cwnd and per-connection
values go on the `session` span as fields set at its end.)
- `process_cpu_seconds_total{mode}`, `process_max_resident_memory_bytes`,
  `process_open_fds` (sampled; `getrusage` / `/proc/self` on Linux,
  best-effort elsewhere)

Disk read/write bytes are counted at irsync's own read/write call sites
(not `/proc/self/io`), so they mean "file data irsync moved" and work on
macOS too.

## Sinks

### Debug HTTP port

- Client: `--debug-addr <host:port>`; daemon: `[daemon] debug_addr` in
  `irsync.toml` (picking it up on SIGHUP is not required; a restart is
  fine). Off when unset. A bare port means
  `127.0.0.1:<port>`.
- Endpoints:
- With `--debug-addr`, the client by default (decided by the user) keeps
  the port serving after the trailer and any error list are printed, and
  blocks until SIGINT or SIGHUP, then exits with the sync's exit code. It
  prints one stderr line saying so (`irsync: debug port open at ADDR;
  Ctrl-C to exit`). `--debug-port-no-wait` exits as soon as the sync
  ends instead; without `--debug-addr` it is a usage error. Tests scrape
  a finished client the default way and use `--debug-port-no-wait` where
  they only need "during".
  - `GET /metrics` — Prometheus text via `metrics-exporter-prometheus`.
  - `GET /debug/timing` — span latency histograms from `tracing-timing`,
    as plain text (per span name: count, p50/p90/p99/max).
  - `GET /debug/flame` — the folded stacks collected so far
    (`tracing-flame`-style, ready for `inferno-flamegraph`). Collection
    only runs while the port is enabled.
- Small, dependency-light server (hyper or a minimal hand-rolled HTTP/1.1
  responder on tokio — builder's call); runs on the existing runtime.

### OpenTelemetry

- Traces: `tracing-opentelemetry` layer → `opentelemetry-otlp` exporter
  (batch span processor).
- Metrics: the same `metrics` values reach OTel through a bridge
  recorder that forwards to an `opentelemetry` `Meter` (an existing
  maintained crate if one fits the current `opentelemetry` version,
  otherwise a small in-tree recorder — builder's call).
- Enabled when `OTEL_EXPORTER_OTLP_ENDPOINT` (or the traces/metrics
  specific variant) is set, or `[telemetry] otlp_endpoint` is in
  `irsync.toml`. Standard `OTEL_*` variables (service name, headers,
  protocol, sampler) are honoured; env wins over config.
- `service.name` is `irsync` (client) / `irsync-daemon`;
  resource attributes include version and endpoint id.
- Shutdown flushes with a bounded timeout (≈2 s) so a short sync is not
  held up by a dead collector.

### `-v` session summary

Printed after each session's `-v` file lines and before the existing
trailer, when `verbose >= 1` and not `-q`. Built from an in-process
recorder of this session's values (always installed when `-v` is set,
independent of the other sinks). Sketch:

```
session 1/1  push  ./photos -> nas:/backup  0:03:12  [direct, 1 reconnect]
  wall      0:03:12
  busy      walk 21.0s  hash 37.1s  read 1:02  send 1:48  delete 1.3s
  waiting   peer 44.0s
  transfer  1,882 files  3.41G literal  12.0G matched  41.2MB/s
  hashing   2,107 files  15.3G  412MB/s  waited 18.4s
  disk      read 15.4G  write 0  fsync 0.0s
  network   sent 3.52G  recv 41.0M  rtt 12ms  lost 0.02%
  process   cpu 1:12 user / 0:31 sys  peak rss 312M  peak fds 214
```

Exact layout is the builder's call; it must fit 80 columns and use the
existing `human`/`commas`/`clock` helpers. Off a terminal it is plain
text like the rest of `-v`.

## Trace propagation

- New `Hello` field: W3C `traceparent` (and optional `tracestate`)
  strings. Empty when the client has no OTel exporter; the daemon then
  starts its own root span.
- The client sets it from its `session` span; on reconnect it reuses the
  same `session` span so all attempts share the trace.
- *Call:* a session resumed by a *later* client process (a stored
  session record, `reconnect.rs`) starts a new trace; its `session` span
  carries the `session_id` attribute, which is how the runs are
  correlated. No index/state format change.
- The daemon only honours the context when its own OTel export is on.
  No other trust implications: it is a correlation id.

## Build

*Call:* OTLP export sits behind a cargo feature `otel`, **on by
default**. Without it, `[telemetry]` and `OTEL_*` are parsed and warned
about once ("built without OTel support"); the debug port and `-v`
summary are always built. CI gains one `--no-default-features` check.

## Zero cost when off

- No sink: no metrics recorder is installed (macros reduce to an atomic
  load and branch); no `tracing-opentelemetry`/timing/flame layers, so
  spans above the fmt filter level hit the callsite-interest cache.
- The `-v` summary recorder is cheap atomics keyed by metric; installed
  only with `-v`.
- Samplers (QUIC stats, process stats) run only while some sink is on.
- Fan-out: when several metric sinks are on, `metrics-util`'s fanout
  recorder feeds them; when one is on it is installed directly.
- Verify with a benchmark run (stress crate or a large local tree): wall
  time with all sinks off within noise of `main` today.

## Configuration summary

| Where | Setting | Effect |
|---|---|---|
| client CLI | `--debug-addr ADDR` | debug HTTP port |
| client CLI | `--debug-port-no-wait` | exit when the sync ends instead of waiting for SIGINT/SIGHUP |
| `[daemon]` | `debug_addr = "..."` | debug HTTP port on the daemon |
| `[telemetry]` | `otlp_endpoint`, `otlp_protocol` (grpc/http) | OTel export, both roles |
| env | `OTEL_*` | standard OTel config, overrides `[telemetry]` |
| env | `IRSYNC_TRACE_FILES=1` | include per-`file` spans in OTel/flame |

(Names are suggestions; the builder may adjust to match `irsync config`
conventions.)

## Testing

- Unit: span shape for a small push/pull using a test subscriber;
  metric values via `metrics-util`'s debugging recorder.
- Integration: `/metrics` and `/debug/timing` answer during and after a
  sync; `-v` summary present, `--stats`/trailer unchanged.
- Propagation: daemon session span's parent equals the client's span
  context (in-memory OTel exporter in tests).
- Local manual check: a sample `docker compose` with an OTel collector +
  Jaeger (or Grafana LGTM) under `docs/` to view a linked trace.

## Left to the builder

- HTTP server implementation, exact metric bucket boundaries, the OTel
  metrics bridge (crate vs in-tree), exact `-v` layout, which crate
  versions, and where helper modules live (suggested `crates/irsync/src/telemetry/`).
- Whether `tracing-timing`/`tracing-flame` are used directly or replaced
  by a small in-tree layer if their versions don't fit.
