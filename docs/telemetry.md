# Telemetry

When a sync is slow, irsync can show where the time went (walking, hashing,
waiting on the peer, writing, deleting) and what it cost in CPU, memory,
disk I/O and wire bytes. One set of spans and metrics feeds three sinks, each
off until asked for. With all three off the cost is a handful of untaken
branches (see [Overhead](#overhead)).

## The sinks

| Sink | Turn it on | What you get |
| --- | --- | --- |
| Summary | `-v` on the client | A per-session block at the end of each session: wall time, busy time per activity, peer wait, CPU, peak memory, wire bytes, RTT and loss. |
| Debug HTTP port | `--debug-addr [IP:]PORT` on the client, `debug_addr` under `[daemon]` for the daemon | `/metrics` (Prometheus text), `/debug/timing` (wall and busy time per span name) and `/debug/flame` (folded stacks). A bare port binds 127.0.0.1. |
| OpenTelemetry | `OTEL_EXPORTER_OTLP_ENDPOINT` or `[telemetry] otlp_endpoint` | Spans and metrics over OTLP, see [OpenTelemetry export](#opentelemetry-export). |

The client with `--debug-addr` waits for SIGINT or SIGHUP after the sync so
that you can scrape the final numbers; `--debug-port-no-wait` closes the port
when the sync ends instead. The daemon's port lives as long as the daemon.

Per-file spans (`file` and its children) are off by default because a tree of
tens of thousands of files would swamp a trace. `IRSYNC_TRACE_FILES=1` turns
them on for the span sinks (OTLP, `/debug/timing`, `/debug/flame`).

## Metrics

Every metric carries `side` (`client` or `daemon`) and `role`
(`sender` or `receiver`); the daemon's also carry `export`. There are no path
or session-id labels (cardinality); the session id is on the `session` span.
The Prometheus names below are the OTLP names; OTLP units follow the `Unit`
column.

Counters:

| Name | Unit | Extra labels | Meaning |
| --- | --- | --- | --- |
| `irsync_busy_microseconds_total` | microseconds | `activity` | Time spent per activity, summed over tasks: `walk`, `plan`, `hash`, `read`, `send`, `recv`, `write`, `place`, `delete`, `index_wait`, and `peer_wait`. See [Busy time](#busy-time-and-peer-wait). |
| `irsync_active_microseconds_total` | microseconds | `activity` | Wall time with at least one task doing each activity (no `peer_wait`), added when a session ends. |
| `irsync_sessions_total` | count | `outcome` | Sessions ended: `ok`, `partial`, `failed`, `max_delete`. Counts connection attempts: a client session that reconnects counts `failed` and then `ok`. |
| `irsync_files_total` | count | `result` | `transferred`, `skipped`, `failed`, `created`. `failed` counts the entries with at least one error, each once; errors about the session as a whole (an incomplete listing, a refused delete pass) are not counted. |
| `irsync_entries_listed_total` | count | `kind` | Source entries listed: `regular`, `directory`, `symlink`, `special`. |
| `irsync_bytes_literal_total` | bytes | | File data sent or received as literal chunk bytes. |
| `irsync_bytes_matched_total` | bytes | | File data the receiver did not take from the sender (local copies, earlier progress, files already identical), counted when each file is placed; the sender counts what the receiver's acknowledgement reports. |
| `irsync_bytes_hashed_total` | bytes | | Bytes the hashers read. |
| `irsync_files_hashed_total` | count | | Files the hashers read. |
| `irsync_disk_read_bytes_total` | bytes | | File data irsync read from disk. |
| `irsync_disk_write_bytes_total` | bytes | | File data irsync wrote to disk. |
| `irsync_wire_bytes_total` | bytes | `dir` | Bytes on the QUIC connection: `tx` or `rx`. |
| `irsync_deleted_total` | count | | Paths the receiver deleted. |
| `irsync_reconnects_total` | count | `reason` | Client reconnects: `dial` (no connection yet) or `lost`. |
| `irsync_quic_lost_packets_total` | count | | QUIC packets lost, sampled once a second. |

The busy-time, active-time and `irsync_bytes_*`/`irsync_files_hashed_total`/disk counters
are process-wide meters, published once a second and at exit, so they
aggregate across the exports a daemon serves and carry no `export` label.

Histograms:

| Name | Unit | Extra labels | Meaning |
| --- | --- | --- | --- |
| `irsync_phase_duration_seconds` | seconds | `phase` | Wall time of each session phase (`connect`, `handshake`, `list`, `plan`, `transfer`, `delete`, `reconnect`). Phases overlap. |
| `irsync_file_transfer_seconds` | seconds | | From a file's first request to its DONE. |
| `irsync_hash_wait_seconds` | seconds | | Time a role waited on a hash result. |
| `irsync_fsync_seconds` | seconds | | Time to fsync a received file. |
| `irsync_file_size_bytes` | bytes | | Size of each transferred file. |
| `irsync_quic_rtt_seconds` | seconds | | QUIC path RTT, sampled once a second per connection. |

Gauges:

| Name | Unit | Extra labels | Meaning |
| --- | --- | --- | --- |
| `irsync_sessions_active` | count | | Sessions the daemon is serving. |
| `irsync_streams_open` | count | | Open data streams. |
| `irsync_hash_queue_depth` | count | | Hash jobs waiting for a worker. |
| `process_cpu_seconds_total` | seconds | `mode` (`user`, `sys`) | CPU time of the process. |
| `process_max_resident_memory_bytes` | bytes | | Peak resident set size. |
| `process_open_fds` | count | | Open file descriptors. |

The `process_*` metrics carry only `side` (and `mode` for CPU), not `role` or
`export`: they describe the process, not a session.

### Busy time and peer wait

`irsync_busy_microseconds_total{activity}` is a counter in microseconds (the
`metrics` crate has only integer counters, so seconds would lose the
fraction). Each activity counts the time tasks spent doing it, summed over
every task.

- `send` and `recv` include network and flow-control waits on the data
  streams: a stream blocked on the peer's window, or paused by
  `--bwlimit`, is still busy sending.
- `peer_wait` is wall time with no activity in progress at all: this side has
  no local work and is waiting for the peer.
- An activity that runs on the other side (for example `plan` on a push,
  where the daemon plans) shows up here as `peer_wait`, not as plan time.
- `index_wait` is time spent waiting on the index: for its lock while
  another task holds it, or for the entries this side queued to be written.
  The index is one SQLite database per root. Placed and hashed files, and
  entries of copy sources found gone, are queued for a writer thread that
  writes them in batches (what queues up within 10 ms of the first, as
  every commit empties the read connections' page caches), so nobody waits
  for that, except once at the end
  of a session (and before a lookup that needs freshly hashed files). The
  rarer writes (saving progress) take turns behind one lock, and lookups
  run on read connections of their own. The waiting task's own activity (`write` or
  `place`) counts the same time too.
- `hash` on a receiver is the check of each arriving chunk against its hash,
  as well as hashing existing files. On a sender it includes encoding a
  chunk against the receiver's bases (matching its blocks).
- `place` covers every entry made or updated at the destination: placing
  received files, and creating directories, symlinks and special files, or
  updating an entry's metadata alone. Deciding what to do with an entry is
  `plan`.

Busy times can add up to more than the wall time: files are hashed, sent and
written in parallel, so ten workers busy for one second each count ten
seconds. They say where the work is, not how long the run took. `peer_wait`
is the only one that cannot overlap the others.

`irsync_active_microseconds_total{activity}` is the other half: the wall time
during which at least one task was doing the activity, so it never exceeds
the wall time. Busy divided by active is how many tasks did it at once, on
average. The `-v` summary shows the active time, followed by `×N` when that
average is 1.5 or more: `place 18.2s ×14.8` is 18.2 s of the run during which
about fifteen files were being placed at a time. `index_wait` appears there
as `index` on the `waiting` line.

## Spans

Names are stable. Per-file spans need `IRSYNC_TRACE_FILES=1`.

```
session            session_id, direction, side, role, endpoint_id, peer,
 |                 resumed, source_index; at the end rtt_max_ms, cwnd,
 |                 lost_packets, reconnects
 +- connect        attempt, path          (client only, one per attempt)
 +- handshake
 +- list           party
 +- plan
 +- transfer
 |   +- file       path, size, kind (new or delta)   [IRSYNC_TRACE_FILES=1]
 |       +- hash
 |       +- send   (sender)
 |       +- recv   (receiver)
 |       +- place  (receiver: rename, metadata, fsync)
 +- delete         mode                   (each deletion pass)
 +- reconnect      attempt, reason        (wait between attempts)
```

`session` is a root span per source: on the client a new trace, on the
daemon a child of the client's span (the client's W3C trace context travels
in `Hello`), so one sync is one trace across both machines. The local
endpoint id is the `endpoint_id` field on `session` spans, not a resource
attribute; it is recorded only while spans are exported over OTLP. Events
logged inside a span pick up its context.

Log lines show that context too: the daemon's lines, and the client's under
`-v`, are prefixed with the span stack and its fields, as in
`session{session_id=… side="daemon" …}:transfer: …`,
which tells concurrent sessions apart.

## Flamegraphs

`/debug/flame` serves self time per span stack in folded form (microseconds),
the input of the `inferno` tools:

```sh
cargo install inferno
curl -s localhost:9464/debug/flame | inferno-flamegraph > f.svg
```

With the client, run the sync with `--debug-addr 9464` and fetch after it
finishes, while the client waits for SIGINT.

## Running a local collector

`docs/telemetry/compose.yaml` starts an OpenTelemetry collector, Jaeger
(UI on http://localhost:16686) and Prometheus (http://localhost:9090), which
scrapes `host.docker.internal:9464`:

```sh
docker compose -f docs/telemetry/compose.yaml up -d
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318 \
  irsync -a --debug-addr 9464 src/ HOST:export/
```

The collector listens on 4317 (gRPC) and 4318 (HTTP). Jaeger shows the
`session` trace; Prometheus has the metrics above from the debug port, and the
OTLP metrics go to the collector's debug exporter. Stop it with
`docker compose -f docs/telemetry/compose.yaml down`.

## Overhead

Benchmark: a push of a generated tree to a local daemon on the same machine
(`irsync -a --no-resume`, destination empty, source index warm), release
builds, `main` at b9f3369 against this branch. The tree is `filegen run
--seed 1 --steps 600000 --max-total-size 2G --max-file-size 192K`: 22,000
files, 1.7 GiB (close to the 20,000 files and 2 GiB aimed at; `filegen`
draws a random sequence of operations and does not take a file count). It
lived on tmpfs, so disk speed does not enter. The machine is shared and
noisy: single runs of the same binary vary by up to 2x, so runs were
interleaved in random order and the table pools 23 runs per configuration
(5 + 8 + 10 over three batches); the last two columns are from the last
batch (10 runs, medians) and add the client and daemon CPU time, which is far
steadier than wall time.

| Configuration | Wall median, 23 runs | vs `main` | Wall median, 10 runs | CPU median, client + daemon | vs `main` |
| --- | --- | --- | --- | --- | --- |
| `main` | 11.59 s | | 11.97 s | 44.4 s | |
| branch, sinks off | 11.64 s | +0.4% | 10.50 s | 43.5 s | -2.1% |
| branch, all sinks on | 11.26 s | -3.3% | 11.45 s | 46.5 s | +4.6% |

"All sinks on" is `-v`, `--debug-addr` on the client and the daemon, and
OTLP over HTTP on both sides (`OTEL_EXPORTER_OTLP_ENDPOINT`). No real collector was
used: the endpoint was a local Python HTTP server that answers every POST
with 200, so serialisation and sending are measured, the collector's own
work is not. Against sinks off, the median wall time with everything on is
-3.3% (pooled) and CPU is +6.8% (10 runs), so the wall time stays within the
5% budget; the extra CPU is the once-a-second sampler, the 22,000 `-v`
lines and the exporters running on otherwise idle cores.

## OpenTelemetry export

OTLP export of spans and metrics is on when `OTEL_EXPORTER_OTLP_ENDPOINT`
(or `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` / `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT`)
is set, or `[telemetry] otlp_endpoint` is in `irsync.toml`; the environment
wins. The protocol is `OTEL_EXPORTER_OTLP_PROTOCOL` (`grpc` or
`http/protobuf`), else `[telemetry] otlp_protocol` (`grpc` or `http`), else
HTTP. The other standard `OTEL_*` variables (headers, service name,
sampler, timeouts, `OTEL_METRIC_EXPORT_INTERVAL`) are honoured. Metrics are
exported every 10 s (an empty `OTEL_METRIC_EXPORT_INTERVAL` counts as unset)
and once more at exit; the exit flush takes at most 2 s.

The daemon samples every session unless `OTEL_TRACES_SAMPLER` is set on its
side: the sampled flag in the client's trace context does not suppress the
daemon's spans, which then join the client's trace id even when the client
dropped its own. The client keeps the SDK's default sampler (parent-based,
always on).

Export is behind the cargo feature `otel`, on by default.

A build without it ignores the telemetry settings with a warning; see
[DEVELOPMENT.md](../DEVELOPMENT.md#cargo-features) for checking that by hand.
