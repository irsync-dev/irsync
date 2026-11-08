# Performance

How to find out where irsync spends its time, and how to tell whether a
change made it faster. [Watch a transfer with telemetry](../how-to/telemetry.md)
covers irsync's own timings; this page adds the method and the system tools.


## Method

1. **Measure before changing anything.** Pick one workload that shows the
   problem and write down its numbers on the current commit. A guess about
   where the time goes is a hypothesis, not a finding: the cost is often
   somewhere no one expects (SQLite re-preparing a statement on every call,
   for example; see [Known costs](#known-costs)).
2. **Keep the workload fixed and reproducible.** Generate the tree with
   [filegen](filegen.md) from a fixed seed, run the daemon and the client on
   the same machine with release builds, and reset the state the run depends
   on (the destination tree, and the index and resume state described in
   [Files and state](../reference/files-and-state.md#state-directory)) the
   same way every time. Use an
   idle machine: on a busy one, single runs of the same binary vary by up to
   2x.
3. **Compare binaries, not commits in your head.** Build each version you
   compare into its own file (`cp target/release/irsync /some/dir/irsync-old`)
   and interleave their runs: old, new, old, new. Check that each run did the
   same work (`-v` shows files and bytes transferred) and that the trees match
   afterwards (`diff -r`), or the faster run may simply have done less. The
   [Overhead](../reference/metrics.md#overhead) benchmark of the telemetry is
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

## Host setup for perf

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

## Tools

**`-v` summary.** Add `-v` to the client. The block at the end of each
session shows wall time, busy time per activity (walk, plan, hash, write,
waiting on the peer, …), CPU and wire bytes, for the client and the daemon.
Start here to see which phase and which side to look at.

**Telemetry.** `--debug-addr` serves span timings and flamegraph input over
HTTP. These are irsync's own spans, so they show time per phase rather than
per function. See [Watch a transfer with telemetry](../how-to/telemetry.md).

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

**Syscalls per call site.** When `strace -c` shows a count worth cutting,
find which code makes the calls. `strace -k` prints a stack for every
syscall, and `tools/syscall-sites.py` groups the calls by syscall, result
and the innermost irsync frames. Use a small workload (a few hundred
files), since every call writes a full stack:

```sh
CARGO_PROFILE_RELEASE_DEBUG=line-tables-only CARGO_TARGET_DIR=target/dbg \
  cargo build --release -p irsync
strace -f -k -e trace=statx,fstat,newfstatat -o trace.txt \
  target/dbg/release/irsync daemon --config irsync.toml
# ... run the client against it, then stop the daemon ...
tools/syscall-sites.py trace.txt target/dbg/release/irsync
```

Each output line is a count, the syscall and where it was called from,
innermost frame first (the line numbers depend on the build):

```text
     142  statx ENOENT             part_exists (recv.rs:353) < open_from (recv.rs:560) < poll (session/recv_io.rs:461)
     142  statx (fd)               open_locked_part (recv.rs:435) < open_part_in_dir (recv.rs:464) < open_sized_part (recv.rs:1220)
```

Divide by the number of files to get calls per file, then look for one
file paying for the same answer twice: two fstats of the same descriptor,
an lstat whose result the next call would report anyway, a call whose
outcome is already known. Count again after a change; the same command
shows whether each site went away.

Two details the script handles:

- strace prints a frame's offset in the file, while `addr2line` wants a
  virtual address. The script adds the executable segment's address minus
  its offset (`readelf -lW`), and steps each return address back by one
  so it lands in the call.
- With `-f`, a call interrupted by another thread is printed as
  `<unfinished ...>` and later `<... resumed>`, with its stack after the
  resumed line. The script attaches it to the right call.

The line tables do not change the code, so counts from this build match
the normal release build. `-k` needs an strace built with stack-trace
support (`strace -k -e trace=openat true` prints stacks if it has it).

## Known costs

- **SQLite re-preparing a statement.** A statement whose plan depends on a
  bound value (`?2 IS NULL OR path > ?2`, `LIMIT ?3`) can be re-prepared on
  every execution. In `perf report` this shows up as `sqlite3Reprepare`.
  Spell such variants into the SQL text, so each one is prepared and cached
  once.
- **WAL commits empty other connections' page caches.** Each commit makes
  every other connection reread the pages it touched. Fewer, larger commits
  (the index writer batches what arrives within 10 ms, `LINGER` in
  `crates/irsync/src/index.rs`) and
  memory-mapped reads (`PRAGMA mmap_size`) keep that cheap. A larger
  `cache_size` does not help, because the misses are invalidations, not
  capacity.
- **Blocking-pool round trips per file.** With tens of thousands of small
  files, the number of `spawn_blocking` calls per file matters more than
  what each one does. Doing a file's related blocking steps in one call
  saves a thread hand-off, with its futex wake-ups, per step.
- **Syscalls that are expected to fail.** Creating a directory that already
  exists, or checking a path that is not there, costs a syscall per file.
  Trying the operation that usually succeeds, and recovering only when it
  fails, removes them.
