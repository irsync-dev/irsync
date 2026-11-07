# irsync-netstress: implementation decisions

This file records the calls made while building `irsync-netstress` and the
fault layer behind it, from
`docs/superpowers/specs/2026-11-06-network-faults-design.md` and
`docs/superpowers/plans/2026-11-06-network-faults.md`, and what the first
real runs showed. The spec left these to the builder or did not settle them.
Where the code and the spec differ, the code is what shipped.

## Calls

1. **Profile numbers.** They are a proposal, not measurements, and live in
   `crates/irsync/src/net/netsim/profile.rs`.
   - `4g-vehicle`: down 2 to 30 Mbit/s, up 0.7 to 10 Mbit/s, the rate
     redrawn every 2 to 6 s; queue 512 KiB down and 256 KiB up; 30 ms one
     way, 20 ms jitter; bursty loss (good state 0.2%, bad state 20%, 0.4%
     chance of going bad and 10% of recovering, per packet); 0.5% reordering,
     0.1% duplication. Handovers every 30 to 90 s lasting 0.2 to 2 s. One
     dead zone of 20 to 60 s.
   - `dsl-video`: down 16 Mbit/s, up 1 Mbit/s, fixed; queue 1536 KiB down
     and 64 KiB up; 10 ms one way, 2 ms jitter; 0.01% random loss. Video
     cross traffic on the downlink only: a burst of 1 to 2 s every 2 to 4 s,
     arriving at twice the link rate. No outages.
   Why: they follow the spec's descriptions. Cost if wrong: the profiles
   mislead; they are one table, so retuning is cheap.

2. **`test-blackout` is a test-only profile.** No shaping, no loss, one
   6 s dead zone after 256 KiB of traffic, client `--timeout 2`, sizes 3
   iterations of 8M/1M. Why: the end-to-end tests need an outage that fits
   in seconds. The README marks it as not for campaigns.

3. **When the dead zone opens.** After 256 KiB to 4 MiB of traffic (drawn per
   client process), not after a fixed time. One per client process, so one per
   sync. Why: the spec wants the outage "at a random point while data is
   moving"; counting traffic puts it in the middle of the transfer however
   fast or slow the link is. Cost: a sync with under 256 KiB to move never
   gets one (noted, not failed; a whole run without one exits 3).

4. **`--reconnect 5m` and the per-profile client `--timeout`.** irsync-stress
   passes `--reconnect 0`; here the client gets `--reconnect 5m` so it can
   outlast a 60 s dead zone with room to spare. `--timeout` is the profile's
   (`10` for the two real profiles, `2` for `test-blackout`), placed before
   the passthrough arguments so a user's `-- --timeout N` wins. 10 s is
   longer than the 2 s worst handover and shorter than the 20 s shortest dead
   zone, so a dead zone always forces a reconnect and a handover never does.

5. **Stream numbering.** The fault layer's random streams are derived from
   the run seed and a stream number: the daemon is stream 0, the ping
   (sentinel) client is stream 0 as well, and iteration i's client is stream
   i. Each side and direction also mixes in its own tag. Why: the processes
   must not make correlated decisions, and an iteration's faults should not
   depend on how many earlier iterations ran.

6. **The environment variable.** `IRSYNC_NETSIM=profile=<name>,side=<client|daemon>,seed=<n>,stream=<n>`,
   all four keys required. A build without the feature never reads it.

7. **`CustomAddr` comes from `iroh_base`.** The fault layer is an iroh custom
   transport (id `0x6e65_7473_696d`, "netsim"); an address is the IPv4
   address's 4 bytes then the port's 2 bytes, big-endian.

8. **The `[netsim]` marker is on `--version` only.** Not on the startup lines
   or `run.toml` fields of irsync. The rig reads `irsync --version` and
   refuses to start unless it ends with ` [netsim]`; the `netsim active` line
   in each log then proves the profile was applied.

9. **The fault build command** is
   `cargo build --release --workspace --features netsim --target-dir target/netsim`.
   The plan's `--features irsync/netsim` builds irsync without the feature,
   because irsync lists itself as a dev-dependency. `--features netsim`
   with `--workspace` enables it on the package that has it. The separate
   target directory keeps `target/release` free of fault code.

