# Telemetry

## OpenTelemetry export

OTLP export of spans and metrics is on when `OTEL_EXPORTER_OTLP_ENDPOINT`
(or `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` / `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT`)
is set, or `[telemetry] otlp_endpoint` is in `irsync.toml`; the environment
wins. The protocol is `OTEL_EXPORTER_OTLP_PROTOCOL` (`grpc` or
`http/protobuf`), else `[telemetry] otlp_protocol` (`grpc` or `http`), else
HTTP. The other standard `OTEL_*` variables (headers, service name,
sampler, timeouts, `OTEL_METRIC_EXPORT_INTERVAL`) are honoured. Metrics are
exported every 10 s and once more at exit; the exit flush takes at most 2 s.

Export is behind the cargo feature `otel`, on by default.

### Manual check: a build without `otel`

```sh
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318 \
  cargo run -p irsync --no-default-features -- -a src/ HOST:export/
```

stderr starts with `irsync: built without OpenTelemetry support; ignoring
[telemetry]`, and the sync runs as usual. CI checks that this build stays
clippy-clean (`cargo clippy -p irsync --no-default-features --all-targets`).
