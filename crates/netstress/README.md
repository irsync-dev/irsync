# irsync-netstress

`irsync-netstress` tests irsync over a bad network. It runs the same
sync-and-check loop as [irsync-stress](../stress/README.md), but every
packet between the irsync client and the daemon passes through a simulated
link: limited speed, a queue that overflows, delay, bursts of loss, and on
some profiles a dead zone that cuts the link for 20 to 60 seconds.

The question it answers: when the network is bad, does irsync still deliver
exactly the right files, reconnect and resume when the link drops, and keep
both the client and the daemon running? Slow syncs and warnings in the log
are expected. Wrong files, failed syncs, crashes or leftover partial files
are not.

It is a separate rig. `irsync-stress` keeps testing over a perfect localhost
network and is unchanged.

This file is the user guide. For how the simulated network works inside,
see [docs/NETSTRESS_HOW_IT_WORKS.md](../../docs/NETSTRESS_HOW_IT_WORKS.md).
The design is in
[docs/superpowers/specs/2026-11-06-network-faults-design.md](../../docs/superpowers/specs/2026-11-06-network-faults-design.md).
The decisions made while building it, and the results of the first real
runs, are in [docs/NETSTRESS_DECISIONS.md](../../docs/NETSTRESS_DECISIONS.md).

- [Quick start](#quick-start)
- [The fault build](#the-fault-build)
- [Command line](#command-line)
- [Profiles](#profiles)
- [What a run does](#what-a-run-does)
- [What counts as a pass](#what-counts-as-a-pass)
- [Output](#output)
- [Exit codes](#exit-codes)
- [Files a run leaves behind](#files-a-run-leaves-behind)
- [Reading the logs](#reading-the-logs)
- [After a failure](#after-a-failure)
- [Repeating a run](#repeating-a-run)
- [Sweeps](#sweeps)
- [Using the fault layer by hand](#using-the-fault-layer-by-hand)
- [Changing or adding a profile](#changing-or-adding-a-profile)
- [Testing the rig](#testing-the-rig)
- [Known limits](#known-limits)

## Quick start

```
# 1. Build the fault binaries (once, and after every change to irsync).
cargo build --release --workspace --features netsim --target-dir target/netsim

# 2. A short smoke run, about a minute: no shaping, one 6 s dead zone per sync.
target/netsim/release/irsync-netstress --profile test-blackout --direction push ~/work/ns-smoke

# 3. A real run: a moving vehicle on 4G, pushing. Up to about 10 to 15 minutes.
target/netsim/release/irsync-netstress --profile 4g-vehicle --direction push --seed 1 ~/work/ns-4g-push
```

The root directory (the last argument) must not exist yet, or must be
empty. It is never deleted. Use a short path such as `~/work/...`: filegen
makes deep trees and refuses to run when the root's path is long enough to
push files past the system's path length limit.

## The fault build

The fault layer is compiled into irsync only when the cargo feature
`netsim` is on. A normal build has none of it, and never reads the
`IRSYNC_NETSIM` variable. Build the fault binaries into their own target
directory:

```
cargo build --release --workspace --features netsim --target-dir target/netsim
```

- The binaries land in `target/netsim/release/`: `irsync`,
  `irsync-netstress`, `irsync-stress`, `filegen` and `fileset`.
- The separate target directory keeps `target/release` free of fault
  code, so a fault build can never be mistaken for a real one.
- Write the command exactly as shown. `--features irsync/netsim` looks
  the same but builds irsync *without* the feature, because irsync lists
  itself as a dev-dependency.
- Check it: `target/netsim/release/irsync --version` ends with
  ` [netsim]`. `irsync-netstress` checks this itself and refuses to start
  with any other irsync, printing the build command.
- A debug build works too (drop `--release`, binaries in
  `target/netsim/debug/`), but the link model then costs more CPU and runs
  take longer.

Rebuild after every change to irsync. The rig uses whatever binary it
finds; an old fault build tests old code.

## Command line

```
irsync-netstress --profile <NAME> --direction <push|pull> [OPTIONS] <ROOT> [-- <CLIENT ARGS>...]
```

| Option | Meaning | Default |
| --- | --- | --- |
| `<ROOT>` | Working directory for the run. Created if missing; must be empty if it exists. Never deleted. | required |
| `--profile <NAME>` | The network to simulate: `4g-vehicle`, `dsl-video` or `test-blackout`. See [Profiles](#profiles). | required |
| `--direction <push\|pull>` | Which way the client moves data. On a push the client sends; on a pull the daemon sends. | required |
| `--iterations <N>` | Number of sync-and-check rounds. | the profile's |
| `--seed <S>` | Seed for the file trees and for the network's random choices. Printed in the header and saved in `run.toml`. | random |
| `--steps <N>` | filegen operations per fill and per mutation. | the profile's |
| `--max-total-size <SIZE>` | Passed to filegen: the most data the tree may hold. | the profile's |
| `--max-file-size <SIZE>` | Passed to filegen: the largest single file. | the profile's |
| `--filegen-set <KEY=VALUE>` | An extra filegen `--set` for every filegen run; repeatable. | none |
| `--log-level <LEVEL>` | Log level for the daemon and the client. | `debug` |
| `--keep-going` | Carry on after a failed iteration instead of stopping. | off |
| `--irsync`, `--filegen`, `--fileset <PATH>` | Use this binary instead of looking one up. The tools are looked up next to `irsync-netstress` first, then on `PATH`. | lookup |
| `-- <ARGS>...` | Everything after `--` is added to every irsync client command. | none |

What the rig adds to the client command line by itself:

- `--reconnect 5m`: the client may spend up to five minutes reconnecting
  after the link drops. That is far longer than the longest dead zone (60
  s). irsync-stress passes `--reconnect 0` instead.
- `--timeout <N>`, the profile's value: after N seconds without hearing
  from the daemon, the client drops the connection and reconnects. It comes
  before your `-- <ARGS>`, so `-- --timeout 20` overrides it.

Examples:

```
# Five quick rounds of DSL in the pull direction, keep going past failures
irsync-netstress --profile dsl-video --direction pull --iterations 5 --keep-going ~/work/ns-dsl

# 4G with a smaller tree, and the client told to sync four files at a time
irsync-netstress --profile 4g-vehicle --direction push --max-total-size 64M ~/work/ns-4g -- --parallel 4

# Info-level logs (debug is the default, and is large on long runs)
irsync-netstress --profile 4g-vehicle --direction pull --log-level info ~/work/ns-4g-info
```

## Profiles

Each profile describes one kind of bad network. "Up" is client to daemon,
"down" is daemon to client. On a **push** the file data goes up; on a
**pull** it comes down. A slow uplink therefore hurts pushes most.

### `4g-vehicle`: a moving vehicle on 4G

| | down (daemon to client) | up (client to daemon) |
| --- | --- | --- |
| speed | 2 to 30 Mbit/s | 0.7 to 10 Mbit/s |
| speed changes | a new speed every 2 to 6 s | same |
| queue | 512 KiB (about 2 s at the slowest speed) | 256 KiB (about 3 s at the slowest speed) |
| delay | 30 ms each way, drifting by up to ±20 ms | same |
| loss | about 1%, in bursts: normally 0.2%, but now and then the link goes bad and loses 20% for a while | same |
| other | 0.5% of packets arrive out of order, 0.1% arrive twice | same |

Outages, in both directions at once:

- **Handovers** every 30 to 90 s, each cutting the link for 0.2 to 2 s,
  like moving from one cell tower to the next. Too short to force a
  reconnect.
- **One dead zone per sync**, 20 to 60 s long, like a tunnel. It opens
  once the sync has moved 256 KiB to 4 MiB of traffic, so it always hits
  in the middle of a transfer. It is long enough that the client must give
  up on the connection, reconnect and resume.

Run defaults: 10 iterations, 100 filegen steps, trees up to 256M, files up
to 32M, client `--timeout 10`.

What it is meant to catch: resume bugs, files left half-written, state that
does not survive a reconnect, the client or daemon getting stuck or giving
up.

### `dsl-video`: a DSL line overloaded by video streaming

| | down (daemon to client) | up (client to daemon) |
| --- | --- | --- |
| speed | 16 Mbit/s, fixed | 1 Mbit/s, fixed |
| queue | 1536 KiB | 64 KiB (about 0.5 s) |
| delay | 10 ms each way, drifting by up to ±2 ms | same |
| loss | 0.01% at random | same |
| video traffic | every 2 to 4 s a burst of 1 to 2 s: the video fills the queue to 1280 KiB and then takes 10 of the 16 Mbit/s. irsync's packets wait behind it (650 to 790 ms extra delay) and are lost only if irsync sends faster than the 6 Mbit/s left over. | none |

No handovers and no dead zones. The link never drops, so **any reconnect
fails the iteration**: under this profile the client should never decide
the connection is dead.

Run defaults: 10 iterations, 100 filegen steps, trees up to 32M, files up
to 4M, client `--timeout 10`.

What it is meant to catch: timeouts that are too short for a slow or
overloaded line, stalls when packets wait a long time in a queue, and
irsync giving up on a peer that is merely slow.

### `test-blackout`: for the rig's own tests

No speed limit, no delay, no loss. One 6 s dead zone per sync, after 256
KiB of traffic. Client `--timeout 2`. Defaults: 3 iterations, 50 steps,
trees up to 8M, files up to 1M.

It exists so the automated tests can check the reconnect path in seconds.
It is also a quick way to check that a build and a machine work before
starting a long real run. It does not simulate any real network; don't use
it for campaigns.

### Where the numbers come from

The numbers are a first proposal based on what these networks are known to
do, not measurements. The exact values are in two places:
`crates/irsync/src/net/netsim/profile.rs` (the link and the outages) and
`crates/netstress/src/profile.rs` (run sizes and client timeout). See
[Changing or adding a profile](#changing-or-adding-a-profile).

## What a run does

The sequence is irsync-stress's, with the fault layer switched on:

1. **Checks the tools.** Finds `irsync`, `filegen` and `fileset`, and
   refuses to go on unless `irsync --version` ends with ` [netsim]`. Only
   then does it create the root.
2. **Writes `run.toml`** with every setting, so the run can be repeated.
3. **Starts the daemon** with `IRSYNC_NETSIM` set for the daemon side. It
   waits up to 15 s for the daemon to log `netsim active`, which proves the
   fault layer really is on.
4. **Ping round.** One sync of a single small file, which must arrive byte
   for byte. This proves the two sides can talk through the fault layer
   before any real work. A failure here is a setup failure (exit 2).
5. **Fills the source tree** with filegen and records its manifest with
   `fileset init`.
6. **For each iteration:**
   1. Checks the daemon is still running and writes `=== iteration N ===`
      into the daemon log.
   2. Runs the client with `IRSYNC_NETSIM` set for the client side. Each
      iteration gets its own fault schedule (its own speeds, losses and
      dead zone timing).
   3. Checks the daemon survived the sync.
   4. Reads the client log: was the fault layer active, did each dead zone
      end in a resumed session, was there any reconnect outside a dead
      zone?
   5. Compares the two manifests and runs `fileset check` on the
      destination, as irsync-stress does.
   6. Prints the iteration line. Then mutates the source tree with filegen
      for the next round.
7. **Stops the daemon**, looks for `.irsync.*.part` files left anywhere in
   either tree, prints the footer and exits.

On a push the client's tree is the source and the daemon's export is the
destination. On a pull they swap.

## What counts as a pass

An iteration passes when everything irsync-stress checks passes:

- the client exits 0,
- the source and destination manifests match,
- `fileset check` finds no differences,

and the network side looks right too:

- the client log says `netsim active` (the fault layer was on),
- every dead zone that opened was followed by the **same** session
  resuming (`session started ... resumed=true` with the first session's id),
- there was no reconnect outside a dead zone,
- the daemon is still running afterwards.

Across the whole run:

- no `.irsync.*.part` file is left in either tree,
- if the profile has dead zones, at least one sync must actually have hit
  one. Otherwise nothing was tested (exit 3).

A single sync that ends before its dead zone is due (because it moved less
than the threshold) is noted with `outages 0`, not failed. Later
iterations of a run often move little data, so this is normal for them.

## Output

A header, one line per iteration, a footer:

```
irsync-netstress
  root:      /Users/mike/work/ns-4g-push
  seed:      1
  direction: push
  endpoint:  716a53cc4cea6705494da4d3b72cae40cfa658440e2e1fe4afba1f1bfbc2a5bb
  address:   127.0.0.1:50357
  network:   4g-vehicle
iter 0001/0010  sync 94.7s  exit 0    manifest ok  check ok  outages 1
iter 0002/0010  sync 61.2s  exit 0    manifest ok  check ok  outages 1
...
passed=10 failed=0 elapsed=812.3s outages=10
```

- `sync`: how long the client took. Under `4g-vehicle` this includes the
  dead zone, so 60 to 160 s is normal.
- `exit`: the client's exit code. 0 is success, 23 means some files did
  not transfer.
- `outages`: how many dead zones opened during this sync (0 or 1).
- The footer's `outages=` is the total for the run.

A failed iteration gets an extra line under it when the network checks
failed, naming the reason and the client log:

```
iter 0001/0010  sync 72.3s  exit 0    manifest ok  check ok  outages 0
  reconnect without a dead zone (1 time(s)); see /Users/mike/work/ns-dsl/logs/iter-0001.client.log
```

When the file checks failed, the path of the check report follows instead,
as in irsync-stress:

```
iter 0003/0010  sync 104.2s  exit 23   manifest MISMATCH  check FAILED (added=1 deleted=1 changed=0)  outages 1
/Users/mike/work/ns-4g-push/logs/iter-0003.check.txt
```

At the end, any partial files left behind are listed before the footer:

```
partial file left behind: /Users/mike/work/ns-4g-push/DST/data/a/b/.irsync.report.pdf.part
```

and a run where no dead zone ever opened ends with:

```
faults did not bite: no iteration had an outage
```

The possible reasons on the extra line:

| Reason | Meaning |
| --- | --- |
| `fault layer not active` | The client log has no `netsim active` line. |
| `reconnect without a dead zone (N time(s))` | The client decided the connection was dead and reconnected while the link was up (no dead zone open). The link was slow or lossy, but not down. |
| `dead zone did not end in a resumed session (D dead zone(s), R resumed)` | A dead zone opened, but afterwards the client did not resume the session it had before: it started a fresh one, never got back, or the sync ended. |

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Every iteration passed. |
| `1` | At least one iteration failed (file checks or network checks), or a partial file was left behind. |
| `2` | The rig could not do its job: bad arguments, a root that is not empty, a missing tool, an irsync without the fault layer, the daemon did not start or died, the ping round failed, a tool failed, or the client exited with something other than 0 or 23. |
| `3` | The profile has dead zones but none opened in the whole run, so the reconnect path was never tested. This is not a pass. Usually the trees were too small; raise `--max-total-size`. |
| `130` | Interrupted (Ctrl-C or SIGTERM). The daemon and any running client are stopped first. |

## Files a run leaves behind

The layout is irsync-stress's:

```
ROOT/
  run.toml                    every setting of the run, enough to repeat it
  SRC/config/, SRC/data/      the source side (the client on a push)
  DST/config/, DST/data/      the destination side (the daemon on a push)
  filegen/                    filegen's log and state
  logs/
    daemon.log                the daemon's log for the whole run, with === iteration N === markers
    ping.client.log           the client log of the ping round
    filegen.stderr.log        filegen's error output
    iter-0001.client.log      the client log of each iteration
    iter-0001.check.txt       what fileset check found
    iter-0001.manifest.blake3 hashes of both manifests
```

`run.toml` has one extra section:

```toml
[network]
profile = "4g-vehicle"
client_timeout_secs = 10
reconnect = "5m"
```

The logs are small (a few MB for a 10-iteration 4G run at `debug`); the
data trees are what take space, up to the profile's total size on each
side. Delete roots you no longer need. The sweep script deletes the roots
of passing runs for you.

## Reading the logs

The logs contain colour codes; read them with `less -R`, or strip the codes
before searching with `grep`.

The fault layer writes these lines, all at `warn` so they show at any log
level:

| Line | Where | Meaning |
| --- | --- | --- |
| `netsim active profile=… side=… seed=… stream=…` | client and daemon, at start | The fault layer is on, with these settings. |
| `netsim: dead zone open secs=N` | client | The dead zone started; it lasts N seconds. |
| `netsim: dead zone closed` | client | The dead zone ended. It is logged with the first packet after the end, so it may come a little later than N seconds. |
| `netsim: counts sent=… queue_drops=… loss_drops=… outage_drops=… send_errors=… truncated=…` | client and daemon | What happened to this process's packets so far. Every 10 s while traffic flows, and once at the end. |
| `netsim: send failed: …` | client and daemon | The real socket refused a packet. The first one is at `warn`, later ones at `debug`. Should never happen. |

The counts, per process, counting only packets this process sent (except
`outage_drops` and `truncated`):

| Count | Meaning |
| --- | --- |
| `sent` | Packets irsync handed to the fault layer to send. |
| `queue_drops` | Dropped because the simulated queue was full: irsync sent faster than the link could carry. |
| `loss_drops` | Dropped by the random loss. |
| `outage_drops` | Dropped because a handover or the dead zone was on, sent or received. Client only; the daemon's is always 0. |
| `send_errors` | The real socket refused to send. Should be 0. |
| `truncated` | A received packet was bigger than irsync's buffer and was dropped. Should be 0. |

The counts tell you whether a failing sync saw the losses the profile
should give. For example, `queue_drops` far above `loss_drops` on a push
under `dsl-video` means irsync kept overfilling the 64 KiB uplink queue.
The daemon's last counts line is usually its last 10-second report: the
rig stops the daemon without letting it write a final one.

What a normal dead zone looks like in the client log (4G, `--timeout 10`),
in order:

1. `netsim: dead zone open secs=41`
2. about 10 s later: `no activity; dropping the connection secs=10`, then
   `session failed`, then `connection lost; reconnecting`
3. further reconnect attempts while the zone lasts
4. `netsim: dead zone closed`
5. `session started session=<same id> resumed=true`

In the daemon log around the same time, expect
`connection failed: control stream: connection lost` and, from the QUIC
layer, `failed closing path err=LastOpenPath`. An occasional `SendDatagram`
warning from iroh is also normal. None of these fail a run.

Useful searches (on a log with colour codes stripped):

```
# strip colour codes
sed 's/\x1b\[[0-9;]*m//g' logs/iter-0003.client.log > /tmp/iter3.log

# the network story of one sync
grep -E 'netsim|no activity|reconnecting|session started|session failed' /tmp/iter3.log

# every file that failed, and why
grep -E 'failed|did not take the range' /tmp/iter3.log

# the daemon's side of iteration 3
sed -n '/=== iteration 3 ===/,/=== iteration 4 ===/p' logs/daemon.log
```

## After a failure

Start with the first failed iteration. With `--keep-going`, later
iterations inherit the damage (for example a `.part` file left by an
earlier one), so only the first failure says anything clear about its
cause.

1. Read the iteration line and the reason line under it.
2. Read `run.toml` for the seed, profile, direction and sizes.
3. Read the client log of that iteration (see [Reading the
   logs](#reading-the-logs)): the network story, then which files failed
   and with what message.
4. Read the daemon log between `=== iteration N ===` and the next marker.
5. For file differences, read `logs/iter-NNNN.check.txt` (`ADD`, `DEL`,
   `CHG` lines), as in irsync-stress.

What each kind of failure usually points at:

| Symptom | Where to look |
| --- | --- |
| Client `exit 23` with `peer did not take the range in time` | The sender waited 60 s for the receiver to confirm a piece of a file and gave up (`STOPPED_TIMEOUT` in `crates/irsync/src/send.rs`). Seen under both real profiles. Not yet known whether the wait is too short for a slow link, the receiver is stuck, or the fault layer holds packets back. Compare with the counts lines. |
| `reconnect without a dead zone` | The client saw 10 s without any packet from the daemon while the link was up. Look at the counts around that time: many drops point at the profile or the link model, few drops point at irsync. |
| `dead zone did not end in a resumed session` | The reconnect worked but a new session started instead of the old one resuming, or the client never got back. Look at what the client logged after `dead zone closed`. |
| `partial file left behind` | A failed or interrupted file left its `.irsync.<name>.part`. It usually goes with a client exit 23 earlier in the run. |
| Exit 2 with `daemon died during iteration N` | The daemon crashed. The end of `daemon.log` has the reason. This is always worth a bug report. |
| Exit 2 with `irsync client exited N` | The client failed in a way that is not a partial transfer: 1 (the whole sync failed, for example the reconnect window ran out), 25 (`--max-delete` stopped it), or a crash. The client log has the reason. |
| Exit 3 | No sync moved enough data to hit the dead zone. Use a bigger `--max-total-size`. |

To tell an irsync problem from a problem with the simulation, change one
thing and run the same seed again. If a failure goes away with gentler
numbers in the profile, the profile was too harsh. If it stays even under
`test-blackout`, which has no shaping at all, look at irsync or the
transport. The first-run findings and the open questions are in
[docs/NETSTRESS_DECISIONS.md](../../docs/NETSTRESS_DECISIONS.md).

## Repeating a run

The seed fixes the file trees exactly and fixes the random choices of the
network (which speed, which packet is lost, how long the dead zone lasts).
But the network runs on the real clock, and threads do not run in the same
order every time, so a repeated run does not replay packet for packet. The
same seed gives a run with the same character, not the same run. To repeat
one:

```
irsync-netstress --profile <profile> --direction <dir> --seed <seed> ~/work/new-root
```

using the values from the failed run's `run.toml`. Some hints:

- Run it alone. Several runs at once share the CPU, which changes timing
  and makes syncs slower.
- If the failure was in iteration N, keep `--iterations N` and let it stop
  at the first failure (no `--keep-going`).
- Try a few seeds. A failure that shows up on one seed in ten is still a
  real failure.
- Extra client options go after `--`, for example
  `-- --timeout 30` to test whether a longer timeout makes a failure go
  away.

## Sweeps

`tools/sweep.sh` runs many seeds, both profiles and both directions, a few
runs at a time, always with `--keep-going`. It deletes the roots of passing
runs (they can be repeated from their seed) and keeps the roots of failing
ones.

```
crates/netstress/tools/sweep.sh                     # seeds 1 to 10, both profiles, both directions
SEEDS="1 3" PROFILES=dsl-video crates/netstress/tools/sweep.sh
JOBS=2 crates/netstress/tools/sweep.sh --iterations 3
```

| Variable | Meaning | Default |
| --- | --- | --- |
| `BIN` | Directory with `irsync-netstress`, `irsync`, `filegen`, `fileset`. | `target/netsim/release` |
| `OUT` | Where roots, outputs and `summary.txt` go. | `~/work/irsync-netstress-<date>` |
| `SEEDS` | Seeds to run, as arguments to `seq`: `"1 10"` is 1 to 10, `"5 5"` is just 5. | `1 10` |
| `JOBS` | Runs at the same time. | `4` |
| `PROFILES` | Profiles to run, separated by spaces. | `4g-vehicle dsl-video` |

Any arguments go to every `irsync-netstress` run, before the root.

It refuses to start without a fault build. For each run it writes
`OUT/<profile>-<direction>-<seed>.out` (the rig's output) and adds one line
to `OUT/summary.txt`:

```
4g-vehicle push 3 exit=1 passed=9 failed=1 elapsed=1104.2s outages=10
```

It exits non-zero if any run did not exit 0.

Plan for the time. A full default sweep is 40 runs of up to about 10
minutes each, more when four share the CPU: the first `4g-vehicle` pull
took 919 s that way. Expect two to three hours at `JOBS=4`. Lower `JOBS` if
syncs slow down enough to cause timeouts of their own.

## Using the fault layer by hand

The rig is not the only way to use the fault layer. Any irsync from the
fault build turns it on when the `IRSYNC_NETSIM` variable is set:

```
IRSYNC_NETSIM=profile=<name>,side=<client|daemon>,seed=<n>,stream=<n>
```

All four keys are required, in any order. A bad value is an error, not
ignored.

- Set it on **both** sides with the same profile and seed, `side=daemon`
  for the daemon and `side=client` for the client. With the variable set,
  irsync talks only through the fault layer: no ordinary UDP socket, no
  relay. A client with the variable cannot reach a daemon without it, and
  the other way round.
- `stream` picks one of many independent fault schedules for the same
  seed. The rig uses 0 for the daemon and the ping, and the iteration
  number for each iteration's client. Use different streams to get
  different schedules.
- The fault layer only works over IPv4 on this machine. Point the client at
  the address the daemon prints with `--address 127.0.0.1:<port>`.
- Give the client a `--reconnect` window longer than the dead zones (the
  rig uses `5m`) and the profile's `--timeout`. Without a reconnect window
  a dead zone simply fails the sync.
- Look for `netsim active` in each side's log to be sure it is on.

## Changing or adding a profile

The profiles live in two places:

- `crates/irsync/src/net/netsim/profile.rs`: the link in each direction
  (`link()`) and the handovers and dead zone (`outages()`). The fields are
  documented in the file and explained in
  [docs/NETSTRESS_HOW_IT_WORKS.md](../../docs/NETSTRESS_HOW_IT_WORKS.md).
- `crates/netstress/src/profile.rs`: the run defaults (iterations, steps,
  sizes), the client `--timeout`, and whether the profile has dead zones.

To change numbers, edit those tables and rebuild the fault build. The unit
tests in `crates/irsync/src/net/netsim/link.rs` and `outage.rs` check some
profile behaviour (for example that 4G loses about 1% in bursts, and that
`dsl-video`'s video leaves room for irsync); a big change may need their
bounds updated. Record why in
[docs/NETSTRESS_DECISIONS.md](../../docs/NETSTRESS_DECISIONS.md).

To add a profile:

1. Add a variant to `ProfileName` in `crates/irsync/src/net/netsim/mod.rs`,
   and its name to `FromStr` and `Display` there.
2. Add its link and outage parameters in
   `crates/irsync/src/net/netsim/profile.rs`.
3. Add the same name to `Profile` in `crates/netstress/src/profile.rs`,
   with its defaults.
4. Add a unit test in `link.rs` or `outage.rs` that checks the behaviour
   the profile is meant to have.
5. Document it in the [Profiles](#profiles) section above.

Rules that keep a profile useful:

- The client `--timeout` must be **longer** than the longest handover and
  **shorter** than the shortest dead zone. Then a dead zone always forces a
  reconnect and a handover never does. For `4g-vehicle` that is 2 s < 10 s
  < 20 s.
- A profile without dead zones fails on any reconnect, so its worst stall
  (queueing delay plus loss bursts) must stay well under the client
  `--timeout`.
- Video-style cross traffic must hold the queue **below** its size
  (`hold_bytes < queue_bytes`), or irsync's packets have no room at all and
  the link blacks out. Nothing checks this yet.
- The dead zone threshold (`dead_zone_after_bytes`) must be well under what
  a typical sync moves, or most syncs never hit it.

## Testing the rig

```
cargo test -p netstress                       # the rig: unit and end-to-end tests
cargo test -p irsync --features netsim        # irsync, including the fault layer's own tests
```

The end-to-end tests run the real rig under `test-blackout` (and one short
`dsl-video` pull). The first time, they build the fault binaries into
`target/netsim/debug` themselves, which takes a few minutes.

The fault layer's own tests:

- `crates/irsync/src/net/netsim/*.rs`: the link model, outages, seeds and
  addresses.
- `crates/irsync/tests/netsim.rs`: a real client and daemon sync a 4 MiB
  file through a dead zone and resume; without the variable nothing
  changes.
- `crates/irsync/tests/netsim_endpoint.rs`: an endpoint with the fault
  layer on offers no ordinary IP or relay address, so no packet can go
  around it.

## Known limits

- **Not a packet-for-packet replay.** See [Repeating a run](#repeating-a-run).
- **CPU-sensitive.** The simulation runs in the irsync processes on the
  real clock. A busy machine delays packets on top of the simulated delay.
  Real runs are best done alone, or a few at a time.
- **Loopback only, IPv4 only.** Both sides run on this machine.
- **Only the client has outages.** The client's fault layer drops both
  what it sends and what it receives during a handover or dead zone; the
  daemon's never cuts the link itself. Both directions are still cut.
- **One dead zone per client process.** A sync is one client process, so
  one per sync. A sync that moves less than the threshold gets none.
- **First-proposal numbers.** The profiles have not been checked against
  measured networks.
- **Client kill and daemon crash are not covered.** Killing the client
  mid-sync and crashing the daemon are a planned second project.
