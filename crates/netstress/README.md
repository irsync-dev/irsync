# irsync-netstress

`irsync-netstress` runs the same sync-and-verify loop as `irsync-stress`,
but across a simulated bad network. The irsync client and daemon send and
receive every QUIC packet through a fault layer built into irsync (the cargo
feature `netsim`). The layer imposes a named profile: limited speed, a
queue that overflows, delay, bursty loss, and dead zones that cut the link
for 20 to 60 seconds. An iteration passes when the sync, the manifests and
`fileset check` pass, as in irsync-stress, and the network behaved as the
profile says it should. Design:
[docs/superpowers/specs/2026-11-06-network-faults-design.md](../../docs/superpowers/specs/2026-11-06-network-faults-design.md).
Calls made while building it and the first real runs:
[docs/NETSTRESS_DECISIONS.md](../../docs/NETSTRESS_DECISIONS.md).

## The fault build

The fault layer is not in a normal build. Build the fault binaries into
their own target directory:

```
cargo build --release --workspace --features netsim --target-dir target/netsim
```

They land in `target/netsim/release/`. Their `irsync --version` ends with
` [netsim]`. `irsync-netstress` checks that and refuses to start with an
irsync that lacks it. Write the command exactly as shown:
`--features irsync/netsim` builds irsync without the feature, because irsync
lists itself as a dev-dependency.

## Command

```
irsync-netstress --profile <NAME> --direction <push|pull> [OPTIONS] <ROOT> [-- <CLIENT ARGS>...]
```

The options, the root layout, `run.toml`, the logs and the tool lookup are
those of [irsync-stress](../stress/README.md). Differences:

- `--profile <NAME>`: the network to run under (required), see below.
- `--iterations`, `--steps`, `--max-total-size`, `--max-file-size` default
  to the profile's values.
- The client runs with `--reconnect 5m` (irsync-stress uses `--reconnect 0`)
  and with the profile's `--timeout`, before any `-- <CLIENT ARGS>`.
- The header has a `network:` line.

Example:

```
target/netsim/release/irsync-netstress --profile 4g-vehicle --direction push --seed 1 ~/work/netstress-4g
```

## Profiles

| profile | what it is | defaults (iterations, total, file) | client `--timeout` |
| --- | --- | --- | --- |
| `4g-vehicle` | A moving vehicle on 4G: rate between 2 and 30 Mbit/s down (0.7 and 10 up), about 60 ms round trip with jitter, bursty loss, handovers every 30 to 90 s, and one dead zone per sync | 10, 256M, 32M | 10 s |
| `dsl-video` | A DSL line overloaded by video: 16 Mbit/s down, 1 Mbit/s up, video bursts on the downlink, a large queue, no outages | 10, 32M, 4M | 10 s |
| `test-blackout` | For tests only: no shaping, one 6 s dead zone | 3, 8M, 1M | 2 s |

The numbers are a first proposal, not measurements. The exact values are in
`crates/irsync/src/net/netsim/profile.rs`.

A dead zone opens after a profile-set amount of traffic (256 KiB to 4 MiB
for `4g-vehicle`), once per client process, so every sync gets one. It cuts
both directions. The client logs when it opens and closes.

Each process also logs, at `warn`, what became of its datagrams: `netsim:
counts sent=… queue_drops=… loss_drops=… outage_drops=… send_errors=…
truncated=…`, every 10 s while traffic flows and when the transport closes.

## What counts as a pass

On top of irsync-stress's checks:

- The client and daemon logs say the fault layer is active with the right
  profile (`netsim active`).
- An iteration whose dead zone opened must show the session interrupted and
  resumed (the same session id, `resumed=true`).
- A reconnect with no dead zone open fails the iteration.
- The daemon is still running after each iteration.
- No `.irsync.*.part` file is left in either tree after the run.

A sync that ends before its dead zone is due is noted, not failed.

## Output

As irsync-stress's, with `network:` in the header and the number of outages
that forced a reconnect on each iteration line:

```
iter 0001/0010  sync 94.7s  exit 0    manifest ok  check ok  outages 1
...
passed=10 failed=0 elapsed=812.3s outages=10
```

`outages` counts dead zones that forced the session to be interrupted and
resumed.

## Exit codes

- `0`: every iteration passed
- `1`: at least one iteration failed (including a network check)
- `2`: the rig could not do its job: bad arguments, a root that is not
  empty, a missing or non-fault tool, a daemon that died, or a client exit
  other than 0 or 23
- `3`: the profile has dead zones but none bit in the whole run, so nothing
  was tested; not a pass
- `130`: interrupted

## Campaign tool

`tools/sweep.sh` runs every seed, profile and direction, a few at a time,
and keeps only the roots of failing runs. Defaults: seeds 1 to 10, four
runs at a time, both real profiles, the binaries in `target/netsim/release`.
It refuses to start without a fault build, writes `summary.txt` and exits
non-zero if any run did not exit 0. See the header of the script for its
environment variables (`BIN`, `OUT`, `SEEDS`, `JOBS`, `PROFILES`). A full
default sweep is 40 runs of up to 10 minutes each; the first `4g-vehicle`
pull run took 919 s with four runs sharing the CPU, so expect longer.

## Testing

`cargo test -p netstress` runs unit tests and end-to-end tests under
`test-blackout`. The end-to-end tests build the fault binaries themselves
into `target/netsim/debug` the first time they run, which takes a while.
