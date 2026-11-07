# Network-fault test rig — design

Date: 2026-11-06
Status: implemented 2026-11-07 (plan: ../plans/2026-11-06-network-faults.md)

## Intent

Every irsync-stress run so far crossed a perfect network: loopback, no
loss, no delay, no outage. The bugs the harness has found were all in
planning and placing files; the code that copes with a bad network
(congestion control, the idle watchdog, reconnecting, resuming a session,
adopting `.part` files) has never run in such a loop at all.

This project builds a new, separate test rig that runs the same kind of
sync-and-verify loop across a simulated bad network, to prove irsync gets
a tree across intact when the network misbehaves. It is an addition: the
existing irsync-stress harness, its tests and its sweeps over perfect
loopback stay as they are and keep their purpose (Mike's direction).

Mike's priorities, highest first: a lossy network, a network that drops
out, the client being killed, the daemon crashing. This project covers the
first two. Killing the client and crashing the daemon are a second
project: they need a rig that kills and restarts processes, not a faulty
network.

What "intact" means here, in Mike's words: a network drop is long enough
to force irsync to reconnect and resume the session; neither the client
nor the daemon may crash; the experience is seamless, except for warnings
and the extra time spent.

## Goals

- A new command (working name `irsync-netstress`, Claude's call) runs the
  loop through a named network profile. The client and the daemon send and
  receive every QUIC packet between them through a fault layer that
  imposes that profile's behaviour.
- Two profiles to start with, both chosen by Mike:
  1. `4g-vehicle`: an intermittent 4G connection in a moving vehicle.
  2. `dsl-video`: a DSL line overloaded by video streaming.
- More profiles can be added later as presets of the same link model,
  without changing the fault layer.
- An iteration passes when the client exits 0, the manifests are equal and
  the check is clean, as in irsync-stress, and in addition the daemon is
  still running and the network checks below hold (see Verification).
- A run fits in about 10 minutes (Mike's limit), with each profile's
  default tree size chosen to make that true.
- The faults are driven by the run's seed, as filegen's operations are.

## Non-goals

- Changing irsync-stress. Its command line, output, exit codes, tests,
  `tools/breadth.sh`, `tools/matrix.sh` and their results stay as they
  are. The new rig may reuse code from the `stress` crate's library;
  moving code there to share it is fine as long as irsync-stress behaves
  identically and its tests pass unchanged.
- Killing the client, crashing the daemon (the second project).
- Bit-for-bit reproducible network behaviour. The faults run on the real
  clock against real processes and threads, so which packet falls into an
  outage depends on timing. The seed fixes the fault schedule and the
  random draws, not the interleaving. A failure is reproduced by re-running
  the seed, possibly several times, not by replay.
- Simulating NAT, relays, address changes, path migration or anything
  above the packet level (DNS, discovery). The fault layer only decides
  what happens to packets on the one path between client and daemon.
- turmoil or any whole-process simulation. Considered and rejected: it
  needs both peers in one process on a simulated clock and tokio-shaped
  sockets, while irsync talks through iroh and its QUIC stack `noq` with
  their own sockets, and does real, blocking disk I/O that a simulated
  clock does not cover.
- An external proxy, or the operating system's traffic shaping (`pf` and
  dummynet, `tc netem`). Mike ruled out a proxy; the OS tools need root.

## Anti-goals

- The fault layer in a normal irsync build. It exists only when a cargo
  feature is enabled; a build without it contains none of its code, and no
  configuration or environment variable can turn faults on in such a
  build.
- Fault-feature binaries replacing the normal ones. They are built into
  their own target directory, so `target/release/irsync`, which
  irsync-stress and everyday use rely on, is never a fault build.
- A run that passes because the faults never bit (see Verification).
- Any traffic between the two peers that bypasses the fault layer (a
  second, direct IP path iroh found on its own). Every packet goes through
  it or the run means nothing.

## Constraints

- iroh 1.3's custom transport hook, `Builder::add_custom_transport`
  (traits `CustomTransport`, `CustomEndpoint`, `CustomSender`, behind
  iroh's feature `unstable-custom-transports`; example in iroh's
  `examples/custom-transport.rs`). Mike's choice. iroh marks it unstable:
  it may change in any iroh release without a major version bump, so the
  fault layer is kept small and in one place, and an iroh upgrade that
  breaks it shows up as a build failure of the fault feature only.
- The rig runs the real `irsync` binaries as separate processes (a daemon
  and one client per iteration), on loopback, as irsync-stress does.
- The faults happen in real time. A 1 Mbit/s uplink really is 1 Mbit/s.
- macOS and Linux, as the rest of irsync.

## Behaviour

### The link model

Each profile is a preset of one link model, applied separately to each
direction of the link. The client is the home or vehicle end; the daemon
is the far end. "Up" is client to daemon, "down" is daemon to client.

Per direction:

- **Rate**: the bottleneck's speed, possibly varying over time.
- **Queue**: a finite buffer in front of the bottleneck, in bytes. A
  packet that finds it full is dropped (tail drop). A full queue is also
  what makes delay grow (bufferbloat).
- **Cross traffic**: competing traffic that takes a share of the rate on
  its own schedule, filling the queue.
- **Delay and jitter**: a base one-way delay plus random variation.
- **Random loss**: loss independent of the queue, in bursts rather than
  evenly spread (a two-state good/bad model is the usual way).
- **Reordering and duplication**: occasional.
- **Outages**: time windows in which every packet in both directions is
  dropped.

### The two profiles

The numbers are Claude's proposal, not measurements; Mike may tune them.
Each is a starting point the decisions file records once runs show how
irsync behaves.

**`4g-vehicle`, intermittent 4G in a moving vehicle**

- Round trip about 60 ms, ±40 ms jitter.
- Rate varying between about 2 and 30 Mbit/s, changing every few seconds,
  in both directions (uplink about a third of the downlink).
- About 1% random loss, in bursts.
- Handovers every 30 to 90 s: the link goes silent for 0.2 to 2 s, and
  packets in flight around it may arrive reordered.
- Dead zones (tunnels, rural gaps): the link is dead for 20 to 60 s. These
  are the outages that must force a reconnect. Mike's choice: every sync
  gets one, at a random point while data is moving, rather than on a
  realistic every-few-minutes timetable that would often fall between
  syncs and test nothing. Handovers keep their realistic timetable.

**`dsl-video`, a DSL line overloaded with video streaming**

- 16 Mbit/s down, 1 Mbit/s up; a push is far slower than a pull.
- Video cross traffic on the downlink: a chunk every 2 to 4 s that fills
  the link while it lasts.
- A large queue: while full, delay climbs to 300 to 800 ms.
- Loss mostly from queue overflow: clumps of a few percent while full,
  almost none otherwise.
- No outages.

### The command

`irsync-netstress --profile <NAME> --direction <push|pull> [OPTIONS] <ROOT>
[-- <CLIENT ARGS>...]` (the name and exact options are Claude's working
proposal). It mirrors irsync-stress where the two overlap (`--seed`,
`--iterations`, `--steps`, the size limits, `--filegen-set`,
`--keep-going`, the tool paths, the root layout, `run.toml`, logs, exit
codes 0, 1, 2 and 130), so a developer who knows one knows the other.

The rig:

- refuses to start unless the `irsync` binary it found was built with the
  fault feature (it checks, not assumes, and says what to build);
- tells the daemon and every client which profile, which side (client or
  daemon) and which seed to use;
- passes the client a reconnect window instead of irsync-stress's
  `--reconnect 0`, and `--timeout 10`, so a dead zone of 20 s or more
  always forces a reconnect and a 2 s handover never does;
- uses the profile's default sizes unless `--iterations`,
  `--max-total-size`, `--max-file-size` or `--steps` override them;
- records the profile and its parameters in `run.toml` and prints the
  profile in the header.

Default sizes, chosen to fit about 10 minutes (Claude's estimate from the
rates above):

| profile | iterations | max total size | max file size |
| --- | --- | --- | --- |
| `4g-vehicle` | 10 | 256M | 32M |
| `dsl-video` | 10 | 32M | 4M |

A pull under `dsl-video` uses the fast direction and finishes well inside
the limit; the sizes are set for the slow one.

### Seeds and who owns the outage

The run's seed drives the faults as well as filegen. The two processes
draw from separate streams derived from it (one per side, and per
iteration on the client side), so they do not make correlated decisions.
The fault stream is separate from filegen's, so a seed makes the same
trees here as in irsync-stress, which lets a failure here be compared with
the same seed over a perfect network.

Outages are owned by the client process (Claude's call): during an outage
it drops every packet it would send and every packet it receives, which
cuts both directions without the daemon needing to know the schedule. The
dead zone's timeline therefore starts with each client process, which is
also what lets every sync get one. The daemon's fault layer only shapes
its own direction (rate, queue, delay, loss).

The fault layer logs, in the client log, when a dead zone opens and when
it closes, so the rig can tie what irsync did to the outage that caused
it.

### Verification

On top of irsync-stress's checks (client exit, manifests, `fileset
check`):

- After every iteration the daemon must still be running. A daemon that
  died is a failed run (exit 2), with its log kept.
- What counts as a reconnect (from Mike's "reconnect and resume the
  session"): a session interrupted by a dead zone and then resumed, the
  same session, after it. A redial log line is not proof on its own:
  irsync logs `connection lost; reconnecting` once per retry, and also on
  an ordinary failed first dial before any session exists.
- In an iteration whose dead zone opened, the session must have been
  interrupted and resumed; a dead zone that opened without that following
  is a failed iteration (the outage was meant to force it).
- A sync can end before its dead zone was due (very little to transfer).
  That iteration is noted, not failed. A run in which no dead zone ever
  bit is reported as "faults did not bite" and does not count as a pass;
  it exits with a code of its own, distinct from 0, 1 and 2. With a dead
  zone in every sync this should be rare and points at a real problem.
- A reconnect with no dead zone open (under `dsl-video`, or caused by a
  handover alone) is a failed iteration (Claude's call; Mike may relax it
  to a warning). A slow but working link that makes irsync give up is the
  kind of weakness the rig exists to find.
- Each client log and the daemon log say, near their start, that the fault
  layer is active and with which profile and side; the rig checks for
  that line, so a binary that silently ignored the profile is caught.
- After a passing run, no `.irsync.*.part` is left in either tree (as
  `tools/clean-check.sh` checks for irsync-stress runs): interrupted
  transfers are where a stranded `.part` would come from.

### Output

As irsync-stress's, with a `network:` line in the header naming the
profile, the number of outages that forced a reconnect on each iteration
line, and the total in the footer.

### Campaign tool

A script of its own beside the existing ones (for example
`crates/<rig>/tools/sweep.sh`): many seeds, both directions, every
profile, a few runs at a time, keeping only failing roots, as
`tools/breadth.sh` does. `tools/breadth.sh` and `tools/matrix.sh` are not
changed.

## Testing

- Unit tests of the link model, with a fake clock: rate limiting, the
  queue filling and dropping, cross traffic, outages dropping everything,
  the same seed giving the same draws.
- An irsync integration test that runs a real push and pull through the
  fault layer with a short scripted outage (seconds, not the profile's),
  and asserts the session was interrupted and resumed and the tree
  arrived. It must not take minutes.
- An end-to-end test of the rig, like irsync-stress's: a tiny run under a
  profile with shortened parameters, checking the header, the reconnect
  count and the "faults did not bite" exit.
- The anti-goal "not in a normal build" is checked: a profile given to a
  binary built without the feature is refused, not ignored.
- irsync-stress's tests pass unchanged.

## Documentation

- A README for the new crate: the command, the two profiles, the build it
  needs, the exit codes, the reconnect count, the campaign script.
- `docs/STRESS_BUG_HUNTING.md`: a pointer to the new rig for network
  faults; section 6 item 3 (kill-and-resume) points at the second project.
- A decisions file for the rig, as irsync-stress has
  `docs/IRSYNC_HARNESS_DECISIONS.md`, and the profile numbers once tuned.
- `DEVELOPMENT.md`: how to build the fault-feature binaries into their own
  target directory, and that the rig exists beside irsync-stress.

## Left to the builder

- The crate's and the command's names, and how much of the `stress`
  crate's library it reuses or moves into shared code (irsync-stress must
  behave identically either way).
- The cargo feature's name, which crates it reaches, and the separate
  target directory's name.
- How the profile, side and seed reach the two processes (a hidden CLI
  option, an environment variable or a config key), provided a build
  without the feature cannot be made to inject faults.
- How the custom transport carries packets: most likely a real loopback
  UDP socket underneath, with the fault layer deciding each packet's fate
  on the sending side, and the client's `--address` turned into the
  custom address. Whatever the mechanism, no other path between the two
  peers may remain (iroh's IP transports cleared, relay and lookups off as
  in the isolated mode irsync-stress uses).
- Where the link model's queue lives (sender side per direction is the
  natural fit, since each process knows its own direction).
- The loss model's exact form, and how reordering and duplication are
  drawn, within the profiles above.
- The reconnect window passed to the client, as long as it is longer than
  the longest outage.
- How the rig recognises "interrupted, then the same session resumed" in
  the client log. If irsync logs nothing that says so unambiguously, add
  one log line to irsync that does, rather than inferring it.
- How "while data is moving" is judged when placing a sync's dead zone
  (for example, after a random amount of payload sent, or a random delay
  after the first data packet).
- The new exit code's number.

## Later

- The second project: killing the client mid-sync and crashing the daemon,
  with a rig that restarts them (`STRESS_BUG_HUNTING.md` section 6
  item 3).
- More profiles (satellite, congested Wi-Fi), and profiles that change
  over a run.