10. **Exit 2 for a client exit other than 0 or 23**, as in irsync-stress. A
    client that exits 1 or 10, say, is the rig not working, not a failed
    iteration. A client exit of 23 is an iteration failure.

11. **The link model and its random draws.** The model does not make every
    random draw for every packet: a packet that finds the queue full is
    dropped and returns before the later draws (loss, reorder, duplicate,
    jitter). A seed fixes the draws but not the interleaving of real threads
    and clocks, so runs are not bit-for-bit repeatable.

## First real runs (2026-11-07)

One run per profile and direction, seed 1, started together on the same Mac
(so they shared CPU), build `011dcad`. Roots are kept under
`~/work/netstress-<profile>-<direction>`.

| run | wall time | result |
| --- | --- | --- |
| `4g-vehicle` pull | 919 s | passed 10 of 10, 10 outages |
| `4g-vehicle` push | 287 s | failed at iteration 3 (stopped, no `--keep-going`), 3 outages |
| `dsl-video` pull | 72 s | failed at iteration 1 |
| `dsl-video` push | 66 s | failed at iteration 1 |

Only one of the four runs passed. These are findings about irsync under the
fault layer, not about the rig; irsync was not changed.

### 4g-vehicle

Every iteration of both runs shows `outages 1`: the dead zone opened, the
client logged `no activity; dropping the connection secs=10`, then
`connection lost; reconnecting`, and `session started ... resumed=true` after
the dead zone closed. Dead zones in the logs looked at were 33 and 41 s long. Syncs took 36 to 165 s
in the pull (mean about 92 s), 79 to 112 s in the push. The full pull run
took 15 minutes, over the 10 minute target; it ran beside three other runs,
so the default sizes want checking on a quiet machine.

Warnings in the passing pull run, per iteration: the three `no activity` /
`session failed` / `connection lost; reconnecting` lines, `netsim active`,
the dead zone open and close lines, an occasional `SendDatagram` warning from
iroh, and on the daemon `connection failed: control stream: connection lost`
(once per outage) and `failed closing path err=LastOpenPath` from the QUIC
layer. All are expected from a dead zone.

**Push, iteration 3 (failed).** The client exit was 23. After the dead zone
and the resume, one file failed with `peer did not take the range in time`;
the file was not delivered and its `.irsync.*.part` was left in the
destination. `fileset check` reported 1 added (the `.part`) and 1 deleted
(the file). Log: `~/work/netstress-4g-vehicle-push/logs/iter-0003.client.log`.

### dsl-video

No dead zone is configured, so any reconnect is a failure.

**Pull, iteration 1 (failed).** Client exit 0, manifests equal, check clean,
but the rig failed the iteration: `reconnect without a dead zone (1)`. About
57 s into the sync the client logged `no activity; dropping the connection
secs=10`, then reconnected and resumed. So the 16 Mbit/s link with video
bursts stalled the connection for 10 s once. Log:
`~/work/netstress-dsl-video-pull/logs/iter-0001.client.log`.

**Push, iteration 1 (failed).** Client exit 23. About 60 s into the sync six
files failed with `peer did not take the range in time`, leaving three
`.part` files and six missing files. No reconnect, no dead zone. Log:
`~/work/netstress-dsl-video-push/logs/iter-0001.client.log`.

### What to look at next

`peer did not take the range in time` appears in two runs (4g push iteration 3,
dsl push iteration 1) and is the likely common cause of the exit 23s. It
means the sender gave up waiting for the receiver to accept a file range.
Whether the wait is too short for a slow uplink or the receiver is stuck is
for the irsync side to answer; the rig has shown only that it happens under
these profiles. The next step is to re-run the four with `--keep-going` and
other seeds (`tools/sweep.sh`) to see how often each failure occurs.

### The sweep smoke run

`SEEDS="1 1" PROFILES=dsl-video tools/sweep.sh` made two runs (push and
pull, seed 1) and a two-line `summary.txt`, and exited 1. Both runs failed,
so their roots were kept under the sweep's `OUT`. With `--keep-going` (which
the sweep always passes) `dsl-video` failed 10 of 10 iterations on push, all
with client exit 23 and `outages 0`, and 9 of 10 on pull (iteration 1
passed). Later failures include `.part` files left behind by earlier ones,
so only the first failing iteration of each run says anything about the
cause. This agrees with the single-seed runs above: `dsl-video` does not
yet pass on seed 1 in either direction.
