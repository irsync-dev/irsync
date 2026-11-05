# Developing irsync

Notes for working on irsync itself: setting up a clone, the checks to run
before pushing, testing, where the design documents are, and how to profile
and tune performance.

## Setup

- **Rust:** `rust-toolchain.toml` pins the stable channel; rustup picks it
  up.
- **GNU rsync 3.1 or newer on `PATH`**, for `crates/irsync/tests/rsync_compare.rs`,
  which compares irsync's result with rsync's. Without it the test prints a
  line and passes, which `cargo test -q` hides, so it silently checks
  nothing. On macOS the bundled rsync (2.6.9 or openrsync) is too old:
  `brew install rsync`. With `CI` set, the test fails instead of skipping.
- **The pre-push hook.** The repository's git remote is not on GitHub, so
  the CI workflow in `.github/workflows/ci.yml` does not run on pushes. The
  hook in `.githooks/` enforces its formatting check locally instead: every
  commit being pushed is checked with `cargo fmt --all -- --check` in a
  throwaway worktree, and the push is refused if any file is not
  rustfmt-clean. Install it once per clone:

  ```sh
  git config core.hooksPath .githooks
  ```

## Before pushing

Clippy and the tests are not part of the hook; run them yourself:

```sh
touch crates/irsync/build.rs    # re-runs the build script, which bakes in the git commit
cargo test -q --workspace --all-features --no-fail-fast
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
```

`ci.yml` runs the same checks on Linux and macOS, should the repository
move to GitHub.

## Testing

### Cargo features

- `otel` (on by default) builds the OpenTelemetry export. The clippy run
  with `--no-default-features` keeps the build without it compiling.
- `test-hooks` (tests only; the dev-dependency on `irsync` turns it on)
  adds the test fields of `SideHooks` (`crates/irsync/src/session/mod.rs`):
  injected faults, slowed steps, a cap on files in flight, and the
  receiver's event log, which `crates/irsync/tests/delta.rs` checks the
  order of steps against (see [docs/DELTA_RECEIVE.md](docs/DELTA_RECEIVE.md),
  section 5).

To check by hand that a build without `otel` ignores the telemetry
settings:

```sh
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318 \
  cargo run -p irsync --no-default-features -- -a src/ HOST:export/
```

stderr starts with `irsync: built without OpenTelemetry support; ignoring
[telemetry]`, and the sync runs as usual.

### Platform skips

Tests that create non-UTF-8 file names skip when the filesystem refuses the
name (APFS does); on Linux they always run. See R29 in
[docs/IRSYNC_DECISIONS.md](docs/IRSYNC_DECISIONS.md).

### Workload and stress tools

Two crates in the workspace exist for testing irsync. Neither shares code
with it.

