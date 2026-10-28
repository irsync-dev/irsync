# irsync stress harness — design

Date: 2026-10-21
Status: implemented 2026-10-21 (plan: `../plans/2026-10-21-stress-harness.md`)

## Intent

irsync, fileset and filegen were built as three legs of one loop: filegen
mutates a tree, fileset records it, irsync carries tree and manifest to the
other side, fileset checks that everything arrived. Each leg exists and is
tested on its own. Nothing yet runs the loop.

This project adds the fourth piece: a harness that sets up a client and a
daemon on one machine, drives that loop for a chosen number of iterations,
and stops at the first discrepancy with enough left on disk to understand
what went wrong. It is both the long-running stress test a developer starts
before going home and, with tiny parameters, the integration test CI runs on
every push.

The isolated network mode (design
`docs/superpowers/specs/2026-10-21-isolated-network-design.md`) was built
for this: the daemon binds loopback and reports its port, the client dials
that port directly, and nothing leaves the machine.

## Goals

- One command runs the whole loop from a seed, in either direction, for N
  iterations, and exits non-zero at the first failed verification.
- A failed run leaves behind, in one place: the seed and parameters the run
  started from, the manifest describing what the destination should have
  contained, fileset's report of how it differed, and detailed logs of the
  client and the daemon for the iteration that failed.
- A run is reproducible: the same seed and parameters produce the same
  sequence of trees on the source side.
- A small instance of the harness runs under `cargo test` in CI on Linux
  and macOS, so the three tools are exercised together on every push.

## Non-goals

- Negative authorization tests (a key that must be rejected, a read-only
  module that must refuse a push). The harness checks that the configured
  identity is accepted and nothing more.
- Fault injection: killing the client or the daemon mid-transfer to
  exercise resume, dropping the connection, filling the disk.
- Resuming an interrupted stress run. A run that was stopped is started
  again from scratch with a fresh root directory.
- Running across two machines. Both sides live on the host the harness runs
  on, and the daemon is reached through loopback only.
- Performance measurement beyond the per-iteration wall time the harness
  prints. filegen's log already times every filesystem operation, and
  irsync's `--stats` can be passed through.

## Anti-goals

