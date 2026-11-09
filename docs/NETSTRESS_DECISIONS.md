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
     cross traffic on the downlink only: a burst of 1 to 2 s every 2 to 4 s
     that fills the queue to 1280 KiB and then takes 10 of the 16 Mbit/s
     (see call 12). No outages.
   Why: they follow the spec's descriptions. Cost if wrong: the profiles
   mislead; they are one table, so retuning is cheap.

2. **`test-blackout` is a test-only profile.** No shaping, no loss, one
   6 s dead zone after 256 KiB of traffic, client `--timeout 2`, sizes 3
   iterations of 8M/1M. Why: the end-to-end tests need an outage that fits
   in seconds. The README marks it as not for campaigns.
   `test-slow` (added with the fix below) is irsync's own: a clean
   256 kbit/s uplink for a regression test of a push that takes over a
   minute. The rig does not offer it. Neither does it offer `test-wan`
   (added 2026-11-09): 100 Mbit/s each way and 80 ms round trip, clean,
   for measuring `--parallel` (`IRSYNC_DECISIONS.md`, 2.1 item 7).

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

5. **Stream numbering and seed mixing.** The fault layer's random streams
   are derived from the run seed and a stream number: the daemon is stream
   0, the ping (sentinel) client is stream 0 as well, and iteration i's
   client is stream i. Each generator's seed is the run seed, the side, the
   stream number and the purpose (link or outages) folded together through
   splitmix64, one part after another (`fault_seed` in
   `crates/irsync/src/net/netsim/mod.rs`). Why: the processes must not make
   correlated decisions, and an iteration's faults should not depend on how
   many earlier iterations ran. The first version XORed the parts with fixed
   tags, which made streams collide: client iteration 3's uplink drew the
   same numbers as the daemon's downlink, and seeds 1 to 10 times iterations
   1 to 10 gave only 16 distinct client fault schedules. A test now checks
   that 10 seeds by 10 streams give 100 distinct seeds for each purpose.

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

12. **`dsl-video`'s cross traffic leaves room for irsync.** The first
    version had the video arrive at twice the link rate into the shared
    queue. Once the queue was full every irsync packet was dropped, which
    meant 100% loss for up to about 1.2 s in every 2 to 4 s cycle, not the
    spec's "clumps of a few percent while full". Now, during a burst, the
    video fills the queue at twice the link rate up to a standing 1280 KiB
    (about 650 ms at 16 Mbit/s) and then holds it there, taking 10 of the
    16 Mbit/s. The remaining 6 Mbit/s and the last 256 KiB of the queue are
    irsync's: its packets queue behind the video (delay 650 to 790 ms) and
    are dropped only when irsync itself sends faster than its share. Why: a
    real video stream is a responsive flow that backs off rather than
    overfilling the buffer forever, and the spec asks for a few percent of
    loss while full. A test sends 8 Mbit/s for 60 s and checks 0.5% to 5%
    overall loss with fewer than 100 drops in a row (seeds 0 to 29
    gave 0.6% to 3%). Cost if wrong: the profile is gentler than a
    real overloaded line; `hold_bytes` and `share` in `profile.rs` tune it.

13. **Datagram counts.** Each process's transport counts the datagrams iroh
    handed it to send (`sent`), those dropped because the queue was full
    (`queue_drops`), by random loss (`loss_drops`), by an outage in either
    direction (`outage_drops`, client only), the sends the socket refused
    (`send_errors`) and received datagrams larger than iroh's buffer
    (`truncated`, detected by reading into a buffer one byte larger, and
    dropped). It logs them at `warn` as `netsim: counts sent=…
    queue_drops=… loss_drops=… outage_drops=… send_errors=… truncated=…`
    every 10 s while traffic flows and once when the transport is dropped
    (best effort: a process that exits without dropping its endpoint skips
    the last line). The first send error is logged at `warn`, later ones at
    `debug`. Why: before, losses outside the model were invisible, and
    there was no way to compare a failing sync's losses with what the
    profile should give.

