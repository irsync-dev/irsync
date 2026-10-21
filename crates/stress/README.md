# irsync-stress

`irsync-stress` runs the stress loop that filegen, fileset and irsync were
built for. It starts an irsync daemon on loopback, fills a source tree with
`filegen`, records it with `fileset init`, then repeats: sync with the
irsync client, compare the two manifests, run `fileset check` on the
destination, mutate the source with `filegen` and update its manifest. Any
iteration where the sync, the manifest comparison or the check fails is
reported and (unless `--keep-going`) ends the run. It touches no network
beyond loopback. Full design:
[docs/superpowers/specs/2026-10-21-stress-harness-design.md](../../docs/superpowers/specs/2026-10-21-stress-harness-design.md).
Decisions taken while building it, and the irsync bugs it has found:
[docs/IRSYNC_HARNESS_DECISIONS.md](../../docs/IRSYNC_HARNESS_DECISIONS.md).

## Command

```
irsync-stress --direction <push|pull> [OPTIONS] <ROOT> [-- <CLIENT ARGS>...]
```

`<ROOT>` is created if missing; it must be empty if it exists. It is never
deleted, on success or failure.

- `--direction <push|pull>`: which way the client moves data (required).
  On a push the source tree is `ROOT/SRC/data` and the daemon serves
  `ROOT/DST/data`; on a pull the roles are swapped.
- `--iterations <N>`: sync-and-verify rounds (default 10)
- `--seed <S>`: seed for filegen; random if omitted, and printed in the header
- `--steps <N>`: filegen operations per fill or mutation (default 100)
- `--max-total-size <SIZE>`, `--max-file-size <SIZE>`: handed to filegen
  verbatim; when not given, filegen's own defaults (25G and 256M) apply.
  filegen refuses a per-file limit above the total, so a small
  `--max-total-size` needs a `--max-file-size` as well.
- `--filegen-set <KEY=VALUE>`: an extra `--set` for every filegen run
  (repeatable), e.g. `--filegen-set names.max_len=200`
- `--log-level <LEVEL>`: log level for the daemon and the client (default
  `debug`)
- `--keep-going`: carry on after a failed iteration instead of stopping
- `--irsync <PATH>`, `--filegen <PATH>`, `--fileset <PATH>`: use this binary
  instead of looking one up
- `-- <ARGS>...`: everything after `--` is appended to the irsync client
  command line, e.g. `-- --parallel 4 --stats`. The harness sets no client
  inactivity timeout, so a hung sync hangs the run; `-- --timeout N` makes
  such a client fail instead, and the harness then exits 2.

The tools are looked up next to the `irsync-stress` binary first, then on
`PATH`. `run.toml` records the path and `--version` output of each.

Examples:

```
irsync-stress --direction push --iterations 20 /tmp/stress-push
irsync-stress --direction pull --seed 7 --keep-going /tmp/stress-pull -- --parallel 4
irsync-stress --direction push --max-total-size 8M --max-file-size 1M /tmp/small
```

## Output

A header, one line per iteration, a footer:

```
irsync-stress
  root:      /path/to/run
  seed:      3
  direction: push
  endpoint:  716a53cc4cea6705494da4d3b72cae40cfa658440e2e1fe4afba1f1bfbc2a5bb
  address:   127.0.0.1:50357
iter 0001/0003  sync 0.2s  exit 0    manifest ok  check ok
iter 0002/0003  sync 0.3s  exit 0    manifest ok  check ok
iter 0003/0003  sync 0.2s  exit 0    manifest ok  check ok
passed=3 failed=0 elapsed=1.1s
```

A failed iteration looks like this, followed by the path of its check
report:

```
iter 0002/0015  sync 1.3s  exit 23   manifest ok  check FAILED (added=0 deleted=1 changed=0)
```

## Layout

```
ROOT/
  run.toml                    parameters of the run, enough to repeat it
  SRC/
    config/                   client or daemon config, depending on direction
    data/                     the tree filegen mutates; its manifest is data/.fileset.sqlite
  DST/
    config/                   the other side's config
    data/                     where the tree arrives; checked here
  filegen/                    filegen's log and state file (outside both trees)
  logs/
    daemon.log                daemon output for the whole run, with iteration markers
    ping.client.log           client log of the sentinel round
    filegen.stderr.log        filegen's stderr, appended by every filegen run
    iter-0001.client.log      client log per iteration
    iter-0001.check.txt       fileset check output per iteration
    iter-0001.manifest.blake3 hashes of both manifests per iteration
```

## After a failure

Look at, in this order:

1. `ROOT/run.toml`: seed, direction, sizes, extra client arguments, tool
   versions, and the daemon's endpoint id, address and pid. It is enough to
   repeat the run.
2. The two manifests. `SRC/data/.fileset.sqlite` is what the destination
   should have matched; `DST/data/.fileset.sqlite` is what was checked.
   Inspect them with `fileset`, or compare `logs/iter-NNNN.manifest.blake3`.
3. `logs/iter-NNNN.check.txt`: what `fileset check` found (`ADD`, `DEL`,
   `CHG` lines and a summary).
4. `logs/iter-NNNN.client.log`: the client's output for that iteration, and
   in `logs/daemon.log` the line `=== iteration N ===` that marks where that
   iteration starts, so the daemon's side of the same sync can be read.

The daemon and client logs contain ANSI colour codes (irsync's tracing
output); use `less -R`.

## Exit codes

- `0`: every iteration passed
- `1`: at least one iteration failed (client exit 23, manifests differ, or
  `fileset check` found differences)
- `2`: the harness could not do its job: bad arguments, a root that is not
  empty, a missing tool, a daemon that did not start, a tool failing, or
  the client exiting with anything other than 0 or 23
- `130`: interrupted by SIGINT or SIGTERM; the client, filegen or fileset
  run in progress (if any) and the daemon are sent SIGINT first

A client exit of 23 (partial transfer) is a failed iteration, not a harness
error; the manifest comparison and the check still run for it.

## Bugs the harness has found

Two irsync bugs, both fixed on 2026-10-21: file names longer than about 242
bytes could not be transferred, because the staging name
`.irsync.<name>.part` exceeded `NAME_MAX`; and a FIFO whose only change was
its modification time was never updated. Until the fix, runs needed
`--filegen-set names.max_len=200` and had to discount `CHG ... mtime=` lines
on FIFOs; a default run now passes with neither. The decisions file has the
history, and [docs/STRESS_BUG_HUNTING.md](../../docs/STRESS_BUG_HUNTING.md)
the plan for finding the next ones.

## Campaign tools

`tools/breadth.sh` runs the breadth sweep described in
[docs/STRESS_BUG_HUNTING.md](../../docs/STRESS_BUG_HUNTING.md): many seeds,
short runs, both directions, several at a time, keeping only the roots of
failing runs. See the header of the script for its environment variables.

## Testing

`cargo test -p stress` runs unit tests and an end-to-end test that starts a
real daemon and the real tools. That test needs the other binaries, which
`cargo test -p stress` alone does not build: run `cargo build --workspace`
first (or use `cargo test --workspace`).
