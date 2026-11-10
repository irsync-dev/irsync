# How netstress works

Part of [Testing](testing.md): how the netstress rig works inside.

This page explains what happens inside the network-fault rig: how the fault
layer gets between irsync and the network, how it decides what happens to
each packet, and how the rig decides whether a run passed. For how to use
it, see [Network stress with netstress](netstress.md). The decisions made
while building it and the first results are in
[NETSTRESS_DECISIONS.md](https://github.com/irsync-dev/irsync/blob/main/docs/NETSTRESS_DECISIONS.md).

## The big picture

There are two parts:

1. **The fault layer**, inside irsync itself. It is compiled in only with
   the cargo feature `netsim`, and switched on at run time by the
   `IRSYNC_NETSIM` environment variable. When it is on, every QUIC packet
   irsync sends or receives passes through a simulated link.
2. **The rig**, `irsync-netstress`. It is irsync-stress's loop (make a
   tree, sync it, check it, change it, repeat) with the variable set on the
   daemon and on every client, plus checks that read the client logs to
   make sure the network did what it should.

```text
                         irsync-netstress
            (filegen, fileset, run.toml, logs, checks)
               |                                  |
     spawns once, with                  spawns once per iteration, with
     IRSYNC_NETSIM=...,side=daemon      IRSYNC_NETSIM=...,side=client,stream=<i>
               |                                  |
               v                                  v
   +-------------------------+        +-------------------------+
   | irsync daemon           |        | irsync client           |
   |   session, files        |        |   session, files        |
   |   iroh endpoint, QUIC   |        |   iroh endpoint, QUIC   |
   |   +-------------------+ |        | +-------------------+   |
   |   | netsim transport  | |        | | netsim transport  |   |
   |   |  downlink model   | |        | |  uplink model     |   |
   |   |                   | |        | |  handovers,       |   |
   |   |                   | |        | |  dead zone        |   |
   |   +---------+---------+ |        | +---------+---------+   |
   +-------------|-----------+        +-----------|-------------+
                 |     real UDP sockets on 127.0.0.1           |
                 +---------------------------------------------+
```

Both programs are the real irsync binaries, running as separate processes
on this machine, exactly as in irsync-stress. Only the network between
them is simulated, and it is simulated in real time: a 1 Mbit/s uplink
really carries 1 Mbit/s.

## Why a custom transport

iroh, the networking library irsync uses, lets a program add its own
"custom transport": a piece of code that carries iroh's packets instead of
an ordinary UDP socket. The fault layer is such a transport. It still uses
a real UDP socket on 127.0.0.1 underneath, but it decides for every packet
whether, when and how often it gets sent.

The alternatives, and why they were not used:

- **A proxy between client and daemon**: ruled out in the
  [design](https://github.com/irsync-dev/irsync/blob/main/docs/superpowers/specs/2026-11-06-network-faults-design.md).
- **The operating system's traffic shaping** (`pf` and dummynet on macOS,
  `tc netem` on Linux): needs root, and differs per system.
- **turmoil or another whole-program simulator**: needs both peers in one
  process on a simulated clock with simulated sockets. irsync talks through
  iroh and its QUIC stack with their own sockets, and does real, blocking
  disk I/O that a simulated clock does not cover.

The custom transport needs only a few hooks in irsync, works without root
on macOS and Linux, and keeps the real binaries and the real clock. Its
cost: iroh marks the hook as unstable, so an iroh upgrade may break it.
Because all of it is behind the `netsim` feature, such a break shows up
only as a failure to build the fault build, never in a normal build.

## How irsync is wired to the fault layer

Everything sits behind `#[cfg(feature = "netsim")]`. A normal build has
none of this code and never reads the variable.

- **The feature** (`crates/irsync/Cargo.toml`) turns on iroh's
  `unstable-custom-transports` and a few small dependencies the transport
  needs (`iroh-base`, `n0-watcher`, `noq-udp`, `rand`, `rand_chacha`).
- **The version** (`crates/irsync/src/build.rs`): a fault build's
  `irsync --version` ends with ` [netsim]`. The rig and the sweep script
  check this before doing anything.
- **Reading the variable** (`netsim::active()` in
  `crates/irsync/src/net/netsim/mod.rs`): read once per process and
  remembered. Unset means off. A value that does not parse is an error
  every time it is asked for, so irsync stops instead of silently running
  without faults.