**`filegen`** is a deterministic, resumable filesystem workload generator:
from a seed it applies a reproducible sequence of creates, writes, renames,
links, deletes and metadata changes to a directory tree, logging and timing
every step. It is the mutating half of a stress loop: filegen changes a
tree, `fileset update` records it, irsync carries it to another host, and
`fileset check` there confirms every change arrived. It is also the way to
make a reproducible tree for benchmarks (see
[Performance tuning](#performance-tuning)).
See [crates/filegen/README.md](crates/filegen/README.md).

**`irsync-stress`** closes the loop: it starts an irsync daemon on loopback
and repeatedly mutates a source tree with filegen, syncs it with irsync, and
verifies the result with `fileset check`, keeping logs, both manifests and a
`run.toml` for every failure. Its first runs found two real irsync bugs
(long file names failed to transfer, FIFO timestamps never updated), both
since fixed. See [crates/stress/README.md](crates/stress/README.md), and
[docs/STRESS_BUG_HUNTING.md](docs/STRESS_BUG_HUNTING.md) for how to make it
find more.

Its end-to-end test needs the other binaries, which `cargo test -p stress`
alone does not build: run `cargo build --workspace` first, or use
`cargo test --workspace`.

## Design documents

| Document | What it holds |
| --- | --- |
| [docs/protocol.md](docs/protocol.md) | The wire protocol. Protocol and on-disk state change without compatibility paths until the first release. |
| [docs/DELTA_RECEIVE.md](docs/DELTA_RECEIVE.md) | How the receiver fills a changed file: the overlapped delta algorithm. |
| [docs/IRSYNC_VS_RSYNC.md](docs/IRSYNC_VS_RSYNC.md) | irsync's transfer algorithm compared critically with rsync's, and the remedies that came of it. |
| [docs/IRSYNC_DECISIONS.md](docs/IRSYNC_DECISIONS.md) | Design and implementation decisions of v1 that the code does not show, and the points knowingly left open. |
| [docs/FILESET_DECISIONS.md](docs/FILESET_DECISIONS.md), [docs/FILEGEN_DECISIONS.md](docs/FILEGEN_DECISIONS.md), [docs/IRSYNC_HARNESS_DECISIONS.md](docs/IRSYNC_HARNESS_DECISIONS.md) | The same for fileset, filegen and irsync-stress; the last also lists the irsync bugs the harness found. |
| [docs/STRESS_BUG_HUNTING.md](docs/STRESS_BUG_HUNTING.md) | A strategy for finding bugs with irsync-stress. |
| [docs/telemetry.md](docs/telemetry.md) | Spans, metrics and their sinks. |
| [docs/daemon.md](docs/daemon.md) | Running the daemon. |
| `docs/superpowers/` | The specs (`specs/`), implementation plans (`plans/`) and progress ledgers (`ledgers/`) features were built from. |
| [TODO.md](TODO.md) | Open ideas, in no particular order. |

## Performance tuning

### Method

1. **Measure before changing anything.** Pick one workload that shows the
   problem and write down its numbers on the current commit. A guess about
   where the time goes is a hypothesis, not a finding: in this project the
   cost was more than once somewhere no one expected (SQLite re-preparing a
   statement on every call, for example).
2. **Keep the workload fixed and reproducible.** Generate the tree with
   `filegen` from a fixed seed, run the daemon and the client on the same
   machine with release builds, and reset the state the run depends on (the
   destination, the index after `irsync gc`) the same way every time. Use an
   idle machine: on a busy one, single runs of the same binary vary by up to
   2x.
3. **Compare binaries, not commits in your head.** Build each version you
   compare into its own file (`cp target/release/irsync /some/dir/irsync-old`)
   and interleave their runs: old, new, old, new. Check that each run did the
   same work (`-v` shows files and bytes transferred) and that the trees match
   afterwards (`diff -r`), or the faster run may simply have done less. The
   [Overhead](docs/telemetry.md#overhead) benchmark in the telemetry docs is
   a worked example: interleaved runs, pooled medians, and CPU time
   reported beside wall time.
4. **Prefer counts to times.** CPU time, context switches and syscall counts
   vary far less between runs than wall time, and they say what changed.
   Report wall time too, but over several runs.
5. **Find the cause before fixing it.** Go from the coarse to the fine: the
   `-v` summary for the phase, `perf stat` for CPU against waiting,
   `perf record` for the hot code, `strace -c` for the syscalls. When a
   profile shows a cost but not its caller, count the calls per call site
   with temporary code (a counter keyed by `#[track_caller]`'s location
   works well), then remove it.
6. **Change one thing, then measure again** with the same workload and the
   same commands. Bisect when a regression's cause is unclear: build the
   commits in between with `git worktree` and run each.
7. **Record the result** in the commit message: the workload, the numbers
   before and after, and what did not change.

### Host setup for perf

Linux restricts what an unprivileged `perf` may see. Two sysctls matter:

| Setting | Value needed | Why |
| --- | --- | --- |
| `kernel.perf_event_paranoid` | `1` or lower | Lets your user record kernel samples of its own processes. At `2` perf sees user space only; some distributions default higher still, which blocks unprivileged `perf` altogether. |
| `kernel.kptr_restrict` | `0` | Shows kernel symbol addresses in `/proc/kallsyms`. At `1`, your user sees all-zero addresses, so perf records kernel samples but cannot name them. |

Check the current values first, so you can restore them:

```sh
sysctl kernel.perf_event_paranoid kernel.kptr_restrict
```

**One-off**, until the next reboot:

```sh
sudo sysctl kernel.perf_event_paranoid=1
sudo sysctl kernel.kptr_restrict=0
```

**Persistent**, applied at every boot:

```sh
sudo tee /etc/sysctl.d/99-perf.conf <<'EOF'
kernel.perf_event_paranoid = 1
kernel.kptr_restrict = 0
EOF
sudo sysctl --system    # applies it now as well
```

To undo it, delete `/etc/sysctl.d/99-perf.conf` and reboot, or set the values
you noted back with `sysctl`.

`head -2 /proc/kallsyms` shows non-zero addresses once `kptr_restrict` is in
effect.

These settings weaken the kernel's defences slightly. Every local user can
then profile its own processes in kernel mode and read kernel addresses,
which makes kernel exploits easier. That is fine on a single-user
development machine. Do not leave them set on a shared or production host.

### Tools

**`-v` summary.** Add `-v` to the client. The block at the end of each
session shows wall time, busy time per activity (walk, plan, hash, write,
waiting on the peer, …), CPU and wire bytes, for the client and the daemon.
Start here to see which phase and which side to look at.

**Telemetry.** `--debug-addr` serves span timings and flamegraph input over
HTTP. These are irsync's own spans, so they show time per phase rather than
per function. See [docs/telemetry.md](docs/telemetry.md).

**`perf stat`.** CPU time and context switches for a whole process:

```sh
perf stat -e task-clock,context-switches irsync daemon --config irsync.toml
```

High CPU points at computation. Many context switches point at threads
handing work to each other (with tokio, every `spawn_blocking` round trip
costs futex wake-ups). Wall time much longer than CPU time means the process
is waiting: on disk, on the network, or on a lock.

**`perf record`.** Where the CPU time goes, with call stacks. Build with frame
pointers into a separate target directory, so the normal build stays cached:

```sh
RUSTFLAGS="-C force-frame-pointers=yes" CARGO_TARGET_DIR=target/fp \
  cargo build --release -p irsync
perf record -F 999 -g -o perf.data target/fp/release/irsync daemon --config irsync.toml
perf report -i perf.data --no-children
```

The prebuilt standard library has no frame pointers, so stacks that pass
through it can be cut short. `perf record --call-graph dwarf` avoids that,
at the cost of much larger data files. Kernel frames need the host setup
above.

**`strace -c`.** Syscall counts and errors per syscall:

```sh
strace -f -c -o strace.txt irsync daemon --config irsync.toml
```

The `errors` column is often the useful part: tens of thousands of failing
calls (a `mkdir` getting `EEXIST`, a `statx` getting `ENOENT`) are work that
can usually be skipped. strace slows the traced process by an order of
magnitude, so use its counts, not its times.

### Things this project has run into

- **SQLite re-preparing a statement.** A statement whose plan depends on a
  bound value (`?2 IS NULL OR path > ?2`, `LIMIT ?3`) can be re-prepared on
  every execution. In `perf report` this shows up as `sqlite3Reprepare`.
  Spell such variants into the SQL text, so each one is prepared and cached
  once.
- **WAL commits empty other connections' page caches.** Each commit makes
  every other connection reread the pages it touched. Fewer, larger commits
  (the index writer batches what arrives within a few milliseconds) and
  memory-mapped reads (`PRAGMA mmap_size`) keep that cheap. A larger
  `cache_size` does not help, because the misses are invalidations, not
  capacity.
- **Blocking-pool round trips per file.** With tens of thousands of small
  files, the number of `spawn_blocking` calls per file matters more than
  what each one does. Doing a file's related blocking steps in one call cut
  daemon CPU noticeably.
- **Syscalls that are expected to fail.** Creating a directory that already
  exists, or checking a path that is not there, costs a syscall per file.
  Trying the operation that usually succeeds, and recovering only when it
  fails, removes them.