- A run that fails must not clean up after itself. Deleting the root
  directory on failure would destroy exactly what the run exists to
  produce. (On success the root is kept as well: the tree can be tens of
  gigabytes, and deleting it is the user's decision.)
- The harness must not touch the network beyond loopback. The daemon runs
  with relay, DHT, DNS, mDNS and the port mapper off; the client is given
  `--address` and therefore uses none of them.
- The harness must not share code with irsync's verification. It drives
  the `fileset` binary, which shares nothing with irsync, and compares
  manifests with its own BLAKE3 call. A bug in irsync's hashing must not
  be able to hide itself.

## Constraints

- Rust, in the existing cargo workspace: fmt clean, clippy clean with
  warnings denied, tests pass on Linux and macOS (CI in
  `.github/workflows/ci.yml`).
- The three tools are driven as subprocesses through their command lines,
  not linked as libraries. Their command lines are the contract that the
  stress loop in `crates/filegen/README.md` and `crates/fileset/README.md`
  already describes, and running the real binaries is the point.
- irsync's daemon prints `endpoint id: <id>` and then one `listening on:
  <addr>` line per bound socket to stdout, flushed before it accepts
  connections. That is the only way to learn a port-0 daemon's port.
  `crates/irsync/tests/isolated_e2e.rs` shows the full recipe: `id generate`,
  authorized_keys, daemon config, spawn, parse, dial with `--address`.
- filegen refuses an output directory inside the tree it mutates, and a
  `--seed` run refuses to start if its log or state file already exists.
  Its state file is `<out_dir>/filegen-<seed>.state.yaml` and a resumed run
  appends to the same log.
- fileset stores the manifest at `<root>/.fileset.sqlite` and knows to skip
  its own manifest and SQLite sidecar files when checking. Closing the
  SQLite store leaves no sidecars, so the manifest file is a complete,
  byte-stable artefact between runs. The fjall backend is not used: it
  touches lock and journal files on open, which would defeat the checksum
  comparison.
- irsync's client passes only delete, dry-run, itemize, `-u` and `-c` to the
  daemon. On a push the daemon applies full archive metadata regardless of
  the client's flags; on a pull the client's `-a` does it. Owner and group
  changes need root on the receiving side, so they cannot be relied on.
- Logging on both irsync binaries is `tracing` to stderr, with `RUST_LOG`
  overriding the verbosity flags.

## Behaviour

### Command

A new workspace crate `crates/stress` building one binary, `irsync-stress`:

```
irsync-stress [OPTIONS] --direction <push|pull> <ROOT> [-- <CLIENT ARGS>...]
```

- `<ROOT>`: directory for the run. Created if missing; must be empty if it
  exists. Everything the run produces lives below it.
- `--direction push|pull` (required): push means the daemon serves the
  destination and the client sends; pull means the daemon serves the
  source and the client receives.
- `--iterations N` (default 10): sync-and-check rounds.
- `--seed S` (default: drawn from the OS and printed): filegen's seed for
  the whole run.
- `--steps N` (default 100): filegen steps per filegen invocation,
  including the first one that populates the empty tree.
- `--max-total-size SIZE` (default: not passed, so filegen's 25G applies):
  handed to filegen as its cap on the tree.
- `--max-file-size SIZE` (default: not passed, so filegen's 256M applies):
  handed to filegen. Needed whenever the total cap is below 256M, because
  filegen refuses a per-file limit above the total.
- `--filegen-set KEY=VALUE` (repeatable): passed to every filegen
  invocation as `--set KEY=VALUE`, so any filegen configuration key can be
  changed without editing the harness. The first filegen run records them
  in its state file; resumed runs inherit them from there, but the harness
  passes them on every invocation anyway so the command lines in the logs
  are self-contained.
- `--log-level LEVEL` (default `debug`): the `RUST_LOG` value for the
  client and the daemon. `trace` is available for a short run that needs
  it.
- `--keep-going`: do not stop at the first failed check; record it, keep
  the failed iteration's artefacts, and continue. Off by default.
- Everything after `--` is appended verbatim to every sync invocation of
  the client, after the harness's own flags, so it can add `--parallel`,
  `--bwlimit`, `--stats` or anything else the client accepts. It cannot
  remove the harness's flags.

### Layout under ROOT

```
ROOT/
  run.toml                 parameters of this run, written before anything else
  SRC/
    config/                client or daemon config, depending on direction
    data/                  the tree filegen mutates; fileset manifest lives here
  DST/
    config/                the other side's config
    data/                  where the tree arrives; checked here
  filegen/                 filegen's log and state file (outside both trees)
  logs/
    daemon.log             daemon stderr, whole run, with iteration markers
    ping.client.log        client stderr for the sentinel round
    iter-0001.client.log   client stderr per iteration
    iter-0001.check.txt    fileset check stdout+stderr per iteration
    iter-0001.manifest.blake3   both manifests' hashes per iteration
```

`run.toml` holds the seed, direction, iterations, steps, size caps, filegen
settings, log level, extra client arguments, the three binaries' paths and `--version`
output, the daemon's endpoint id and bound address once known, and the
start time. Reading it must be enough to repeat the run.

The config directories hold what each side would have in `~/.config/irsync`
and `~/.local/state/irsync` on a real machine: the daemon side gets
`irsyncd.toml`, its identity, `authorized_keys` and a `state/` directory;
the client side gets its key pair and a client config file. The client is
run with `HOME` and `XDG_STATE_HOME` pointed below its config directory, as
the e2e test does, so no state leaks into the developer's own
`~/.local/state/irsync`, and with `IRSYNC_CONFIG` and `IRSYNC_PASSPHRASE`
unset. The daemon's identity is generated by the daemon itself on first
start; the client's with `irsync id generate --out`.

The daemon serves one module, `data`, whose path is `DST/data` on a push
and `SRC/data` on a pull, read-write in both cases. The client's
authorized_keys line grants `data:rw`.

### Sequence

1. Validate arguments, create the layout, write `run.toml`.
2. Start the daemon with `--listen 127.0.0.1:0` and the isolated config
   (`relay_mode = "disabled"`, `portmap = false`, all discovery off), stdout
   piped, stderr to `logs/daemon.log`. Read the endpoint id and the bound
   address from stdout. If no `listening on:` line arrives within a bound
   (builder's choice, on the order of 15 s), fail with exit 2.
3. Sentinel round. Write `SRC/data/.irsync-stress-ping` containing the seed
   and a timestamp. Sync once (same flags as an iteration). Verify
   `DST/data/.irsync-stress-ping` exists with identical bytes. A failure
   here is a setup failure, exit 2, since nothing about filegen or fileset
   is involved yet. The file stays in the tree; filegen may later modify or
   delete it like any other file.
4. First fill: `filegen run --seed S --steps N --out-dir ROOT/filegen`
   (plus `--max-total-size`, `--max-file-size` and `--set` as given) on
   `SRC/data`.
5. `fileset init -q SRC/data`.
6. For each iteration i from 1 to N:
   1. Sync. Record wall time. Client exit 0 is a success, exit 23 (partial
      transfer: the session finished but some files failed) is a failed
      iteration, since it means irsync could not carry what filegen made.
      Any other exit code (usage or connection errors, `--max-delete`) is a
      harness failure, exit 2. After exit 23 the manifest comparison and
      the check still run, so the report shows which files were affected.
   2. Compare BLAKE3 of `SRC/data/.fileset.sqlite` and
      `DST/data/.fileset.sqlite`, computed by the harness. Write both hashes
      to `logs/iter-i.manifest.blake3`. A mismatch is a failed iteration.
   3. `fileset check -q DST/data`, output captured to `logs/iter-i.check.txt`.
      Exit 1 from fileset is a failed iteration; exit 2 is a harness
      failure (exit 2 overall).
   4. Print the iteration line. If the iteration failed and `--keep-going`
      is not set, stop here: leave the daemon running long enough to flush,
      then terminate it, and exit 1.
   5. If this is not the last iteration: `filegen run --resume
      ROOT/filegen/filegen-S.state.yaml --steps N SRC/data`, then
      `fileset update -q SRC/data`.
7. Terminate the daemon (SIGINT, which is the signal it handles, then
   SIGKILL after a grace period), wait for it, print the summary, exit 0 if every iteration passed, 1 otherwise.

Every subprocess failure that is not one of the expected verification
outcomes (filegen exits non-zero, fileset update fails, the client exits
with a code other than 0 or 23 on a sync, the daemon dies) stops the run with exit 2 and the
child's stderr is already on disk in `logs/`. The daemon is terminated on
every exit path, including panics and SIGINT, so a stress run never leaves
a stray daemon behind.

### The sync invocation

```
irsync -a -H --delete --reconnect 0 \
    --address 127.0.0.1:<port> --identity SRC-or-DST/config/id_ed25519 \
    -F <client config> [extra client args...] <source> <destination>
```

where on a push `<source>` is `SRC/data/` and `<destination>` is
`<id>:data/`, and on a pull the two are swapped and `<destination>` is
`DST/data/`. Trailing slashes on both local paths. `-H` because filegen
creates hard links; `--delete` because filegen removes files and
directories, and without it every removal would show up as an `ADD` in the
check. `--reconnect 0` so a daemon that dies makes the client fail at once
rather than redial for a day. The client's stderr goes to the iteration's
log file; its stdout is discarded unless the extra arguments ask for
output, in which case it is appended to the same log.

Verbosity: the harness sets `RUST_LOG` on both children from
`--log-level`; it does not pass `-v`.

### Iteration markers in the daemon log

The harness owns the daemon's stderr pipe and copies it to
`logs/daemon.log` on a thread. Before each sync it writes a line
`=== iteration <i> ===` (and `=== ping ===` for the sentinel round) into
that file through the same writer, so the daemon's lines for a given
iteration can be found by searching for the marker. Ordering between the
marker and lines the daemon wrote just before is best effort; the marker
precedes anything the daemon logs about the connection, which is what
matters.

### Output

To stdout, one line per iteration:

```
iter 0001/0100  sync 12.3s  exit 0   manifest ok  check ok
iter 0002/0100  sync 11.8s  exit 23  manifest ok  check FAILED (added=0 deleted=1 changed=2)
```

followed on failure by the path of the check report. A header before the
first iteration names the root, the seed, the direction and the daemon's
endpoint id, and a footer gives the count of passed and failed iterations
and the total time. Exit codes: 0 all iterations passed, 1 at least one
iteration failed verification, 2 the harness or a tool could not run.

### Verification tier

`fileset check` runs at its default tier: type, content, symlink target,
device numbers, mode, mtime and size. Owner, group, link count and xattrs
are not compared: owner and group need root to transfer, and link count is
already exercised by `-H` transferring the links themselves. The harness
does not expose the tier; a developer wanting `--paranoid` runs fileset by
hand against the directories the run left behind.

### CI integration test

A test in `crates/stress/tests/` runs the built `irsync-stress` binary
(through `CARGO_BIN_EXE_irsync-stress`) in a temporary directory with
`--iterations 2 --steps 20 --max-total-size 8M --max-file-size 1M`, once
per direction, and
asserts exit 0 and that the expected files exist under the root. The
harness locates the three tool binaries by looking next to its own
executable first (which is where cargo puts them for a workspace build and
test) and then on `PATH`; a `--irsync`, `--filegen` and `--fileset` flag
each override. The test depends on the three other crates' binaries being
built, which `cargo test --workspace` guarantees through the workspace;
the builder verifies this holds for `cargo test -p stress` as well, and if
it does not, adds the three crates as dev-dependencies or documents that
the test needs a workspace build.

### Documentation

A `crates/stress/README.md` in the style of the other two tool READMEs:
the command, the layout, what to look at after a failure, and the exit
codes. The workspace README gains a short section pointing to it, next to
the fileset and filegen sections.

## Testing

- Argument parsing, including the `--` passthrough and the empty-root rule.
- Layout creation and `run.toml` contents, in a temporary directory,
  without starting anything.
- Daemon startup parsing against a fake child that prints the two lines
  (and one that prints nothing, to cover the timeout).
- The BLAKE3 comparison on identical and differing files.
- Iteration-line formatting and exit code selection from a sequence of
  iteration results, with and without `--keep-going`.
- The end-to-end CI test above, both directions.
- A manual run of a few hundred iterations at the default size before the
  project is called done, with the result noted in the decisions file.
  This run is expected to report failures: see the known bug below. The
  project is done when the harness runs to the end with `--keep-going`,
  every failure it reports is explained, and the artefacts it leaves
  behind are the ones this design promises.

## Known irsync bug the harness will find

Found while checking this design by running its loop by hand: filegen's
`long` name class draws file names of 200 to 255 bytes, and irsync's
receiver stages each incoming file as `.irsync.<name>.part` in the same
directory, so a name longer than about 242 bytes fails with `File name too
long` and the client exits 23. At the default step count this happens
within the first few iterations of a run.

Decision: the bug is not fixed in this project. The harness reports it as
a failed iteration like any other, and the acceptance run documents it. A
run that wants to get past it uses `--filegen-set names.max_len=200`. The
fix (a partial-file name that fits whatever the entry name is) is a
follow-up with its own test.

## Decisions file

`docs/IRSYNC_HARNESS_DECISIONS.md`, in the style of the existing
`FILEGEN_DECISIONS.md` and `FILESET_DECISIONS.md`: the calls made while
building, the acceptance run's outcome, the known bug above and anything
else the first runs surface, each with the reason.

## Left to the builder

- Internal structure of the crate: how the subprocess wrappers, the daemon
  guard and the iteration loop are split into modules.
- The exact `run.toml` schema and the exact wording of log markers,
  iteration lines and error messages.
- How long to wait for the daemon's startup lines and for its exit after
  SIGTERM.
- Whether the sentinel file's name starts with a dot or not; it must not
  collide with anything filegen generates.
- Whether the client's `--timeout` (inactivity) is set by default. If the
  builder sets one, it goes in `run.toml` and the README.
- How the endpoint id is handled: as an opaque string from the daemon's
  stdout line, passed back to the client unchanged, is enough; the irsync
  library need not be linked to parse it.
- The `--seed` default: any source of 64 bits is fine as long as the seed
  is printed in the header and written to `run.toml`.
- Whether `--version` of the three tools is captured by running them or
  read from `CARGO_PKG_VERSION` of the workspace; running them is simpler
  and also proves the binaries are executable before the run starts.