- **Building the endpoint** (`build_endpoint` in
  `crates/irsync/src/net/endpoint.rs`): when the variable is set, irsync
  logs `netsim active` at `warn`, removes iroh's ordinary IP transport and
  its relay transport, and adds the fault transport. With the ordinary
  paths removed, iroh has no way to reach the peer except through the
  fault layer. A test (`crates/irsync/tests/netsim_endpoint.rs`) checks
  that such an endpoint offers no IP or relay address. The transport binds
  to the first IPv4 bind address irsync was given, or `127.0.0.1` on a
  free port.
- **The daemon's address** (`listen_addrs` in
  `crates/irsync/src/daemon.rs`): the daemon prints the fault transport's
  socket address in its startup lines, so the rig (or a person) knows
  where to point the client.
- **The client's dial** (`dial_addrs` in `crates/irsync/src/main.rs`):
  the client turns each address it would dial into a fault-transport
  address. IPv6 addresses are refused.

A fault-transport address is the IPv4 address's 4 bytes followed by the
port's 2 bytes, tagged with the transport id `0x6e65_7473_696d` ("netsim"
in ASCII).

Each irsync process builds its endpoint once. A client reconnecting after
a dead zone reuses the same endpoint, so the same fault transport, with
the same link and outage state, carries the whole sync.

## The path of one packet

Each side simulates only **its own outgoing direction**: the client's
transport is the uplink (client to daemon), the daemon's transport is the
downlink (daemon to client). So every packet is shaped exactly once, by
the side that sends it.

Outages are simulated only on the client, but in both directions: the
client's transport drops what it sends and what it receives while a
handover or the dead zone is on. That cuts the whole link without the two
processes having to agree on timing.

**Sending** (`Shared::send` in `transport.rs`):

```text
iroh hands over a transmit (one packet, or several packed together)
  |
  split into individual packets
  |
  for each packet:
    count it as sent
    client only: is the link up (no handover, no dead zone)?    no -> drop (outage_drops)
    link model: queue full?                                     yes -> drop (queue_drops)
    link model: lost?                                           yes -> drop (loss_drops)
    link model: when does it arrive? (once, or twice if duplicated)
    put it on the delivery queue with that time
```

The send call always tells iroh "done" at once. The fault layer never
makes iroh wait; a packet that does not fit is dropped, as on a real link.

**Delivering** (`deliver` in `transport.rs`): a background task keeps the
waiting packets sorted by delivery time. When a packet's time comes, it
sends it on the real UDP socket to the peer. Loopback adds almost nothing,
so the packet arrives when the model said it would, to within the tokio
timer's resolution of about 1 ms. On a fast link several packets fall into
the same millisecond and arrive together, so the receiver sends fewer ACKs
than it would on a real link where they come one at a time. Measurements
of ACK traffic undercount for this reason.

**Receiving** (`poll_recv` in `transport.rs`):

```text
read a packet from the real socket
  |
  bigger than iroh's buffer?                                    yes -> drop (truncated)
  client only: is the link up?                                  no -> drop (outage_drops)
  hand it to iroh
```

To spot packets that are too big, the transport reads into a buffer one
byte larger than iroh's; if that extra byte gets filled, the packet would
have been cut short, so it is dropped and counted instead.

## The link model

`Link` in `crates/irsync/src/net/netsim/link.rs` decides each packet's
fate. It has no clock of its own: it is told the current time with each
packet. That keeps it simple to test: the same seed and the same packets
at the same times always get the same fates.

The parameters per profile and direction are in
`crates/irsync/src/net/netsim/profile.rs` (`LinkParams`). For each packet
the model does the following, in this order.

### 1. The queue

The link has a speed and a queue in front of it, like a router's buffer
in front of a slow line. The model treats the queue as an amount of bytes,
not a list of packets:

- Between packets, the queue drains at the link's speed.
- A new packet is added to the queue. If that would make the queue bigger
  than its limit (`queue_bytes`), the packet is **dropped**.
- Otherwise its waiting time is the bytes now in the queue (its own
  included) divided by the link's speed.

So a sender that sends faster than the link sees its packets wait longer
and longer, and then get dropped once the queue is full. That is
"bufferbloat", and it is what QUIC's congestion control has to cope with.