14. **`4g-vehicle`'s queues are kept, and give seconds of delay at the
    lowest rates.** 256 KiB up at 0.7 Mbit/s is about 3 s of queueing
    delay, 512 KiB down at 2 Mbit/s about 2 s. Kept as a known property:
    cellular links are known for multi-second bufferbloat, and a queue small
    enough for 500 ms at the lowest rate (about 43 KiB up) would hold only
    35 ms at 10 Mbit/s and drop most bursts at the higher rates. Both are
    under the client's 10 s `--timeout`. Cost if wrong: syncs under
    `4g-vehicle` see longer delays than a real link might give; shrink
    `queue_bytes` in `profile.rs` if that turns out to matter.

## First real runs (2026-11-07)

One run per profile and direction, seed 1, started together on the same Mac
(so they shared CPU), build `011dcad`. The roots were under
`~/work/netstress-<profile>-<direction>` and have since been deleted; the
2026-11-07 sweep below replaces these findings.

| run | wall time | result |
| --- | --- | --- |
| `4g-vehicle` pull | 919 s | passed 10 of 10, 10 outages |
| `4g-vehicle` push | 287 s | failed at iteration 3 (stopped, no `--keep-going`), 3 outages |
| `dsl-video` pull | 72 s | failed at iteration 1 |
| `dsl-video` push | 66 s | failed at iteration 1 |

Only one of the four runs passed. Where the fault lies is unresolved. The
causes may be in irsync, in the fault layer (a bug in the link model or the
transport), or in the profile (for example `dsl-video`'s 1 Mbit/s uplink
with a 64 KiB queue may simply be too harsh). The sections below record what
was observed; every cause named is a hypothesis. irsync was not changed.

What would tell them apart, none of it done yet:
- the same profile with relaxed numbers (faster uplink, bigger queue, no
  loss): if the failures go away, the profile was too harsh;
- the fault layer active but with no shaping (the `test-blackout` link
  without its dead zone): if the failures stay, the transport is suspect;
- packet counts from the fault layer (sent, dropped by queue, dropped by
  loss, delivered) for a failing sync, to see whether the losses match what
  the profile should give. The fault layer now logs these (call 13).

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
(the file).

### dsl-video

No dead zone is configured, so any reconnect is a failure.

**Pull, iteration 1 (failed).** Client exit 0, manifests equal, check clean,
but the rig failed the iteration: `reconnect without a dead zone (1)`. About
57 s into the sync the client logged `no activity; dropping the connection
secs=10`, then reconnected and resumed. Hypothesis, not shown: the link
(16 Mbit/s with video bursts) stalled the connection for 10 s once; a stall
in the fault layer or in irsync would look the same. A further hypothesis,
found after the run: the cross-traffic model then in use (call 12, first
version) dropped every irsync packet for up to about 1.2 s in every 2 to 4
s, and QUIC's retransmit backoff over repeated blackouts of that length may
well add up to 10 s without activity.

**Push, iteration 1 (failed).** Client exit 23. About 60 s into the sync six
files failed with `peer did not take the range in time`, leaving three
`.part` files and six missing files. No reconnect, no dead zone.

### What to look at next

Observed: `peer did not take the range in time` appears in two of the
single runs (4g push iteration 3, dsl push iteration 1) and in the smoke
runs. Hypothesis: it is the common cause of the exit 23s; it says the
sender gave up waiting for the receiver to accept a file range. Whether the
wait is too short for a slow uplink, the receiver is stuck, or the fault
layer is holding packets back is not known; the tests listed above would
separate these. The next step is to re-run the four with `--keep-going` and
other seeds (`tools/sweep.sh`) to see how often each failure occurs.

