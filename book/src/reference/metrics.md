# Metrics

The metrics and spans that `irsync` emits, for the client and the daemon. How
to collect them is in [Watch a transfer with telemetry](../how-to/telemetry.md).

## Labels

Every `irsync_*` metric carries `side` (`client` or `daemon`) and `role`
(`sender` or `receiver`). The daemon's per-session metrics also carry
`export`. There are no path or session-id labels, to keep the number of series
small; the session id is on the `session` span.

The busy-time and active-time counters, the `irsync_bytes_*` counters,
`irsync_files_hashed_total` and the disk counters are process-wide meters,
published once a second and at exit. They add up across the exports a daemon
serves and carry no `export` label.

The `process_*` metrics carry only `side`, and `mode` for CPU: they describe
the process, not a session.

The Prometheus names below are also the OTLP names. OTLP units follow the Unit
column.

## Counters

| Name | Unit | Extra labels | Meaning |
| --- | --- | --- | --- |
| `irsync_busy_microseconds_total` | microseconds | `activity` | Time spent per activity, summed over tasks: `walk`, `plan`, `hash`, `read`, `send`, `recv`, `write`, `place`, `delete`, `index_wait`, and `peer_wait`. See [Busy time and peer wait](#busy-time-and-peer-wait). |
| `irsync_active_microseconds_total` | microseconds | `activity` | Wall time with at least one task doing each activity, without `peer_wait`, added when a session ends. |
| `irsync_sessions_total` | count | `outcome` | Sessions ended: `ok`, `partial`, `failed`, `max_delete`. Counts connection attempts: a client session that reconnects counts `failed` and then `ok`. |
| `irsync_files_total` | count | `result` | `transferred`, `skipped`, `failed`, `created`. `failed` counts the entries with at least one error, each once; errors about the session as a whole, such as an incomplete listing or a refused deletion pass, are not counted. |
| `irsync_entries_listed_total` | count | `kind` | Source entries listed: `regular`, `directory`, `symlink`, `special`. |
| `irsync_bytes_literal_total` | bytes | | File data sent or received as literal chunk bytes. |
| `irsync_bytes_matched_total` | bytes | | File data the receiver did not take from the sender (local copies, earlier progress, files already identical), counted when each file is placed; the sender counts what the receiver's acknowledgement reports. |
| `irsync_bytes_hashed_total` | bytes | | Bytes the hashers read. |
| `irsync_files_hashed_total` | count | | Files the hashers read. |
| `irsync_disk_read_bytes_total` | bytes | | File data `irsync` read from disk. |
| `irsync_disk_write_bytes_total` | bytes | | File data `irsync` wrote to disk. |
| `irsync_wire_bytes_total` | bytes | `dir` | Bytes on the QUIC connection: `tx` or `rx`. |
| `irsync_deleted_total` | count | | Paths the receiver deleted. |
| `irsync_reconnects_total` | count | `reason` | Client reconnects: `dial` (no connection yet) or `lost`. |
| `irsync_quic_lost_packets_total` | count | | QUIC packets lost, sampled once a second. |

## Histograms

| Name | Unit | Extra labels | Meaning |
| --- | --- | --- | --- |
| `irsync_phase_duration_seconds` | seconds | `phase` | Wall time of each session phase: `connect`, `handshake`, `list`, `plan`, `transfer`, `delete`, `reconnect`. Phases overlap. |
| `irsync_file_transfer_seconds` | seconds | | From a file's first request to its completion. |
| `irsync_hash_wait_seconds` | seconds | | Time a role waited on a hash result. |
| `irsync_fsync_seconds` | seconds | | Time to fsync a received file. |
| `irsync_file_size_bytes` | bytes | | Size of each transferred file. |
| `irsync_quic_rtt_seconds` | seconds | | QUIC path RTT, sampled once a second per connection. |

## Gauges

| Name | Unit | Extra labels | Meaning |
| --- | --- | --- | --- |
| `irsync_sessions_active` | count | | Sessions the daemon is serving. |
| `irsync_streams_open` | count | | Open data streams. |
| `irsync_hash_queue_depth` | count | | Hash jobs waiting for a worker. |
| `process_cpu_seconds_total` | seconds | `mode` (`user`, `sys`) | CPU time of the process. |
| `process_max_resident_memory_bytes` | bytes | | Peak resident set size. |
| `process_open_fds` | count | | Open file descriptors. |

## Busy time and peer wait

`irsync_busy_microseconds_total{activity}` counts microseconds, not seconds,
because the `metrics` crate has only integer counters and seconds would lose
the fraction. Each activity counts the time tasks spent doing it, summed over
every task.

- `send` and `recv` include network and flow-control waits on the data
  streams: a stream blocked on the peer's window, or paused by `--bwlimit`, is
  still busy sending.
- `peer_wait` is wall time with no activity in progress at all: this side has
  no local work and waits for the peer.
- An activity that runs on the other side shows up here as `peer_wait`. For
  example, on a push the daemon plans, so the client's planning time is
  `peer_wait`, not `plan`.
- `index_wait` is time spent waiting on the index: for its lock while another
  task holds it, or for the entries this side queued to be written. The index
  is one SQLite database per root. Placed and hashed files, and entries of
  copy sources found gone, are queued for a writer thread that writes them in
  batches: whatever queues up within 10 ms of the first entry, since every
  commit empties the read connections' page caches. Nobody waits for that,
  except once at the end of a session and before a lookup that needs freshly
  hashed files. The rarer writes, such as saving progress, take turns behind
  one lock, and lookups run on read connections of their own. The waiting
  task's own activity, `write` or `place`, counts the same time too.
- `hash` on a receiver is the check of each arriving chunk against its hash,
  and the hashing of existing files. On a sender it includes encoding a chunk
  against the receiver's bases, that is matching its blocks.
- `place` covers every entry made or updated at the destination: placing
  received files, creating directories, symlinks and special files, and
  updating an entry's metadata alone. Deciding what to do with an entry is
  `plan`.

Busy times can add up to more than the wall time: files are hashed, sent and
written in parallel, so ten workers busy for one second each count ten
seconds. They say where the work is, not how long the run took. `peer_wait` is
the only one that cannot overlap the others.

`irsync_active_microseconds_total{activity}` is the other half: the wall time
during which at least one task was doing the activity, so it never exceeds the
wall time. Busy divided by active is how many tasks did it at once, on
average. The `-v` summary shows the active time, followed by `×N` when that
average is 1.5 or more: `place 18.2s ×14.8` is 18.2 s of the run during which
about fifteen files were being placed at a time. `index_wait` appears there as
`index` on the `waiting` line.

## Spans

Span names are stable. The per-file spans, `file` and its children, reach the
span sinks only with `IRSYNC_TRACE_FILES=1`.

```text
session            session_id, direction, side, role, endpoint_id, peer,
 |                 resumed, source_index (client); at the end rtt_max_ms, cwnd,
 |                 lost_packets, reconnects
 +- connect        attempt, path          (client only, one per attempt)
 +- handshake
 +- list
 +- plan
 +- transfer
 |   +- file       path, size, kind (new or delta)   [IRSYNC_TRACE_FILES=1]
 |       +- hash   (sender)
 |       +- send   (sender)
 |       +- recv   (receiver)
 |       +- place  (receiver: rename, metadata, fsync)
 +- delete         mode                   (each deletion pass)
 +- reconnect      attempt, reason        (wait between attempts)
```

`path` on `connect` is `direct` or `relay`. `session` is a root span per
source. On the client it starts a new trace. On the daemon it is a child of
the client's `session` span when the daemon exports over OTLP: the client's
W3C trace context travels in its first message, so one sync is one trace
across both machines. The local `EndpointId` is the `endpoint_id` field on
`session` spans, not a resource attribute, and it is recorded only while spans
are exported over OTLP. Events logged inside a span pick up its context.

Log lines show that context too. The daemon's lines, and the client's under
`-v`, start with the span stack and its fields, as in
`session{session_id=… side="daemon" …}:transfer: …`.

## Overhead

The cost of the telemetry was measured as a push of a generated tree to a
local daemon on the same machine: `irsync -a --no-resume`, an empty
destination, a warm source index, release builds. The baseline is a build
without the telemetry code.

The tree is `filegen run --seed 1 --steps 600000 --max-total-size 2G
--max-file-size 192K`: 22,000 files, 1.7 GiB, on tmpfs, so that disk speed
does not enter. The machine was shared and noisy, and single runs of the same
binary varied by up to 2x. The runs were therefore interleaved in random
order, and the table pools 23 runs per configuration over three batches. The
last two columns are from the last batch, 10 runs, as medians, and add the
client's and the daemon's CPU time, which is far steadier than wall time.

| Configuration | Wall median, 23 runs | vs baseline | Wall median, 10 runs | CPU median, client + daemon | vs baseline |
| --- | --- | --- | --- | --- | --- |
| baseline | 11.59 s | | 11.97 s | 44.4 s | |
| sinks off | 11.64 s | +0.4% | 10.50 s | 43.5 s | -2.1% |
| all sinks on | 11.26 s | -2.8% | 11.45 s | 46.5 s | +4.6% |

"All sinks on" is `-v`, the debug port on the client and the daemon, and OTLP
over HTTP on both sides. The OTLP endpoint was a local HTTP server that
answers every request with 200, so serialising and sending are measured and a
collector's own work is not. Against sinks off, the median wall time with
everything on is 3.3% lower (pooled) and the CPU time 6.8% higher (10 runs).
The extra CPU is the once-a-second sampler, the 22,000 `-v` lines and the
exporters, running on otherwise idle cores.
