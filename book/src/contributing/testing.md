# Testing

Run the whole suite with the features CI uses:

```sh
cargo test -q --workspace --all-features --no-fail-fast
```

[Before pushing](setup.md#before-pushing) lists the other checks.

## Cargo features

- `otel` (on by default) builds the OpenTelemetry export. The clippy run
  with `--no-default-features` keeps the build without it compiling.
- `test-hooks` (tests only; irsync's dev-dependency on itself turns it on)
  adds the test fields of `SideHooks` (`crates/irsync/src/session/mod.rs`):
  injected faults, slowed steps, a cap on files in flight, and the
  receiver's event log, which `crates/irsync/tests/delta.rs` checks the
  order of steps against (see [Receiving a changed
  file](#receiving-a-changed-file) below).
- `netsim` builds the fault layer that `irsync-netstress` drives. It needs
  its own build; see [The fault build](netstress.md#the-fault-build).

To check by hand that a build without `otel` ignores the telemetry
settings:

```sh
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318 \
  cargo run -p irsync --no-default-features -- -a src/ HOST:export/
```

stderr starts with `irsync: built without OpenTelemetry support; ignoring
[telemetry]`, and the sync runs as usual.

## Platform skips

Tests that create non-UTF-8 file names skip when the filesystem refuses the
name (APFS does); on Linux they always run. See R29 in
[IRSYNC_DECISIONS.md](https://github.com/irsync-dev/irsync/blob/main/docs/IRSYNC_DECISIONS.md).
The rsync comparison test skips without GNU rsync 3.1 or newer, and fails
instead when `CI` is set ([Requirements](setup.md#requirements)).

## Workload and stress tools

Three crates in the workspace exist to test irsync. None of them links
irsync; they drive it through its command line. `irsync-netstress` reuses
the `stress` crate's library.

- **`filegen`** applies a reproducible, seeded sequence of creates, writes,
  renames, links, deletes and metadata changes to a directory tree, and
  times every step. It is the mutating half of a stress loop: filegen
  changes a tree, `fileset update` records it, irsync carries it to another
  host, and `fileset check` there confirms every change arrived. It also
  makes reproducible trees for benchmarks
  ([Performance](performance.md#method)). See
  [Generating test trees with filegen](filegen.md).
- **`irsync-stress`** closes that loop on loopback: it starts an irsync
  daemon, mutates a source tree with filegen, syncs it and checks the result
  with `fileset check`, keeping logs and both manifests for every failure.
  See [Stress testing](stress.md) and, for campaigns that find bugs,
  [Hunting bugs](bug-hunting.md).
- **`irsync-netstress`** runs the same loop across a simulated bad network:
  a fault layer inside irsync (the `netsim` feature) shapes every QUIC
  packet between client and daemon. See
  [Network stress with netstress](netstress.md) and
  [How netstress works](netstress-internals.md).

The end-to-end tests of `stress` and `netstress` run the real binaries.
`cargo test -p stress` does not build the other binaries it needs: run
`cargo build --workspace` first, or use `cargo test --workspace`. The
`netstress` end-to-end tests build the fault binaries into
`target/netsim/debug` themselves the first time, which takes a few minutes.

## Receiving a changed file

The properties described in
[Receiving a changed file](../explanation/receiving-a-changed-file.md) are
pinned by end-to-end syncs between two engines in `crates/irsync/tests/`. They
read the receiver's event log (`SideHooks::event`, behind the `test-hooks`
feature), which records `prehash`, `plan`, `chunklist`, `need`, `chunk` (a
network chunk written), `copied` (the local copy task finished), `basis`,
`ops` (a chunk verified from operations), `identical`, `clone`, `inplace` and
`append`, each with the path. `delta.rs` checks the order of the steps (the
request leaves before the local copies, which a 150 ms delay per copy makes
visible), a stale local chunk that is re-requested, and that the old file is
hashed once from plan time; `sub_chunk.rs` tests the block codec in
`irsync::delta` on its own; `sub_chunk_delta.rs` measures the bytes of the
sub-chunk delta and its fallbacks; `inplace.rs` covers the cloned `.part`,
`--inplace` and `--append` (the clone tests skip, and say so, where `target/`
is on a filesystem that cannot clone); `content_equal.rs` covers settling in
place and `-c`; `warm.rs` covers same-size candidates and `irsync index`;
`recv.rs` holds the writer-level tests; `plan.rs` pins the planner's
`--append` and `--append-verify` decisions.

To find the code for a step: the early hash is the `Delta` arm of
`session/planner.rs`; opening, comparing, partitioning and choosing bases are
in `session/recv_io.rs` (`open_writer`, `unchanged_file`, `local_sources`,
`basis_specs`, `offer_bases`, `rebuild`, `copy_local_chunks`, `finish_unit`,
`settle_identical`, `finalize`, the in-place `Gate`); dispatch and re-requests
are in `session/receiver.rs` (`on_opened`, `on_drained`, `MAX_REREQUESTS`);
the sender's use of a basis is `session/sender_role.rs` (`on_basis`) and
`send.rs` (`payload`); the codec is `delta.rs`; the writer, cloning and
adoption are `recv.rs` (`FileWriter`, `reflink`); the folded whole-file hash
is `tree_hash.rs`; chunk lists in parts are `session/parts.rs`; the test hooks
are the fields of `SideHooks` in `session/mod.rs`.