Answered since: the sweep below found the cause, a fixed 60 s limit in the
sender (`STOPPED_TIMEOUT` in `send.rs`) that a slow uplink could not meet.
It was removed in `2d7f708`, and the sweep after the timeout fix passed
400 of 400 iterations.

### dsl-video pull after the final-review fixes (2026-11-07)

One run, alone on the Mac, after the seed mixing (call 5), the new
cross-traffic model (call 12) and the counts (call 13):
`irsync-netstress --profile dsl-video --direction pull --seed 1` (root
since deleted). It passed 10 of 10 in 166.5 s
(syncs 2.4 to 26.5 s), exit 0, no reconnect, no `no activity` line, no send
error. The last counts lines were:

```
daemon.log            sent=62601 queue_drops=0 loss_drops=12 outage_drops=0 send_errors=0 truncated=0
iter-0001.client.log  sent=4814 queue_drops=0 loss_drops=0 outage_drops=0 send_errors=0 truncated=0
iter-0002.client.log  sent=5558 queue_drops=0 loss_drops=3 outage_drops=0 send_errors=0 truncated=0
```

The other clients show the same pattern (399 to 4744 sent, nothing
dropped). The daemon's line is its last 10 s report; the rig stops it
without dropping the transport, so there is no final line. Observed: no
queue drops at all on the downlink, so irsync's congestion control stayed
inside its 6 Mbit/s share and the 256 KiB of headroom, and only the 0.01%
random loss bit. The earlier stray reconnect did not recur. One run on
one seed does not show which change mattered (the seeds changed too, so
this was a different fault schedule), and under the new model this
direction may now be too gentle to find much; a sweep would say more.

### The sweep smoke run

`SEEDS="1 1" PROFILES=dsl-video tools/sweep.sh` made two runs (push and
pull, seed 1) and a two-line `summary.txt`, and exited 1. Both runs failed,
so their roots were kept under the sweep's `OUT`. With `--keep-going` (which
the sweep always passes) `dsl-video` failed 10 of 10 iterations on push, all
with client exit 23 and `outages 0`, and 9 of 10 on pull (iteration 1
passed). Later failures include `.part` files left behind by earlier ones,
so only the first failing iteration of each run says anything about the
cause.

The same seed failed differently in the two runs. In the single run the dsl
pull failed iteration 1 on a stray reconnect (client exit 0); in the smoke
run it passed iteration 1 and failed iteration 2 with exit 23 and `range in
time`, which the single pull run never showed. The faults run on the real
clock, so a seed fixes the draws but does not replay the run (see call 11).
What the two have in common is only that `dsl-video` did not pass cleanly on
seed 1 in either direction.

## Sweep after the listing-backpressure merge (2026-11-07)

`tools/sweep.sh` with its defaults: seeds 1 to 10, both profiles, both
directions, 40 runs, four at a time, build `42d9b9c`. About 2.5 hours.
The failing runs' roots were deleted at the next sweep; their outputs and
summary are kept under `~/work/irsync-netstress-sweep/prev`.

| | runs that passed | iterations that passed |
| --- | --- | --- |
| `4g-vehicle` pull | 10 of 10 | 100 of 100 (99 dead zones, all resumed) |
| `dsl-video` pull | 9 of 10 | 92 of 100 |
| `4g-vehicle` push | 0 of 10 | 25 of 100 |
| `dsl-video` push | 0 of 10 | 1 of 100 |

No run exited 2 or 3. No crash of the client or the daemon, no reconnect
outside a dead zone, and no dead zone that failed to resume. The rig and
the reconnect path work.

**One cause behind every failing run.** In all 21 failing runs the first
failed iteration exits 23 with `peer did not take the range in time` on 1
to 11 files (twice with a `hard link target failed` that follows from it).
Later iterations of a run fail mostly from the `.part` files the first
failure left behind.