A queue of `u64::MAX` (as in `test-blackout`) means no queue at all: no
waiting, no queue drops.

### 2. Speed changes

When a profile gives a range of speeds (4G: 2 to 30 Mbit/s down), the
model draws a new speed at random within that range every so often (4G:
every 2 to 6 s). The queue drains at the old speed up to the moment of the
change, then at the new one. A fixed speed (DSL) never changes.

### 3. Cross traffic (video)

`dsl-video`'s downlink shares its queue with someone else's video stream.
The video comes in bursts: every 2 to 4 s a burst starts and lasts 1 to
2 s. During a burst:

- While the queue holds less than `hold_bytes` (1280 KiB), the video pours
  in at twice the link speed (`load` = 2.0), so the queue fills at the
  link speed.
- Once the queue reaches `hold_bytes`, the video holds it there, like a
  real video player that backs off once its own traffic starts to queue.
  It takes 10 of the 16 Mbit/s (`share` = 0.625). Whatever irsync has added
  above that level drains at the 6 Mbit/s that is left.

Only irsync's packets can fill the last 256 KiB of the queue. So irsync
loses packets only if it sends faster than its 6 Mbit/s share for long
enough to fill that space, not just because the video is playing. Its
packets do wait behind the video: 1280 KiB at 16 Mbit/s is about 650 ms of
extra delay.

Between bursts the queue drains at the full link speed.

