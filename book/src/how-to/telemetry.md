# Watch a transfer with telemetry

When a sync is slow, `irsync` can show where the time went (walking, hashing,
waiting on the peer, writing, deleting) and what it cost in CPU, memory, disk
I/O and bytes on the wire. One set of spans and metrics feeds three sinks,
each off until you turn it on. The names and labels are listed in
[Metrics](../reference/metrics.md).

| Sink | Turn it on | What you get |
| --- | --- | --- |
| Summary | `-v` on the client | A block at the end of each session: wall time, busy time per activity, time waiting on the peer, CPU, peak memory, bytes on the wire, RTT and loss. |
| Debug HTTP port | `--debug-addr [IP:]PORT` on the client; `debug_addr` under `[daemon]` for the daemon | `/metrics` in the Prometheus text format, `/debug/timing` with wall and busy time per span name, and `/debug/flame` with folded stacks. |
| OpenTelemetry | `OTEL_EXPORTER_OTLP_ENDPOINT`, or `otlp_endpoint` under `[telemetry]` | Spans and metrics over OTLP. |

With all three off the cost is a few untaken branches; the measurements are in
[Metrics](../reference/metrics.md#overhead).

## Read the summary

Add `-v` to the client:

```sh
irsync -av ~/Photos/ home:photos/
```

At the end of each session it prints a block like this one:

```text
session 1/2  push  ./photos -> nas:/backup  wall 3:12  direct, 1 reconnect
  busy      walk 21.0s  hash 9.3s ×4.0  read 1:02  send 1:48  delete 1.3s
  waiting   peer 44.0s  index 0.7s ×3.0
  transfer  1,882 files  3.41G literal  12.00G matched  17.77M/s
  hashing   2,107 files  15.30G  412.00M/s
  disk      read 15.40G  write 0  fsync 0.0s
  network   sent 3.52G  recv 41.00M  rtt 12ms (max 40ms)  lost 0.02%
  process   cpu 1:12 user, 0:31 sys  peak rss 312.00M  peak fds 214
```

`busy` and `waiting` show the wall time during which each activity ran,
followed by `×N` when on average N tasks, 1.5 or more, ran it at once. `waiting peer` is time this side
had nothing to do. What each activity covers is in
[Busy time and peer wait](../reference/metrics.md#busy-time-and-peer-wait).

## Open the debug port

1. On the client, add `--debug-addr` with a port, or `IP:PORT`; a bare port
   binds `127.0.0.1`:

   ```sh
   irsync -a --debug-addr 9464 ~/Photos/ home:photos/
   ```

   After the sync the client prints `irsync: debug port open at ADDR; Ctrl-C
   to exit` and keeps the port open until SIGINT or SIGHUP, so that you can
   fetch the final numbers. `--debug-port-no-wait` closes the port when the
   sync ends instead.

   On the daemon, set `debug_addr` and restart the daemon:

   ```sh
   irsync config set daemon.debug_addr 127.0.0.1:9464
   ```

   The daemon prints `debug port: ADDR` at start and keeps the port open as
   long as it runs. If the port cannot be bound, it logs a warning and runs
   without it.

2. Fetch what you need:

   ```sh
   curl -s localhost:9464/metrics
   curl -s localhost:9464/debug/timing
   curl -s localhost:9464/debug/flame
   ```

The port speaks plain, unauthenticated HTTP. Keep it on loopback or behind a
firewall.

## Draw a flamegraph

`/debug/flame` serves the self time of each span stack in microseconds, in the
folded form that the `inferno` tools read. It keeps up to 10,000 distinct
stacks and adds the rest up under `[other]`.

1. Install `inferno`:

   ```sh
   cargo install inferno
   ```

2. Run the sync with `--debug-addr 9464`. When it finishes, while the client
   waits for Ctrl-C, render the graph:

   ```sh
   curl -s localhost:9464/debug/flame | inferno-flamegraph > f.svg
   ```

The stacks are `irsync`'s own spans, so the graph shows time per phase and per
file, not per function.

## Trace single files

Per-file spans, `file` and its children, are off by default, because a tree
of tens of thousands of files would swamp a trace. Set `IRSYNC_TRACE_FILES=1`
to send them to the span sinks: OTLP, `/debug/timing` and `/debug/flame`.

```sh
IRSYNC_TRACE_FILES=1 irsync -a --debug-addr 9464 ~/Photos/ home:photos/
```

Set it on the daemon's side to get the daemon's per-file spans.

## Export over OpenTelemetry

1. Point `irsync` at a collector, with the environment or in the config file:

   ```sh
   export OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318
   irsync config set telemetry.otlp_endpoint http://127.0.0.1:4318
   ```

   `[telemetry]` applies to the client and the daemon alike. The daemon reads
   it at start.

2. Run a sync. The client's `session` span starts a trace. The client sends
   its W3C trace context to the daemon, whose `session` span joins it as a
   child when the daemon exports too, so one sync is one trace across both
   machines.

Export is on when `OTEL_EXPORTER_OTLP_ENDPOINT`,
`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` or `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT`
is set, or `otlp_endpoint` is in the config file; the environment wins. The
protocol is `OTEL_EXPORTER_OTLP_PROTOCOL` (`grpc` or `http/protobuf`), else
`otlp_protocol` (`grpc` or `http`), else HTTP.

The other standard `OTEL_*` variables are honoured: headers, timeouts, the
sampler, `OTEL_SERVICE_NAME` (by default `irsync` for the client and
`irsync-daemon` for the daemon), `OTEL_RESOURCE_ATTRIBUTES` and
`OTEL_METRIC_EXPORT_INTERVAL`. Metrics are exported every 10 seconds, unless
`OTEL_METRIC_EXPORT_INTERVAL` says otherwise, and once more at exit. The exit
flush takes at most 2 seconds.

The daemon samples every session unless `OTEL_TRACES_SAMPLER` is set on its
side: the sampled flag in the client's trace context does not suppress the
daemon's spans, which then join the client's trace id even when the client
dropped its own. The client keeps the SDK's default sampler, which follows the
parent and is otherwise always on.

Export needs the cargo feature `otel`, which is on by default. A build
without it warns and ignores the OTLP settings.

## Local collector

The repository has a Docker Compose setup with an OpenTelemetry collector,
Jaeger and Prometheus:
[`compose.yaml`](https://github.com/irsync-dev/irsync/blob/main/docs/telemetry/compose.yaml),
[`collector.yaml`](https://github.com/irsync-dev/irsync/blob/main/docs/telemetry/collector.yaml)
and
[`prometheus.yml`](https://github.com/irsync-dev/irsync/blob/main/docs/telemetry/prometheus.yml).

1. In a clone of the repository, start it:

   ```sh
   docker compose -f docs/telemetry/compose.yaml up -d
   ```

   The collector listens on 4317 (gRPC) and 4318 (HTTP). Jaeger's UI is on
   <http://localhost:16686>, Prometheus on <http://localhost:9090>.
   Prometheus scrapes `host.docker.internal:9464` every 5 seconds.

2. Run a sync that exports over OTLP and opens the debug port on 9464:

   ```sh
   OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318 \
     irsync -a --debug-addr 9464 src/ HOST:export/
   ```

3. Look at the results. Jaeger shows the `session` trace. Prometheus has the
   metrics from the debug port. The metrics sent over OTLP go to the
   collector's `debug` exporter, in its log.

4. Stop it:

   ```sh
   docker compose -f docs/telemetry/compose.yaml down
   ```

## See also

- [Metrics](../reference/metrics.md): every metric, label and span.
- [Configuration](../reference/configuration.md#telemetry): the `[telemetry]`
  keys.