The timing is exact. Under `dsl-video` push every failing sync takes 61
to 62 s, and the files fail 60.0 s after `session started`. Under
`4g-vehicle` push the files fail 60 s after the session resumes from its
dead zone. That is `STOPPED_TIMEOUT` in `crates/irsync/src/send.rs`: after
writing a range and calling `finish()`, the sender waits at most 60 s for
the receiver to confirm it.

Observed in `dsl-video-push-1` iteration 1: the client's counts show no
queue drops and no loss; the daemon's receiver had 0 to 2 of each failed
range's chunks after 60 s (`arrived=0 promised=12`, `arrived=1
promised=4`); the session delivered 1.8 MB in about 60 s, about a quarter
of what the 1 Mbit/s uplink can carry. The pull direction, whose data goes
down the 16 Mbit/s link, almost always passes.

Confirmed after the sweep: the 60 s starts when the range is in QUIC's
local send buffer, not when it is on the wire, and `stopped()` resolves
when the peer's transport has acknowledged every byte (noq's own
documentation), not when the receiver has processed it. Up to 32 ranges
(`DEFAULT_PARALLEL`) share the link in turn (noq's send fairness), so each
one's clock also counts the time the others take. Experiment: the same
6 MB of 256K files under `dsl-video` push, seed 101, failed with 34 timed
out ranges at `--parallel 32` (78 s) and passed at `--parallel 1` (89 s,
longer, but no range waited 60 s). After a dead zone the resumed session
queues its whole backlog at once, which explains the 4G case.

The fix removes the limit: the sender waits for `stopped()` as long as it
takes. The limit added nothing: a peer that acknowledges anything (keep-
alives included) acknowledges this data too, and a dead peer ends the
connection (QUIC's 60 s idle timeout, the client's `--timeout`), which ends
the wait with an error. Regression test:
`a_slow_uplink_that_needs_more_than_a_minute_still_delivers_everything` in
`crates/irsync/tests/netsim.rs` pushes 3 MiB over the new `test-slow`
profile (256 kbit/s); before the fix all 12 files failed at 60 s, after
it the push takes about 100 s and succeeds.

Why the uplink ran
at only about a quarter of its rate with no drops was a separate question,
answered 2026-11-08: BBR on a return path that delays acknowledgements.
`dsl-video`'s downlink cross traffic holds up to 1280 KiB in the queue for
1 to 2 s every 2 to 4 s, so a push's ACKs come back up to about 0.65 s
late. BBR keeps no more in flight than about twice bandwidth times minimum
RTT (a few KB here) plus an ACK-aggregation allowance capped at its window,
so the sender idles through each burst; the fault layer drops nothing.
3 MiB pushes (kbit/s, 1 Mbit/s uplink): `dsl-video` 560; without the cross
traffic 964; without loss and jitter but with it 576; with Cubic 871
(Cubic fills the uplink queue: about 220 queue drops a push). BBR against
Cubic, two runs each, kbit/s:

| profile, direction | BBR | Cubic |
| --- | --- | --- |
| `dsl-video` push, 3 MiB | 498, 546 | 850, 874 |
| `dsl-video` pull, 24 MiB | 4422, 4709 | 7596, 8650 |
| `4g-vehicle` push, 8 MiB | 1273, 1240 | 1001, 1041 |
| `4g-vehicle` pull, 8 MiB | 1701, 1794 | 1088, 1283 |

Neither wins everywhere: Cubic is 1.6 to 1.8 times faster on the stable
line with cross traffic, BBR 23 to 50% faster on the lossy 4G link, and
Cubic's full queue costs other traffic on the line about 0.5 s of latency.
noq's BBR exposes no gain to tune. So BBR stays the default and
`congestion = "cubic"` is a setting (`[client]`, a host entry, `[daemon]`;
the sending side's applies). With it, the same `dsl-video` push runs at
911 kbit/s against 566 with the default.

**A second problem: an orphaned `.part` blocks deleting its directory.**
In `dsl-video-pull-2`, iteration 3 failed as above and left three `.part`
files. Two were picked up and finished by later syncs. The third belonged
to a file the source later deleted; it stayed (the client's garbage
collection only removes parts older than 7 days), so `fileset check`
flagged it in iterations 4 to 9. In iteration 10 the source removed the
whole directory, and irsync failed to delete it on the destination with
`Directory not empty` (exit 23). This does not need a bad network: any
interrupted sync followed by deleting the file's directory at the source
should show it.

Fixed 2026-11-07: before removing a directory, the delete pass removes
irsync's own temporaries (`.irsync.<name>.part` / `.tmp`) from it when they
are all it holds and no running writer holds a `.part` (each is locked
while written); otherwise they stay and the directory is reported as not
empty, as before. Their progress records go at the next GC. Regression
test: `delete_removes_a_directory_holding_only_leftover_parts` in
`crates/irsync/tests/delete.rs`, which failed with `Directory not empty`
before the fix. A leftover `.part` in a directory that stays is still kept
until GC removes it after 7 days.

Also seen: about 600 debug lines per sync of iroh's `failed to initiate NAT
traversal: Not enough addresses`, because the fault layer gives each side
only one address. Harmless noise.


## Sweep after the timeout fix (2026-11-08)

The same sweep, defaults again, on build `bc95ad2`: the timeout fix
(`2d7f708`) and everything before it, but not the leftover-`.part` delete fix
(`88a06c9`) or the `congestion` setting (`e9ce098`), so BBR throughout.
About 3 h 15 min. Outputs and `summary.txt` are under
`~/work/irsync-netstress-sweep`; every run passed, so no roots were kept.

| | runs that passed | iterations that passed | the sweep before | mean run |
| --- | --- | --- | --- | --- |
| `4g-vehicle` pull | 10 of 10 | 100 of 100 (99 dead zones, all resumed) | 100 of 100 | 832 s |
| `dsl-video` pull | 10 of 10 | 100 of 100 | 92 of 100 | 213 s |
| `4g-vehicle` push | 10 of 10 | 100 of 100 (99 dead zones, all resumed) | 25 of 100 | 1393 s |
| `dsl-video` push | 10 of 10 | 100 of 100 | 1 of 100 | 1798 s |

400 of 400 iterations passed, with no exit 2 or 3 and no crash. The push
direction, which failed in every run before, now passes: the longest single
sync took 505 s (`dsl-video` push seed 6, iteration 1), far past the 60 s
the old limit allowed. The 8 failed `dsl-video` pull iterations of the
sweep before did not come back; they were not looked into further.

What is left is speed, not correctness. A `dsl-video` push run takes about
8 times as long as a pull run (the longest, seed 6, 2234 s), because the
push goes up the 1 Mbit/s uplink and BBR uses only about half of it there
(see above). A full default sweep now takes over 3 hours at `JOBS=4`. A
sweep on a build with `e9ce098` and `congestion = "cubic"` would show
whether Cubic shortens the `dsl-video` push runs without hurting `4g-vehicle`.


## Sweep with Cubic (2026-11-08)

The same sweep with `--congestion cubic`, a new rig option that writes
`congestion = "cubic"` into both the daemon's and the client's config, so
both directions use Cubic. Build: `8ed0392` plus that option, so it also
has the leftover-`.part` delete fix (`88a06c9`). Same seeds, so the same
networks and the same dead zones as the BBR sweep above. About 3 h 12 min.
Outputs and `summary.txt` are under `~/work/irsync-netstress-sweep-cubic`.

40 of 40 runs and 400 of 400 iterations passed, with 99 dead zones in each
`4g-vehicle` direction, all resumed. Mean run time (10 iterations), against
the BBR sweep:

| | BBR | Cubic | Cubic against BBR |
| --- | --- | --- | --- |
| `dsl-video` pull | 213 s (158 to 253) | 107 s (89 to 128) | 2 times as fast |
| `dsl-video` push | 1798 s (1446 to 2234) | 1009 s (829 to 1256) | 44% less time |
| `4g-vehicle` pull | 832 s (765 to 929) | 1556 s (1294 to 1790) | 87% more time |
| `4g-vehicle` push | 1393 s (1169 to 1605) | 1768 s (1466 to 2079) | 27% more time |

The ranges do not overlap in any row. This matches the two-run comparison
under "Sweep after the listing-backpressure merge", at larger scale: Cubic wins clearly on the
stable `dsl-video` line, BBR on the lossy `4g-vehicle` link. On `4g-vehicle`
pull the gap is bigger than the 23 to 50% seen there. The longest single
sync moved too: with BBR it was a `dsl-video` push (505 s), with Cubic a
`4g-vehicle` push (488 s, seed 7, iteration 10). The wall time hardly
changed, because the slow runs just moved from one profile to the other.

So BBR stays the default: a slow sync on a bad mobile link hurts more than
a slow one on a good line, and Cubic's full queue adds latency for other
traffic on the line. Users on a stable wired line who sync a lot can set
`congestion = "cubic"`.


## ACK settings on a long-delay link (2026-11-08)

The open point in `docs/IRSYNC_DECISIONS.md` §2.1 item 1: irsync asks the
peer for immediate ACKs (every packet, within 1 ms), measured so far only
on loopback. Two rig additions made it measurable:

- A `long-rtt` profile: 45 ms each way (90 ms round trip), 50 Mbit/s down
  and 10 Mbit/s up, no jitter, loss or outages.
- A `--acks <immediate|gentle|peer-default>` option, which the fault layer
  takes as `acks=` in `IRSYNC_NETSIM` and applies on both sides.
  `immediate` is irsync's setting; `gentle` is every second packet within
  1 ms; `peer-default` asks for nothing, so the peer ACKs every second
  packet or after 25 ms.

Build: `8cefd29` plus these two additions, BBR. Many small files (up to
16K), 3 syncs per run, `--log-level info`, `--stats`. `long-rtt`: 60000
steps, trees up to 256M, seeds 1 to 5. `dsl-video`: 20000 steps, trees up
to 128M, seeds 1 to 3. The three settings of one seed and direction ran at
the same time, so they shared the machine's load. All 144 syncs passed.
Results and logs are under `~/work/irsync-netstress-acks`.

Total sync time over all seeds, and the packets the client sent on pulls
(nearly all of them ACKs):

| | immediate | gentle | peer-default |
| --- | --- | --- | --- |
| `long-rtt` pull | 544 s | 507 s | 684 s |
| `long-rtt` push | 1211 s | 1216 s | 1215 s |
| `dsl-video` pull | 545 s | 559 s | 648 s |
| `dsl-video` push | 3610 s | 3756 s | 3776 s |
| client packets, `long-rtt` pull | 378,765 | 360,478 | 360,114 |
| client packets, `dsl-video` pull | 100,169 | 90,546 | 73,875 |

What it shows:

- **`long-rtt`: no difference.** In 12 of the 15 pulls all three settings
  took the same time to a tenth of a second. The totals come from three
  syncs where one setting stalled, a different one each time:
  `peer-default` once (193 s against 45 s), `immediate` once and `gentle`
  once (77 s against 44 s). The stalls were later traced to noq's BBR (see
  "The stalls on a long-delay pull"). Pushes are held by the 10 Mbit/s
  uplink and are within 1% of each other. Immediate ACKs sent 5% more
  packets.
- **`dsl-video` pull: `peer-default` is slower.** It lost in 2 of 3 seeds
  (343 s against 280 s, 153 s against 111 s). `immediate` and `gentle` were
  about equal, although immediate ACKs put 11% more packets than `gentle`
  and 36% more than `peer-default` on the 1 Mbit/s uplink.
- **`dsl-video` push: only immediate ACKs never dropped the connection.**
  `immediate` was fastest in 2 of 3 seeds. With `gentle` (3 syncs) and
  `peer-default` (5 syncs) the client logged `session failed error=no
  activity for 10 s` at the very end of the sync and reconnected. The
  daemon had already finished the session and answered `BadSession`, so
  the client started a new one, which only listed again and moved no
  files. The cause was a large final message, not the ACKs themselves; see
  "The 10 s drops at the end of a push" below. The sync times include the
  extra listing. The byte counts of those syncs are not usable: `--stats`
  covers only the last session.

The client's return traffic on pulls was 7.5 to 8% of the data bytes for
all three settings, but that also holds the client's listing and requests,
so it is not a clean measure of ACK traffic.

**A limit of the rig:** the fault layer releases packets on tokio timers
of about 1 ms. At these speeds several packets arrive together and get one
ACK between them, whatever the setting. So the rig shows less extra ACK
traffic than a real link that delivers packets one at a time would.

So irsync keeps immediate ACKs: they cost no time on a long-delay link and
helped on the slow uplink, and their extra packets did not slow anything
down here.

### Found on the way

- **The 10 s drops at the end of a push.** First written up here as a
  resume the daemon refused; see the next section for what it really was.
- **Stalls on `long-rtt` pull.** Some syncs took 2 to 4 times their
  usual time, with every setting. A bug in noq's BBR, not an ACK effect;
  see "The stalls on a long-delay pull" below.


## The 10 s drops at the end of a push (2026-11-09)

A debug-level rerun of the failing cases (`dsl-video` push, seeds 1 and 2,
`--acks gentle` and `peer-default`, under
`~/work/irsync-netstress-acks-debug`) reproduced 6 drops. All looked the
same:

1. The daemon placed every file, ran its delete pass and sent
   `SessionDone`.
2. `SessionDone` lists up to 1000 deleted paths. filegen's deep trees make
   those paths about 340 bytes each, so from the second sync on the message
   was 340 to 500 KB. In the first sync it listed one path.
3. That message took 3 to 11 s to reach the client, over a link that
   should carry it in well under a second. Our reading: during a push the
   daemon sends almost nothing, so its congestion window starts small, and
   during video bursts each round trip takes about 0.8 s. Not measured
   directly.
4. The client's idle watchdog counted a control message only once it was
   complete, so the client saw 10 s of silence and dropped the connection,
   sometimes just before the message would have arrived.
5. The daemon had finished and forgotten the session, so `BadSession` was
   the right answer. The new session found nothing left to do.

Time from the daemon's last delete to the end of the session, syncs with
the large message: immediate ACKs 4 to 8 s, `gentle` 3 to 11 s,
`peer-default` 5 to 11 s; with the small message 0.1 to 1 s for all. So
immediate ACKs did not prevent the problem, they only kept it under 10 s.

Fixed in three parts:

- The control reader counts every read that brings bytes as activity, so a
  large message still arriving is progress
  (`session::control::Counted`).
- The deleted and would-delete lists in `SessionDone` stop at 1000 paths or
  64 KiB, whichever comes first (`MAX_DONE_PATH_BYTES`).
- `SessionDone` already said how many paths it left out; the pushing
  client now adds them to its count, so `--stats` and the
  `irsync_deleted_total` metric are right past the cap
  (`Outcome::deleted_count`). Before, a push with more than 1000 deletions
  reported 1000.

Checked with the same four runs on the fixed build (under
`~/work/irsync-netstress-fixcheck`): 12 of 12 syncs passed with no
reconnect, against 6 drops before. The end of a sync with many deletions
now takes 2 to 7 s instead of 3 to 11 s. It is still slower than the size
of the message explains, so the slow start after a quiet push remains;
the client no longer mistakes it for a dead peer.


## The stalls on a long-delay pull (2026-11-09)

The stalls noted under "ACK settings on a long-delay link" come from noq's
BBR, irsync's default congestion controller. After a quiet spell of about
15 s with only small packets (heartbeats, control messages), BBR takes the
trickle's rate for the link's bandwidth and its window drops from about
1 MB to under 10 KB. It stays there for minutes, with no loss and no
congestion signal on the link.

**Reproduced** with `long-rtt`, pull, seed 2, the three `--acks` settings
run at once (as in the ACK measurement), iterations of 60000 steps, 256 MiB,
files up to 16 KiB. With BBR about one round in three had a sync of 394 s
to over 20 min (stopped there), against about 50 s normally; the case
stalled 4 times in 12 rounds, each time with `peer-default` (the ACK measurement had seen
`immediate` and `gentle` stall too). Stack samples showed
both processes idle, so it is neither disk nor CPU.

**Traced** with debug lines on both sides, logged every 2 s. The sending
side's stays as the debug-level `sends` line (`send::report_sends`):
congestion window, round trip, congestion events, losses, and how many
sends wait for a stream, wait for acknowledgement or move data. The
client's line was dropped. They showed that the daemon's sends had all handed their data to QUIC and
were waiting for acknowledgement; the client had about 30 streams open,
reading as data trickled in; no errors, retries or losses. The daemon's
path statistics then showed:

```
13:54:29 cwnd=1007213 rtt_ms=92 cong=0 lost=0
(nothing but heartbeats for 15 s)
13:54:45 cwnd=8713    rtt_ms=95 cong=0 lost=0
13:55:03 cwnd=23184   rtt_ms=94 cong=0 lost=0
...
14:01:56 cwnd=24686   rtt_ms=95 cong=0 lost=0
```

About 20 to 30 KB a round trip is the 50 packets a second the fault layer
counted during every stall.

**Cubic does not stall.** The same case with `--congestion cubic`, 8 rounds
of 3 runs of 3 iterations: 72 of 72 syncs passed, the slowest 65.7 s, no
stall. Iteration 3 took 57 to 66 s, against about 50 s with BBR when BBR
does not stall. Outputs under `~/work/irsync-netstress-cubic`.

**Cause, in noq-proto 1.3.0** (1.4.0 has the same code). Two parts, found
with a test that drives noq's `Bbr3` through bulk sending, 15 s of one
packet every 500 ms, then bulk again:

1. `Connection::poll_transmit` marks a path app-limited only when it sent
   nothing (`last_packet_number.is_none() && !send_blocked`). A heartbeat
   is a packet sent, so the quiet spell is not app-limited, and its
   delivery rate counts as a bandwidth sample. Once BBR's max filter (two
   probe cycles) has dropped the earlier samples, `max_bw` falls to about
   13 KB/s, one packet per round trip.
2. Even when told the sender is app-limited, `Bbr3::on_end_acks` clears its
   app-limited marker and does not set it again in the same call (an
   `if ... else if`), so every other heartbeat counts as a sample. With
   both cleared and re-armed, the app-limited case keeps its estimate and
   noq's 28 BBR tests still pass.

In the simulation BBR recovers within about 6 s once bulk sending resumes;
in irsync it stayed low for minutes. Why is not known; a guess is that
irsync's traffic after the pause, many small streams each waiting for its
acknowledgements, also looks partly app-limited.

**What follows.** Cubic as the default would trade this for the losses
measured under "Sweep with Cubic" (`4g-vehicle` pulls 87% slower), so BBR
stays the default for now. The fix belongs in noq; a report with the test
and the `on_end_acks` change is drafted. Part 1 needs noq to tell "ran out
of data" from "stopped at the datagram batch limit" in `poll_transmit`.

**Also seen:** the slower syncs that were not BBR stalls (70 to 77 s against
about 50 s) were the client's delete pass. Three runs deleting about 6900
deep paths each at the same time on one disk took 18 s, with the network
idle.