A video that filled the queue completely would drop every irsync packet
for up to about a second in every burst: a total blackout, not an
overloaded line. See call 12 in
[NETSTRESS_DECISIONS.md](https://github.com/irsync-dev/irsync/blob/main/docs/NETSTRESS_DECISIONS.md).

### 4. Bursty loss

Real wireless links lose packets in clumps, not evenly. The model uses
the standard two-state "Gilbert-Elliott" approach: the link is either in a
**good** or a **bad** state, and each state has its own loss rate.

For each packet that got into the queue, the model first decides whether
the state changes, then whether this packet is lost:

| | 4G value | meaning |
| --- | --- | --- |
| `to_bad` | 0.004 | chance per packet that a good link turns bad |
| `to_good` | 0.1 | chance per packet that a bad link recovers |
| `loss_good` | 0.002 | loss while good (0.2%) |
| `loss_bad` | 0.2 | loss while bad (20%) |

With these numbers a good spell lasts about 250 packets and a bad spell
about 10, the link is bad about 4% of the time, and about 1% of packets
are lost overall, many of them close together. `dsl-video` never goes bad
and loses 0.01% at random.

A lost packet has already taken its place in the queue, as if it were lost
on the wire after the bottleneck.

### 5. Delay, jitter, reordering, duplicates

- **Delay**: every packet gets the fixed one-way delay (`base_delay_ms`)
  on top of its queueing time.
- **Jitter**: an offset that drifts slowly, up to `jitter_ms` either way.
  Every 100 ms it takes a random step of up to a quarter of that amount.
  So delay changes smoothly over time instead of jumping per packet.
- **Order**: jitter alone never changes the order of packets. A packet
  never arrives before the one sent just before it.
- **Reordering**: a small share of packets (4G: 0.5%) gets 5 to 30 ms
  extra and may arrive after packets sent later.
- **Duplicates**: a small share (4G: 0.1%) is delivered twice, the second
  copy 1 ms after the first.

## Handovers and dead zones

`Outages` in `crates/irsync/src/net/netsim/outage.rs` decides when the
link is down. It exists only on the client. It is asked for every packet
the client sends or receives, and answers "up", "handover" or "dead zone".

**Handovers** run on a timetable: the first starts 30 to 90 s after the
transport starts, each lasts 0.2 to 2 s, and the next one starts 30 to
90 s after the previous one started. They are short on purpose: shorter
than the client's 10 s `--timeout`, so QUIC simply resends what was lost
and the sync carries on. A handover that would start during the dead zone
is skipped.

**The dead zone** is triggered by traffic, not time. The model counts the
bytes of every packet passing the client, in both directions. When the
count reaches a threshold drawn at the start (4G: somewhere between 256
KiB and 4 MiB), the dead zone opens and lasts its drawn length (4G: 20 to
60 s). It happens once per client process, so once per sync.

Counting traffic instead of time means the dead zone always lands while
data is moving, whether the link is fast or slow, and never while the
client is still starting up or after it has finished. The cost: a sync
that moves less than the threshold never gets a dead zone. The rig notes
that and only complains if no sync of the whole run got one.

The client logs `netsim: dead zone open secs=N` when it opens. It notices
the end on the first packet after the end time and then logs
`netsim: dead zone closed`. Since the client keeps trying to reconnect
during the dead zone, there is a packet soon after the end.

## Time

Both models get their time handed to them: the time since the transport
was created, from tokio's clock. This has two effects:

- **The models can be tested without waiting.** The unit tests feed them
  thousands of packets at made-up times and check the results (for
  example: 4G loses between 0.4% and 2% of packets, and some losses come
  in a row).
- **A real run depends on real timing.** Which packet arrives during a
  dead zone, or finds the queue full, depends on when irsync sends it,
  which depends on CPU load and thread scheduling. That is why a seed does
  not replay a run packet for packet.

The daemon is one process for the whole run, so its downlink model runs
for the whole run too. Its speed changes and video bursts carry on across
iterations, including the time between them. Each client is a new process
with a fresh uplink model and a fresh outage model.

## Seeds and streams

All random choices come from seeded generators (ChaCha8), one per model:
one for the link and, on the client, one for the outages. Each
generator's seed is made from four things:

- the run's seed (`--seed`),
- the side (client or daemon),
- the stream number,
- the purpose (link or outages).

`fault_seed` in `crates/irsync/src/net/netsim/mod.rs` mixes these one
after another with splitmix64, a standard way to scramble a number so that
inputs that differ in any part give unrelated outputs.

The rig numbers streams like this:

| Process | side | stream |
| --- | --- | --- |
| the daemon | daemon | 0 |
| the ping round's client | client | 0 |
| iteration i's client | client | i |

So each iteration has its own fault schedule, and that schedule does not
depend on how many iterations ran before it: iteration 5 of seed 3 gets
the same draws whether the run has 5 iterations or 50. A test checks
that 10 seeds times 10 streams give 100 different seeds for each purpose.
Simpler mixing makes many schedules come out the same; see call 5 in
[NETSTRESS_DECISIONS.md](https://github.com/irsync-dev/irsync/blob/main/docs/NETSTRESS_DECISIONS.md).

## What irsync does during a dead zone

The rig gives the client `--timeout 10` (under the real profiles) and
`--reconnect 5m`. A typical 4G dead zone of, say, 40 s then goes like this:

```text
  0 s   dead zone opens. Every packet in both directions is dropped.
        QUIC resends and sends keep-alives (every 5 s); all are dropped.
 10 s   client: "no activity; dropping the connection secs=10"
        client: "session failed", "connection lost; reconnecting"
        The client keeps trying to connect again, inside its 5 minute window.
 40 s   dead zone ends. The next connection attempt gets through.
        client: "netsim: dead zone closed"
        client: "session started session=<same id> resumed=true"
        The sync carries on where it stopped; partly sent files are resumed.
```

The daemon notices the old connection is gone (it logs
`connection failed: control stream: connection lost`) and accepts the new
one. Its QUIC idle timeout is 60 s.

Why the numbers fit together:

- `--timeout 10` is longer than the longest handover (2 s), so a handover
  never forces a reconnect, and shorter than the shortest dead zone
  (20 s), so a dead zone always does.
- `--reconnect 5m` is far longer than the longest dead zone (60 s), so the
  client never gives up because of the dead zone itself.

The sender waits for the receiver to confirm each piece of a file it sent,
with no time limit of its own: on a slow link, many pieces share the line
and confirmation can take minutes. A fixed limit there fails healthy pushes
over slow uplinks, as the first sweeps in
[NETSTRESS_DECISIONS.md](https://github.com/irsync-dev/irsync/blob/main/docs/NETSTRESS_DECISIONS.md) show. A dead peer
still ends the wait, through a lost connection.

## How the rig judges a run

On top of irsync-stress's checks (client exit, manifests, `fileset
check`), the rig reads each iteration's client log (`netlog.rs` in
`crates/netstress/src`). It removes the colour codes, then walks the
lines in order:

- `netsim active`: the fault layer was on.
- `netsim: dead zone open`: count a dead zone; from here until the next
  `session started`, the client is "in a dead zone".
- `connection lost; reconnecting` outside a dead zone: count a **stray
  reconnect**. Inside a dead zone, reconnects are expected and not counted.
- `session started`: note its `session=` id (the first one is the sync's
  session) and its `resumed=` value. If it comes after a dead zone, says
  `resumed=true` and has the first session's id, count the dead zone as
  **resumed**. Either way, the dead zone part is over.

The `session started` line also carries a `session_id=` and a `resumed=`
in its prefix (from the logging context), which can differ from the
event's own values after a reconnect. The parser only reads what comes
after the words `session started`.

The iteration then fails with a reason when:

1. `netsim active` is missing, or
2. there was any stray reconnect, or
3. more dead zones opened than were resumed.

Exit codes for the whole run (`report.rs`):

- 1 if any iteration failed, or a `.irsync.*.part` file was found
  anywhere in either tree after the daemon stopped;
- otherwise 3 if the profile has dead zones but no iteration logged one;
- otherwise 0.

Exit 2 comes from anything that stops the rig before it can judge: a
missing or non-fault irsync, a daemon that does not log `netsim active`
within 15 s or dies, a failed ping round, or a client exit other than 0
or 23.

## The counts

Each transport counts what happens to its packets and logs it at `warn`
every 10 s while traffic flows and once when the transport is dropped:

```text
netsim: counts sent=4814 queue_drops=0 loss_drops=0 outage_drops=0 send_errors=0 truncated=0
```

The rig does not judge these; they are there for a person reading a
failure. They answer "did the network do what the profile says, or
something else?". For example, losses far above what the profile should
give point at the fault layer; a stall with no drops at all points at
irsync. The meaning of each count is in the
[user guide](netstress.md#reading-the-logs).

The final line is best effort: a process that exits without closing its
endpoint, like the daemon when the rig stops it, skips it, and its last
10-second line is the latest one.

## Simplifications

Things the model does not do, on purpose or for now:

- **The queue is a byte count, not a list of packets.** Waiting time is
  the bytes in the queue divided by the full link speed. While the video
  holds the queue, irsync's bytes above the video's level actually drain
  at irsync's 6 Mbit/s share, so the model's delay for them is a little
  lower than a strict model would give.
- **Each side shapes only what it sends**, and the two directions are
  independent. Real links sometimes couple them (a busy downlink delaying
  uplink acknowledgements); this model does not.
- **The daemon's downlink is one link for the whole run**, shared by
  whichever client is connected. Only one client runs at a time, so this
  does not matter for the rig.
- **Outages are only on the client.** The daemon's transport never cuts
  the link; the client's drops both directions.
- **No NAT, relays, address changes or path changes.** The fault layer
  only decides what happens to packets on one path between two fixed
  addresses.
- **IPv4 and loopback only.**

## Where the code is

| Code | What it does |
| --- | --- |
| `crates/irsync/src/net/netsim/mod.rs` | The variable, profile names, seeds (`fault_seed`), the address format, `netsim active`. |
| `crates/irsync/src/net/netsim/profile.rs` | The numbers of every profile. |
| `crates/irsync/src/net/netsim/link.rs` | The link model: queue, speed, video, loss, delay. |
| `crates/irsync/src/net/netsim/outage.rs` | Handovers and the dead zone. |
| `crates/irsync/src/net/netsim/transport.rs` | The iroh custom transport: sending, delivering, receiving, counts. |
| `crates/irsync/src/net/endpoint.rs`, `daemon.rs`, `main.rs`, `build.rs` | The hooks into irsync. |
| `crates/netstress/src/run.rs` | The rig's run: tools check, daemon, ping, iterations, partial files. |
| `crates/netstress/src/netlog.rs` | Reading a client log for the network story. |
| `crates/netstress/src/report.rs` | Header, iteration lines, footer, exit codes. |
| `crates/netstress/src/profile.rs` | Run defaults and client timeout per profile. |
| `crates/netstress/src/cli.rs`, `run_file.rs` | Command line and `run.toml`. |
| `crates/netstress/tools/sweep.sh` | Many runs at once. |
| `crates/stress` | The loop, layout, daemon handling and checks the rig reuses. |
