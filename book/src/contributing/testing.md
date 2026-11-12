# Testing

This page is the start of the testing pages. For the case made to users, that
a finished sync leaves the destination exactly like the source under the
conditions stated there, and what backs it, see [How we know irsync is correct](../explanation/correctness.md).
This section is for people who change irsync and need to know which test to
add or run.

## The layers

There are four layers. They go from fast and narrow to slow and broad.

1. **Engine tests.** Each runs a sender and a receiver inside one test
   process and checks the result. They live in `crates/irsync/tests/` and
   run with `cargo test`. With the `test-hooks` feature (see [Cargo
   features](#cargo-features)) a test can inject a fault, slow a step or read
   the receiver's event log. They catch logic bugs in one feature: the
   planner, the receiver, deletes, resume, filters. Reach for them first. A
   fix without one is not finished.
2. **The loopback stress loop.** [`irsync-stress`](stress.md) changes a
   source tree with [filegen](filegen.md), syncs it, and checks the copy with
   `fileset check`, over and over. It catches what no one thought to write a
   test for: odd mixes of files, renames, links, long names and big trees. It
   takes minutes.
3. **netstress.** [`irsync-netstress`](netstress.md) runs the same loop
   through a simulated bad network: slow, lossy, and cut for up to a minute.
   It catches bugs in sessions, reconnect, resume and the transport. It takes
   many minutes and needs its own build.
4. **Campaigns.** [Hunting bugs](bug-hunting.md) turns the stress loop into
   a search: many seeds, with the settings aimed at one part of irsync per
   row of a matrix. It catches the bugs a default run is too gentle to hit. It
   takes hours, so run it before a release or after a risky change, not on
   every push.

## Which test for which change

| You changed | Add or run |
| --- | --- |
| The receiver, the planner, delta or chunk handling | An engine test in `crates/irsync/tests/`. Existing ones to extend: `delta.rs`, `plan.rs`, `recv.rs`, `inplace.rs`, `content_equal.rs`. See [Receiving a changed file](#receiving-a-changed-file). |
| Deletes, filters or options | An engine test: `delete.rs`, `filter.rs`, `filters_e2e.rs`, `push_options.rs`. |
| Code that walks or writes the tree: scan, index, metadata, links, renames | An engine test, then a stress run in both directions: `irsync-stress --direction push` and `--direction pull`. |
| Sessions, reconnect, resume or the transport | An engine test (`resume.rs`), then a netstress run, for example `--profile 4g-vehicle`. |
| Anything risky, or something a default stress run barely touches (hard links, sparse files, odd timestamps, huge listings) | A row of the [campaign matrix](bug-hunting.md#the-campaign-matrix), run with `crates/stress/tools/matrix.sh`. |

A bug that stress or netstress finds should end as an engine test that fails
before the fix and passes after it. The stress run found the bug; the engine
test keeps it from coming back. CI runs the whole test suite on every
push, which includes the engine tests and a short end-to-end test of each
rig. The long stress, netstress and campaign runs are done by hand.

For everyday work, run the suite with the features CI uses:

```sh
cargo test -q --workspace --all-features --no-fail-fast
```

This skips four slow network-fault tests (marked `#[ignore]`) in
`crates/netstress/tests/e2e.rs`. They run the real fault binaries on real
time, take a few minutes together, and the first run also builds those
binaries. The full regression, which
CI and [Before pushing](setup.md#before-pushing) use, runs them too:

```sh
cargo test -q --workspace --all-features --no-fail-fast -- --include-ignored
```

No other test is ignored, so `--include-ignored` adds exactly these four.

The syncs through the fault layer in `crates/irsync/tests/netsim.rs` are
not ignored, though they need minutes of network time: they run client
and daemon in one process on tokio's paused clock, over an in-memory
network (`with_netsim` in `crates/irsync/tests/common`). Whenever every
task waits on a timer, the clock jumps to the next one, so they finish in
about a second. Write a new test of irsync through the fault layer that
way. A test that runs the real fault binaries is ignored like the four.

That relies on everything with a deadline reading tokio's clock
(`tokio::time::Instant`), not `std::time::Instant`. irsync's reconnect,
`--bwlimit` and persistence timers do; telemetry and loops in blocking
threads keep the real clock, since paused time stands still there. iroh
1.3.0 had QUIC read the real clock; the workspace `Cargo.toml` patches in
the fix from [n0-computer/iroh#4676](https://github.com/n0-computer/iroh/issues/4676)
until a release has it.
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

The macOS CI job leaves out `crates/irsync/tests/backpressure.rs`,
`crates/irsync/tests/blocking_pool.rs` and `crates/netstress/tests/e2e.rs`,
through the `ci-macos` profile in `.config/nextest.toml`. The macOS runners
are slow and have 3 cores, and these tests take over a third of the test
time there, while the code they test works the same on every OS. Linux CI
runs them on every push, and a local run on a Mac runs them too. A test that
checks something macOS-specific belongs in none of these files.

## Workload and stress tools

Three crates in the workspace exist to test irsync: `filegen`,
`irsync-stress` and `irsync-netstress`. They are described in the layers
above and on their own pages: [filegen](filegen.md), [Stress
testing](stress.md) and [Network stress with netstress](netstress.md). None
of them links irsync; they drive it through its command line.
`irsync-netstress` reuses the `stress` crate's library. filegen also makes
reproducible trees for benchmarks ([Performance](performance.md#method)).

The end-to-end tests of `stress` and `netstress` run the real binaries.
`cargo test -p stress` does not build the other binaries it needs: run
`cargo build --workspace` first, or use `cargo test --workspace`. The
`netstress` end-to-end tests run only with `-- --include-ignored`; they
build the fault binaries into `target/netsim/debug` themselves the first
time, which takes a few minutes.

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
