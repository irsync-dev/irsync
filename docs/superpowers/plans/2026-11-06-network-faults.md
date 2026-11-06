# Network-Fault Test Rig Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A new crate `netstress` building `irsync-netstress`, which runs the sync-and-verify loop of irsync-stress through a simulated bad network (`4g-vehicle`, `dsl-video`), injected by a fault layer that only exists in irsync builds with the cargo feature `netsim`.

**Architecture:** Two halves. In irsync, behind `netsim`: a pure link model (rate, queue, cross traffic, delay, jitter, loss, reordering, duplication) and an outage schedule, driven by a seeded RNG and an injected clock; an iroh custom transport over a real loopback UDP socket that passes every datagram through them; and three small hooks (endpoint build, the daemon's `listening on:` line, the client's dial address). The fault settings reach each process through the environment variable `IRSYNC_NETSIM`. In the new `netstress` crate: a run loop built from the `stress` crate's library modules, which sets that variable for the daemon and every client, reads the client logs to prove each dead zone forced a reconnect and a resume, and reports. irsync-stress is untouched apart from three additive library hooks it calls with today's values.

**Tech Stack:** Rust 2021; iroh 1.3 with feature `unstable-custom-transports` (`CustomTransport`, `CustomEndpoint`, `CustomSender`, `CustomAddr`, `RecvInfo`, `noq_udp::{Transmit, RecvMeta}`; reference implementation `iroh-1.3.0/src/test_utils/test_transport.rs`); tokio `UdpSocket`; `rand` 0.8 + `rand_chacha` 0.3 (workspace); clap 4, serde/toml, anyhow as in `stress`.

**Spec:** `docs/superpowers/specs/2026-11-06-network-faults-design.md`

## Global Constraints

- irsync-stress behaves identically: its command line, output, exit codes and `crates/stress/tests/e2e.rs` pass unchanged. New `stress` library functions are additive; the old ones keep their signatures and delegate.
- `netstress` depends on `stress` (path) but not on `irsync`, `filegen` or `fileset`; it drives them through their command lines, as `stress` does.
- No fault code in a build without `netsim`: every netsim module, field and branch is under `#[cfg(feature = "netsim")]`. Without the feature, `IRSYNC_NETSIM` is never read.
- Feature: `netsim = ["iroh/unstable-custom-transports", "dep:rand", "dep:rand_chacha"]` in `crates/irsync/Cargo.toml`; `rand` and `rand_chacha` become optional workspace dependencies of irsync.
- Fault build: `cargo build --release --workspace --features irsync/netsim --target-dir target/netsim`. Binaries land in `target/netsim/release/`; `target/release/` is never a fault build.
- A fault build's `irsync --version` ends with ` [netsim]` (e.g. `irsync 0.1.0 (abc1234) [netsim]`).
- Environment variable `IRSYNC_NETSIM`, value `profile=<name>,side=<client|daemon>,seed=<u64>,stream=<u64>`, keys in any order, all four required.
- Profile names: `4g-vehicle`, `dsl-video`, `test-blackout` (the last for tests only, Claude's call: no shaping, one short dead zone).
- Custom transport id: `0x6e65_7473_696d` ("netsim"); a `CustomAddr`'s data is the IPv4 address's 4 bytes then the port's 2 bytes, big-endian.
- Log lines the rig parses, emitted at `warn` so any log level shows them (exact message text): `netsim active` (fields `profile`, `side`, `seed`, `stream`), `netsim: dead zone open` (field `secs`), `netsim: dead zone closed`. irsync's existing `connection lost; reconnecting` and `session started` (fields `session`, `resumed`) are used as they are.
- netstress exit codes: 0 pass, 1 an iteration failed, 2 the rig or a tool could not run, 3 faults did not bite, 130 interrupted.
- Client flags under a profile: irsync-stress's fixed list with `--reconnect 5m` instead of `--reconnect 0`, and `--timeout <profile's client timeout>` before the passthrough args.
- Every task ends with these clean: `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo clippy --workspace --all-targets --features irsync/netsim -- -D warnings`; `cargo test --workspace`; and, for tasks touching irsync, `cargo test -p irsync --features netsim`. CI is not running on this repo: run them locally (macOS here; there is no Linux machine).

## Review Focus

1. Client logs are written with ANSI colour codes: the log parser must strip them before matching, or every run reports "faults did not bite". Pinned in Task 7 (`parses_a_coloured_log`).
2. A rig pointed at a normal (non-netsim) irsync: it must stop with exit 2 and the build command, never run an unfaulted loop that passes. Pinned in Task 8 (`a_normal_irsync_is_refused`).
3. QUIC hands the sender batched datagrams (`Transmit::segment_size`): each segment is its own packet and gets its own fate; treating the batch as one packet corrupts the stream. Pinned in Task 4 (`segmented_transmit_is_split`).
4. Jitter must not reorder every packet: QUIC treats reordering as loss. Only packets the model chose to reorder may overtake. Pinned in Task 2 (`jitter_alone_keeps_order`).
5. Every iteration is a new client process with a new UDP port: the daemon's transport must answer whichever address a packet came from. Pinned in Task 8 (the e2e runs three iterations).

---

### Task 1: The `netsim` feature, its settings and addresses

**Files:**
- Modify: `Cargo.toml` (nothing to add: `rand`, `rand_chacha` are already workspace deps)
- Modify: `crates/irsync/Cargo.toml` (feature `netsim`; optional deps `rand`, `rand_chacha`)
- Modify: `crates/irsync/src/net/mod.rs` (`#[cfg(feature = "netsim")] pub mod netsim;`)
- Create: `crates/irsync/src/net/netsim/mod.rs`
- Modify: `crates/irsync/src/build.rs:91-99` (`Display` appends ` [netsim]` under the feature)

**Interfaces:**
- Produces (all in `irsync::net::netsim`):
  ```rust
  pub const ENV: &str = "IRSYNC_NETSIM";
  pub const TRANSPORT_ID: u64 = 0x6e65_7473_696d;
  #[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Side { Client, Daemon }
  #[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum ProfileName { FourGVehicle, DslVideo, TestBlackout }
  impl std::str::FromStr for ProfileName   // "4g-vehicle" | "dsl-video" | "test-blackout"
  impl std::fmt::Display for ProfileName   // the same names
  #[derive(Clone, Debug, PartialEq, Eq)]
  pub struct Spec { pub profile: ProfileName, pub side: Side, pub seed: u64, pub stream: u64 }
  impl Spec { pub fn parse(s: &str) -> anyhow::Result<Spec>; }
  pub fn active() -> anyhow::Result<Option<&'static Spec>>;   // reads ENV once (OnceLock); a bad value is an error every call
  pub fn custom_addr(a: std::net::SocketAddrV4) -> iroh::endpoint::transports::CustomAddr;
  pub fn socket_addr(c: &CustomAddr) -> Option<std::net::SocketAddrV4>;   // None for another id or bad length
  ```

- [ ] **Step 1: Write the failing tests** (in `netsim/mod.rs`, `#[cfg(test)] mod tests`)

```rust
#[test]
fn spec_parses_in_any_order() {
    let s = Spec::parse("stream=3,seed=7,side=client,profile=4g-vehicle").unwrap();
    assert_eq!(s, Spec { profile: ProfileName::FourGVehicle, side: Side::Client, seed: 7, stream: 3 });
}
#[test]
fn spec_rejects_missing_unknown_and_bad_values() {
    for bad in ["profile=4g-vehicle,side=client,seed=7",              // stream missing
                "profile=lte,side=client,seed=7,stream=0",            // unknown profile
                "profile=dsl-video,side=both,seed=7,stream=0",        // bad side
                "profile=dsl-video,side=daemon,seed=x,stream=0",      // bad number
                "profile=dsl-video,side=daemon,seed=1,stream=0,x=1"] {// unknown key
        assert!(Spec::parse(bad).is_err(), "{bad}");
    }
}
#[test]
fn custom_addr_round_trips() {
    let a: SocketAddrV4 = "127.0.0.1:43210".parse().unwrap();
    let c = custom_addr(a);
    assert_eq!(c.id(), TRANSPORT_ID);
    assert_eq!(socket_addr(&c), Some(a));
    assert_eq!(socket_addr(&CustomAddr::from_parts(0x20, &[1, 2])), None);
}
#[test]
fn version_marks_a_netsim_build() {
    assert!(crate::build::LONG_VERSION.ends_with(" [netsim]"));
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p irsync --features netsim --lib netsim`
Expected: FAIL to compile (`Spec`, `custom_addr`, … not defined; the feature does not exist yet).

- [ ] **Step 3: Implement** the feature in `crates/irsync/Cargo.toml`, the module with the interfaces above, and in `build.rs`'s `Display for BuildInfo` a `#[cfg(feature = "netsim")] f.write_str(" [netsim]")?;` after the closing parenthesis. `active()` caches the parse result in a `OnceLock<Result<Option<Spec>, String>>` and maps the cached error back into `anyhow`.

- [ ] **Step 4: Run them to verify they pass**

Run: `cargo test -p irsync --features netsim --lib netsim` → PASS. Also `cargo build -p irsync` (no feature) → builds, and `target/debug/irsync --version` has no ` [netsim]`.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/irsync/Cargo.toml crates/irsync/src/net/mod.rs crates/irsync/src/net/netsim/mod.rs crates/irsync/src/build.rs
git commit -m "feat(netsim): the feature, its IRSYNC_NETSIM settings and custom addresses"
```

---

### Task 2: The link model and the profiles

**Files:**
- Create: `crates/irsync/src/net/netsim/link.rs`
- Create: `crates/irsync/src/net/netsim/profile.rs`

**Interfaces:**
- Consumes: `ProfileName` (Task 1).
- Produces:
  ```rust
  // profile.rs
  #[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Dir { Up, Down }   // Up = client→daemon
  #[derive(Clone, Debug)] pub struct LinkParams {
      pub rate_bps: (u64, u64),                 // drawn uniformly in [min, max]; min == max: fixed
      pub rate_change_ms: (u64, u64),           // how often the rate is redrawn; ignored when fixed
      pub queue_bytes: u64,                     // u64::MAX: no queue limit, no queueing delay
      pub base_delay_ms: u64,                   // one way
      pub jitter_ms: u64,                       // amplitude of the slowly varying offset
      pub loss: GilbertElliott,
      pub reorder: f64, pub duplicate: f64,     // per packet probabilities
      pub cross: Option<CrossTraffic>,
  }
  #[derive(Clone, Copy, Debug)] pub struct GilbertElliott { pub to_bad: f64, pub to_good: f64, pub loss_good: f64, pub loss_bad: f64 }
  #[derive(Clone, Copy, Debug)] pub struct CrossTraffic { pub every_ms: (u64, u64), pub burst_ms: (u64, u64), pub load: f64 } // load: arrival rate as a multiple of the link rate during a burst
  pub fn link(p: ProfileName, d: Dir) -> LinkParams;
  // link.rs
  pub struct Link { /* params, rng: rand_chacha::ChaCha8Rng, queue state, rate state, GE state, jitter offset, last delivery */ }
  impl Link {
      pub fn new(params: LinkParams, seed: u64) -> Link;
      /// What happens to one datagram of `len` bytes offered at `now` (time since the link was made):
      /// empty = dropped; otherwise the delivery times (two when duplicated).
      pub fn offer(&mut self, now: std::time::Duration, len: usize) -> Vec<std::time::Duration>;
  }
  ```

Profile values (the spec's numbers made concrete; Claude's call, recorded in the decisions file in Task 9):

| | `4g-vehicle` Down | `4g-vehicle` Up | `dsl-video` Down | `dsl-video` Up | `test-blackout` (both) |
| --- | --- | --- | --- | --- | --- |
| rate_bps | 2M–30M | 0.7M–10M | 16M fixed | 1M fixed | unlimited (`u64::MAX`) |
| rate_change_ms | 2000–6000 | 2000–6000 | – | – | – |
| queue_bytes | 512 KiB | 256 KiB | 1.5 MiB | 64 KiB | `u64::MAX` |
| base_delay_ms | 30 | 30 | 10 | 10 | 0 |
| jitter_ms | 20 | 20 | 2 | 2 | 0 |
| loss (to_bad, to_good, good, bad) | 0.004, 0.1, 0.002, 0.2 | same | 0, 1, 0.0001, 0 | same | 0, 1, 0, 0 |
| reorder / duplicate | 0.005 / 0.001 | same | 0 / 0 | 0 / 0 | 0 / 0 |
| cross | – | – | every 2000–4000, burst 1000–2000, load 2.0 | – | – |

Model rules the tests pin (the implementer writes the rest):
- Queue: occupancy in bytes drains at the current rate; cross traffic adds `load × rate` bytes per second while a burst lasts. A datagram that would exceed `queue_bytes` is dropped. Otherwise its queueing delay is `occupancy_after × 8 / rate`.
- Delivery time = `now + queueing delay + base delay + jitter offset`, where the jitter offset is a random walk inside `[-jitter, +jitter]` redrawn in small steps every 100 ms of `now`, so consecutive packets see nearly the same offset.
- Order: a delivery time is never earlier than the previous delivery's, unless this packet was chosen for reordering (then it gets 5–30 ms extra delay and may be overtaken; the next packets are not held back by it).
- Duplicate: a second delivery 1 ms after the first.
- Same `seed`, same sequence of offers → same fates.

- [ ] **Step 1: Write the failing tests** (`link.rs`, `#[cfg(test)]`; times are `Duration`s handed in, no real clock)

```rust
fn ms(n: u64) -> Duration { Duration::from_millis(n) }
#[test] fn same_seed_same_fates() {
    let run = || { let mut l = Link::new(link(ProfileName::FourGVehicle, Dir::Down), 7);
                   (0..5000).map(|i| l.offer(ms(i), 1200)).collect::<Vec<_>>() };
    assert_eq!(run(), run());
}
#[test] fn rate_limits_throughput() {          // dsl-video Up: 1 Mbit/s, 64 KiB queue
    let mut l = Link::new(link(ProfileName::DslVideo, Dir::Up), 1);
    let delivered = (0..10_000).filter(|i| !l.offer(ms(*i), 1250).is_empty()).count(); // offered: 10 Mbit/s for 10 s
    assert!((900..=1_100).contains(&delivered), "{delivered}"); // ≈ 1 Mbit/s × 10 s / 10 kbit
}
#[test] fn full_queue_drops_and_delays() {     // the last accepted packet waits ≈ 64 KiB at 1 Mbit/s ≈ 524 ms
    let mut l = Link::new(link(ProfileName::DslVideo, Dir::Up), 1);
    let fates: Vec<_> = (0..200).map(|_| l.offer(ms(0), 1250)).collect();
    assert!(fates.iter().any(|f| f.is_empty()));
    let last = fates.iter().rev().find(|f| !f.is_empty()).unwrap()[0];
    assert!((ms(480)..=ms(560)).contains(&last), "{last:?}");
}
#[test] fn cross_traffic_causes_bufferbloat() { // dsl-video Down: some packet within 10 s waits ≥ 300 ms
    let mut l = Link::new(link(ProfileName::DslVideo, Dir::Down), 3);
    let worst = (0..10_000).filter_map(|i| l.offer(ms(i), 300).first().map(|d| *d - ms(i))).max().unwrap();
    assert!(worst >= ms(300), "{worst:?}");
}
#[test] fn jitter_alone_keeps_order() {         // 4g with reorder and duplicate set to 0
    let mut p = link(ProfileName::FourGVehicle, Dir::Down); p.reorder = 0.0; p.duplicate = 0.0;
    let mut l = Link::new(p, 5);
    let times: Vec<_> = (0..20_000).flat_map(|i| l.offer(ms(i / 4), 200)).collect();
    assert!(times.windows(2).all(|w| w[0] <= w[1]));
}
#[test] fn four_g_loses_about_one_percent_in_bursts() {
    let mut l = Link::new(link(ProfileName::FourGVehicle, Dir::Down), 11);
    let lost: Vec<bool> = (0..200_000).map(|i| l.offer(ms(i), 100).is_empty()).collect();
    let rate = lost.iter().filter(|x| **x).count() as f64 / lost.len() as f64;
    assert!((0.004..=0.02).contains(&rate), "{rate}");
    let runs = lost.windows(2).filter(|w| w[0] && w[1]).count();
    assert!(runs > 0, "losses never come in bursts");
}
#[test] fn test_blackout_passes_everything_at_once() {
    let mut l = Link::new(link(ProfileName::TestBlackout, Dir::Up), 1);
    assert!((0..1000).all(|i| l.offer(ms(i), 1500) == vec![ms(i)]));
}
```

(`four_g_loses_about_one_percent_in_bursts` offers one 100-byte packet per ms, 0.8 Mbit/s, below the slowest 4G rate, so the queue never fills and only random loss counts.)

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p irsync --features netsim --lib netsim::link` → FAIL to compile.

- [ ] **Step 3: Implement** `profile.rs` with the table above and `Link` with the rules above in `link.rs`; RNG `ChaCha8Rng::seed_from_u64(seed)`.

- [ ] **Step 4: Run them to verify they pass**

Run: `cargo test -p irsync --features netsim --lib netsim::link` → PASS (run it three times; the tests are deterministic, so a flake is a bug).

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/net/netsim/link.rs crates/irsync/src/net/netsim/profile.rs crates/irsync/src/net/netsim/mod.rs
git commit -m "feat(netsim): link model with rate, queue, cross traffic, jitter, loss; the profiles"
```

---

### Task 3: The outage schedule

**Files:**
- Create: `crates/irsync/src/net/netsim/outage.rs`
- Modify: `crates/irsync/src/net/netsim/profile.rs` (outage parameters per profile)

**Interfaces:**
- Consumes: `ProfileName` (Task 1).
- Produces:
  ```rust
  // profile.rs
  #[derive(Clone, Debug)] pub struct OutageParams {
      pub handover_every_ms: (u64, u64), pub handover_ms: (u64, u64),   // (0,0): none
      pub dead_zone_ms: (u64, u64),                                     // (0,0): none
      pub dead_zone_after_bytes: (u64, u64),                            // traffic seen before it opens
  }
  pub fn outages(p: ProfileName) -> Option<OutageParams>;   // None for dsl-video
  // outage.rs
  #[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum State { Up, Handover, DeadZone }
  pub enum Change { DeadZoneOpened { secs: u64 }, DeadZoneClosed }   // what the transport logs
  pub struct Outages { /* params, rng, schedule, bytes seen */ }
  impl Outages {
      pub fn new(p: OutageParams, seed: u64) -> Outages;
      /// Counts `len` bytes of traffic (either direction) at `now` and says whether the link is up.
      /// Returns the state and, when it changed into or out of the dead zone, the change to log.
      pub fn packet(&mut self, now: Duration, len: usize) -> (State, Option<Change>);
  }
  ```

Values: `4g-vehicle` handover every 30–90 s for 200–2000 ms, dead zone 20–60 s after 256 KiB–4 MiB of traffic, exactly one per `Outages` (one per client process, so one per sync: Mike's choice). `test-blackout` no handovers, dead zone 6–6 s after 256 KiB–256 KiB. A dead zone's clock starts at the packet that crossed the byte threshold; a handover that falls inside the dead zone is skipped.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn exactly_one_dead_zone_after_the_threshold() {
    let mut o = Outages::new(outages(ProfileName::TestBlackout).unwrap(), 1);
    let mut changes = vec![];
    for i in 0..20_000u64 {                     // 1 KiB per ms: threshold reached at ~256 ms
        let (s, c) = o.packet(ms(i), 1024);
        if let Some(c) = c { changes.push((i, c)); }
        if i < 250 { assert_eq!(s, State::Up); }
    }
    assert!(matches!(changes[..], [(a, Change::DeadZoneOpened { secs: 6 }), (b, Change::DeadZoneClosed)] if b - a == 6000));
}
#[test] fn no_dead_zone_before_enough_traffic() {
    let mut o = Outages::new(outages(ProfileName::TestBlackout).unwrap(), 1);
    assert!((0..100_000u64).all(|i| o.packet(ms(i), 1).0 == State::Up));   // 100 KB < 256 KiB
}
#[test] fn four_g_handovers_are_short_and_spread() {
    let mut o = Outages::new(outages(ProfileName::FourGVehicle).unwrap(), 9);
    // 600 s at one small packet per ms, too little traffic for the dead zone's threshold
    let states: Vec<State> = (0..600_000u64).map(|i| o.packet(ms(i), 0).0).collect();
    let spans = spans_of(&states, State::Handover);     // helper in the test module: lengths of runs
    assert!((6..=21).contains(&spans.len()), "{}", spans.len());   // one per 30–90 s
    assert!(spans.iter().all(|n| (200..=2000).contains(n)));
}
#[test] fn dsl_video_has_no_outages() { assert!(outages(ProfileName::DslVideo).is_none()); }
```

- [ ] **Step 2: Run them to verify they fail** — `cargo test -p irsync --features netsim --lib netsim::outage` → FAIL to compile.
- [ ] **Step 3: Implement** `outages()` values and `Outages` as described; RNG `ChaCha8Rng::seed_from_u64(seed)`.
- [ ] **Step 4: Run them to verify they pass** — same command → PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/net/netsim/outage.rs crates/irsync/src/net/netsim/profile.rs
git commit -m "feat(netsim): handovers and one dead zone per client process"
```

---

### Task 4: The custom transport, wired into irsync

**Files:**
- Create: `crates/irsync/src/net/netsim/transport.rs`
- Modify: `crates/irsync/src/net/netsim/mod.rs` (`pub fn transport(spec: &Spec, bind: SocketAddrV4) -> Arc<dyn CustomTransport>`, `pub fn bound_addr() -> Option<SocketAddrV4>`, `pub fn log_active(spec: &Spec)`)
- Modify: `crates/irsync/src/net/endpoint.rs:187-229` (`build_endpoint`: the netsim branch; `NetError::NetSim(anyhow::Error)` under the feature)
- Modify: `crates/irsync/src/daemon.rs:701-709` (announce the netsim socket instead of `bound_sockets()`)
- Modify: `crates/irsync/src/main.rs:481` (dial the custom address)
- Modify: `crates/irsync/tests/common/mod.rs:690` (add `spawn_daemon_binary_env(server_home, env: &[(&str, &str)]) -> BinaryDaemon`; `spawn_daemon_binary` calls it with `&[]`)
- Create: `crates/irsync/tests/netsim.rs` (`#![cfg(feature = "netsim")]`)
- Create: `crates/irsync/tests/netsim_endpoint.rs` (`#![cfg(feature = "netsim")]`; its own test binary, because it sets `IRSYNC_NETSIM` in its own process before the first `active()` call)

**Interfaces:**
- Consumes: `Spec`, `active()`, `custom_addr`, `socket_addr`, `TRANSPORT_ID` (Task 1); `Link`, `link`, `Dir` (Task 2); `Outages`, `outages`, `State`, `Change` (Task 3).
- Produces: a fault build whose daemon prints `listening on: 127.0.0.1:<port>` for its netsim socket and whose client dials it through the fault layer when `IRSYNC_NETSIM` is set; unchanged behaviour when it is not set.

Behaviour (the transport follows `iroh-1.3.0/src/test_utils/test_transport.rs` for the trait mechanics):
- `bind()`: binds a tokio `UdpSocket` on the given address, records it for `bound_addr()`, spawns one delivery task holding a min-heap of `(deliver_at, dst, bytes)` that sleeps until the next one and `send_to`s it. Local address watcher: `vec![custom_addr(local)]`.
- `poll_send`: splits the `Transmit` by `segment_size` (one packet per segment); for each, on the client side asks `Outages::packet` first (`DeadZone`/`Handover` → drop); then `Link::offer` for its own direction (client: Up, daemon: Down); queues each delivery time. Always `Ready(Ok(()))`.
- `poll_recv`: `poll_recv_from` on the socket; on the client side a packet received while `Outages` says the link is not `Up` is discarded (it still counts as traffic); fills `bufs`, `metas` (`len`, `stride`), and `RecvInfo::new(custom_addr(from), Some(custom_addr(local)))`.
- Seeds: `Link` gets `seed ^ stream ^ dir_tag`, `Outages` gets `seed ^ stream ^ 0x6f75_7461_6765`; `dir_tag` is 1 for Up, 2 for Down.
- `Change`s are logged as `tracing::warn!(secs, "netsim: dead zone open")` and `tracing::warn!("netsim: dead zone closed")`.
- `build_endpoint`: when `netsim::active()?` is `Some(spec)`: `log_active(spec)` (`tracing::warn!(profile = %spec.profile, side = ?spec.side, seed = spec.seed, stream = spec.stream, "netsim active")`), then `builder.clear_ip_transports().clear_relay_transports().add_custom_transport(transport(spec, bind))` with `bind` = the first IPv4 `opts.bind_addrs` entry, else `127.0.0.1:0`; the IP `bind_addr` loop is skipped. No other path may remain.
- Daemon announce: when `netsim::bound_addr()` is `Some(a)`, print `listening on: {a}` instead of iterating `bound_sockets()`.
- Client dial (`main.rs:481`): when `netsim::active()` is `Some`, map each resolved IPv4 address `a` to `TransportAddr::Custom(custom_addr(a))`; an IPv6 address is an error naming netsim.

- [ ] **Step 1: Write the failing tests**

Unit test in `transport.rs`:
```rust
#[test] fn segmented_transmit_is_split() {
    let contents = [1u8; 3000];
    let t = Transmit { destination: "127.0.0.1:1".parse().unwrap(), ecn: None, contents: &contents, segment_size: Some(1200), src_ip: None };
    assert_eq!(segments(&t).map(<[u8]>::len).collect::<Vec<_>>(), vec![1200, 1200, 600]);
}
```
(`fn segments<'a>(t: &'a Transmit<'_>) -> impl Iterator<Item = &'a [u8]>` is the transport's splitter.)

Integration test `crates/irsync/tests/netsim.rs`, real binaries (`CARGO_BIN_EXE_irsync`, built with the test run's features), export and client key set up as `tests/listing_e2e.rs` does, daemon via `spawn_daemon_binary_env` with `IRSYNC_NETSIM=profile=test-blackout,side=daemon,seed=1,stream=0`, client with `IRSYNC_NETSIM=profile=test-blackout,side=client,seed=1,stream=1`, `RUST_LOG=info`, `--address <daemon listen> --timeout 2 --reconnect 1m`, a 4 MiB file (so the 256 KiB threshold is crossed):
```rust
#[test] fn a_dead_zone_forces_a_reconnect_and_the_session_resumes() {
    for direction in ["push", "pull"] {
        let (out, log, src, dst) = sync_through_blackout(direction);   // test-local helper
        assert!(out.status.success(), "{direction}: {log}");
        assert!(log.contains("netsim active"), "{log}");
        let open = log.find("netsim: dead zone open").expect("dead zone opened");
        assert!(log[open..].contains("connection lost; reconnecting"), "{log}");
        let first = session_of(&log[..open]);                              // id from the first "session started"
        let resumed = log[open..].lines().find(|l| l.contains("session started")).expect("redialled");
        assert!(resumed.contains("resumed=true") && resumed.contains(&first), "{resumed}");
        assert_eq!(std::fs::read(src.join("big")).unwrap(), std::fs::read(dst.join("big")).unwrap());
    }
}
#[test] fn without_the_variable_nothing_changes() {   // same binaries, no IRSYNC_NETSIM: plain loopback sync, no "netsim" in the log
    let (out, log) = plain_sync();
    assert!(out.status.success());
    assert!(!log.contains("netsim"));
}
```
(Strip ANSI codes from the log before matching, as `docs/STRESS_BUG_HUNTING.md` section 7 does with `sed 's/\x1b\[[0-9;]*m//g'`.)

The anti-goal "no path bypasses the fault layer", in `tests/netsim_endpoint.rs`:
```rust
#[tokio::test] async fn a_netsim_endpoint_has_no_ip_socket() {
    std::env::set_var("IRSYNC_NETSIM", "profile=test-blackout,side=client,seed=1,stream=1");
    let ep = irsync::net::endpoint::build_endpoint(EndpointOpts::isolated(SecretKey::generate(&mut rand_core::OsRng))).await.unwrap();
    assert!(ep.bound_sockets().is_empty(), "{:?}", ep.bound_sockets());
    assert!(irsync::net::netsim::bound_addr().is_some());
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p irsync --features netsim --test netsim` and `cargo test -p irsync --features netsim --lib netsim::transport` → FAIL (missing `transport`, `segments`, `spawn_daemon_binary_env`).

- [ ] **Step 3: Implement** the transport, `transport()`, `bound_addr()`, `log_active()`, and the three hooks as described.

- [ ] **Step 4: Run them to verify they pass**

Run: the two commands of Step 2 → PASS (the integration test takes about 10 s per direction: the 6 s dead zone plus the 2 s timeout). Then `cargo test --workspace` (no feature) → PASS, unchanged.

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/net crates/irsync/src/daemon.rs crates/irsync/src/main.rs crates/irsync/tests/common/mod.rs crates/irsync/tests/netsim.rs
git commit -m "feat(netsim): iroh custom transport that runs every packet through the fault model"
```

---

### Task 5: Hooks in the `stress` library

**Files:**
- Modify: `crates/stress/src/daemon.rs:297` (`Daemon::start_with_env`; `start` delegates with `&[]`)
- Modify: `crates/stress/src/client.rs` (`sync_args_with`, `run_sync_with_env`; the old functions delegate)

**Interfaces:**
- Produces:
  ```rust
  impl Daemon { pub fn start_with_env(irsync: &Path, files: &DaemonFiles, log: DaemonLog, log_level: &str,
                                      timeout: Duration, env: &[(OsString, OsString)]) -> Result<Daemon>; }
  /// `sync_args` with `reconnect` in place of "0".
  pub fn sync_args_with(d: Direction, layout: &Layout, endpoint_id: &str, port: u16, setup: &ClientSetup,
                        reconnect: &str, extra: &[OsString]) -> Vec<OsString>;
  pub fn run_sync_with_env(irsync: &Path, args: &[OsString], setup: &ClientSetup, log_level: &str,
                           log: &Path, env: &[(OsString, OsString)]) -> Result<SyncResult>;
  ```

- [ ] **Step 1: Write the failing tests** (unit tests beside the existing ones in `client.rs`)

```rust
#[test] fn sync_args_with_puts_the_reconnect_window_in_place_of_zero() {
    let t = tempfile::tempdir().unwrap();                       // set up as push_args_in_spec_order does
    let l = Layout::create(&t.path().join("r")).unwrap();
    let s = setup(&l.src_config, l.src_config.join("id_ed25519")).unwrap();
    let a = strings(&sync_args_with(Direction::Push, &l, "ID", 4321, &s, "5m", &[]));
    assert_eq!(&a[..5], ["-a", "-H", "--delete", "--reconnect", "5m"]);
    assert_eq!(sync_args(Direction::Push, &l, "ID", 4321, &s, &[]),
               sync_args_with(Direction::Push, &l, "ID", 4321, &s, "0", &[]));
}
```
and in `daemon.rs` a test that `start_with_env` passes an env var through (spawn `/bin/sh -c 'echo "endpoint id: x"; echo "listening on: 127.0.0.1:1"; echo "$NETSIM_PROBE" >&2; sleep 5'` as the "irsync" with `NETSIM_PROBE=seen`, then find `seen` in the daemon log).

- [ ] **Step 2: Run them to verify they fail** — `cargo test -p stress --lib` → FAIL to compile.
- [ ] **Step 3: Implement** the three functions; the old ones become one-line delegations.
- [ ] **Step 4: Run them to verify they pass** — `cargo build --workspace && cargo test -p stress` → PASS, including the untouched `tests/e2e.rs`.
- [ ] **Step 5: Commit**

```bash
git add crates/stress/src/daemon.rs crates/stress/src/client.rs
git commit -m "refactor(stress): env and reconnect hooks for a second rig; irsync-stress unchanged"
```

---

### Task 6: The `netstress` crate: command line and profiles

**Files:**
- Create: `crates/netstress/Cargo.toml` (bin `irsync-netstress`, lib; deps `stress` (path), clap, anyhow, serde, toml, rand, jiff — as `stress`)
- Create: `crates/netstress/src/main.rs`, `src/lib.rs`, `src/cli.rs`, `src/profile.rs`

**Interfaces:**
- Consumes: `stress::cli::Direction`.
- Produces:
  ```rust
  #[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
  pub enum Profile { #[value(name = "4g-vehicle")] FourGVehicle, #[value(name = "dsl-video")] DslVideo,
                     #[value(name = "test-blackout")] TestBlackout }
  pub struct Defaults { pub iterations: u64, pub steps: u64, pub max_total_size: &'static str,
                        pub max_file_size: &'static str, pub client_timeout_secs: u64, pub has_outages: bool }
  impl Profile { pub fn name(self) -> &'static str; pub fn defaults(self) -> Defaults; }
  pub struct Cli {   // as stress::cli::Cli, plus `profile`, with the sizes optional so the profile can fill them
      pub root: PathBuf, pub direction: Direction, pub profile: Profile,               // --profile, required
      pub iterations: Option<u64>, pub seed: Option<u64>, pub steps: Option<u64>,
      pub max_total_size: Option<String>, pub max_file_size: Option<String>, pub filegen_set: Vec<String>,
      pub log_level: String, pub keep_going: bool,
      pub irsync: Option<PathBuf>, pub filegen: Option<PathBuf>, pub fileset: Option<PathBuf>,
      pub client_args: Vec<OsString>,
  }
  impl Cli { pub fn to_stress(&self) -> stress::cli::Cli; }   // sizes resolved from the profile's defaults
  ```

Defaults: `4g-vehicle` 10 iterations, 100 steps, 256M, 32M, timeout 10, outages; `dsl-video` 10, 100, 32M, 4M, timeout 10, no outages; `test-blackout` 3, 50, 8M, 1M, timeout 2, outages.

- [ ] **Step 1: Write the failing tests** (`cli.rs`)

```rust
#[test] fn profile_defaults_fill_what_was_not_given() {
    let c = Cli::try_parse_from(["irsync-netstress", "--profile", "dsl-video", "--direction", "push", "/r"]).unwrap();
    let s = c.to_stress();
    assert_eq!((s.iterations, s.steps), (10, 100));
    assert_eq!((s.max_total_size.as_deref(), s.max_file_size.as_deref()), (Some("32M"), Some("4M")));
}
#[test] fn explicit_sizes_win() {
    let c = Cli::try_parse_from(["irsync-netstress", "--profile", "4g-vehicle", "--direction", "pull",
                                 "--iterations", "2", "--max-total-size", "1G", "/r"]).unwrap();
    let s = c.to_stress();
    assert_eq!(s.iterations, 2); assert_eq!(s.max_total_size.as_deref(), Some("1G"));
    assert_eq!(s.max_file_size.as_deref(), Some("32M"));
}
#[test] fn profile_names_match_irsync() {
    assert_eq!(Profile::FourGVehicle.name(), "4g-vehicle");
    assert_eq!(Profile::DslVideo.name(), "dsl-video");
    assert_eq!(Profile::TestBlackout.name(), "test-blackout");
}
```

- [ ] **Step 2: Run them to verify they fail** — `cargo test -p netstress` → FAIL (crate missing).
- [ ] **Step 3: Implement** the crate skeleton; `main.rs` parses and calls `netstress::run::run` (a stub returning `Ok(0)` until Task 8).
- [ ] **Step 4: Run them to verify they pass** — `cargo test -p netstress` → PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/netstress
git commit -m "feat(netstress): crate, command line and profile defaults"
```

---

### Task 7: Reading what the network did from the client log

**Files:**
- Create: `crates/netstress/src/netlog.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Default, PartialEq, Eq)]
  pub struct NetEvents {
      pub active: bool,               // "netsim active" seen
      pub dead_zones: u32,            // "netsim: dead zone open"
      pub resumed: u32,               // dead zones followed by "session started" with resumed=true and the first session's id
      pub stray_reconnects: u32,      // "connection lost; reconnecting" not between a dead zone's open and the next "session started"
  }
  pub fn read(log_text: &str) -> NetEvents;   // strips ANSI codes first
  impl NetEvents {
      /// None: fine. Some(reason): the iteration fails with this reason.
      pub fn verdict(&self, has_outages: bool) -> Option<String>;
  }
  ```

Verdict rules (from the spec's Verification): not `active` → "fault layer not active" (the caller turns this into exit 2, not 1); `stray_reconnects > 0` → fail ("reconnect without a dead zone"); `dead_zones > resumed` → fail ("dead zone did not end in a resumed session"); otherwise none. `dead_zones == 0` is not a failure here (counted by the run for exit 3).

- [ ] **Step 1: Write the failing tests** — a fixture of real-looking lines (`... WARN irsync::net::netsim: netsim active profile=4g-vehicle side=Client seed=7 stream=1`, `... INFO irsync::session::engine: session started session=0011aabb resumed=false ...`, `... WARN ...: netsim: dead zone open secs=31`, two `connection lost; reconnecting` lines, `... netsim: dead zone closed`, `session started session=0011aabb resumed=true`), and:

```rust
#[test] fn a_dead_zone_that_resumed() { assert_eq!(read(FIXTURE), NetEvents { active: true, dead_zones: 1, resumed: 1, stray_reconnects: 0 }); }
#[test] fn parses_a_coloured_log() { assert_eq!(read(&colour(FIXTURE)), read(FIXTURE)); }   // colour(): wraps level and target in \x1b[..m codes as tracing does
#[test] fn a_new_session_after_a_dead_zone_is_not_a_resume() {
    let e = read(&FIXTURE.replace("resumed=true", "resumed=false"));
    assert_eq!(e.resumed, 0); assert!(e.verdict(true).is_some());
}
#[test] fn a_reconnect_outside_a_dead_zone_is_stray() {
    let e = read(&format!("{FIXTURE}\n... WARN irsync::session::reconnect: connection lost; reconnecting attempt=1"));
    assert_eq!(e.stray_reconnects, 1); assert!(e.verdict(true).is_some());
}
#[test] fn inactive_is_reported() { assert!(read("session started").verdict(false).unwrap().contains("not active")); }
```

- [ ] **Step 2: Run them to verify they fail** — `cargo test -p netstress netlog` → FAIL.
- [ ] **Step 3: Implement** `read` as a single pass over the lines with a small state machine (before / inside a dead zone until the next `session started`).
- [ ] **Step 4: Run them to verify they pass** — same command → PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/netstress/src/netlog.rs crates/netstress/src/lib.rs
git commit -m "feat(netstress): read dead zones, resumes and stray reconnects from the client log"
```

---

### Task 8: The run loop, report and exit codes

**Files:**
- Create: `crates/netstress/src/run.rs`, `crates/netstress/src/report.rs`, `crates/netstress/src/run_file.rs`
- Create: `crates/netstress/tests/e2e.rs`

**Interfaces:**
- Consumes: `stress::{tools::Tools, layout::Layout, filegen, fileset, verify, signals, daemon::{self, Daemon, DaemonConfig, DaemonLog}, client, report as base}`; Task 5's hooks; Task 6's `Cli`, `Profile`; Task 7's `netlog`.
- Produces: `pub fn run(cli: &Cli) -> anyhow::Result<i32>`; `run.toml` = irsync-stress's fields (`#[serde(flatten)] stress::run_file::RunFile`) plus `[network] profile, client_timeout_secs, reconnect`.

Sequence: as `stress::run::run` (tools, layout, run.toml, daemon, header, ping, fill, iterations, footer), with these differences:
- Before anything is created: `irsync --version` must end with ` [netsim]`, else exit 2 with `"<path> is not a netsim build; build with: cargo build --release --workspace --features irsync/netsim --target-dir target/netsim"`.
- Daemon: `start_with_env` with `IRSYNC_NETSIM=profile=<p>,side=daemon,seed=<seed>,stream=0`; after startup the daemon log must contain `netsim active` within the startup timeout, else exit 2.
- Each client run (ping = stream 0, iteration i = stream i): `run_sync_with_env` with `IRSYNC_NETSIM=profile=<p>,side=client,seed=<seed>,stream=<n>`; args from `sync_args_with(..., "5m", extra)` where `extra` = `["--timeout", "<client_timeout_secs>"]` then the passthrough args.
- After each iteration's sync: `daemon.check_alive()` (death → exit 2, as in stress); `netlog::read` the client log; a verdict of "not active" → exit 2; any other verdict fails the iteration (with the reason printed after the line).
- After the last iteration: `find` any `.irsync.*.part` in either data tree → the run fails (exit 1) naming them.
- Exit: 2 and 130 as stress; else 1 if any iteration failed or a `.part` was left; else 3 if the profile has outages and no iteration had `dead_zones > 0` ("faults did not bite"); else 0.
- Output: `stress::report::header(...)` plus a line `  network:   <profile>`; each iteration line is the stress line plus `  outages <dead_zones>`; the footer is the stress footer plus `outages=<total>`.

- [ ] **Step 1: Write the failing tests** (`tests/e2e.rs`). The test builds the fault binaries once per test binary (a `OnceLock`): `env!("CARGO") build --workspace --features irsync/netsim --target-dir <workspace>/target/netsim` (debug), and runs `CARGO_BIN_EXE_irsync-netstress` with `--irsync/--filegen/--fileset` pointing into `target/netsim/debug/`.

```rust
#[test] fn test_blackout_push_survives_its_dead_zones_over_three_iterations() {
    let (o, root) = run_rig(&["--profile", "test-blackout", "--direction", "push", "--iterations", "3"]);
    assert_eq!(o.status.code(), Some(0), "{}", show(&o));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("network:   test-blackout"));
    let iters: Vec<&str> = out.lines().filter(|l| l.starts_with("iter ")).collect();
    assert_eq!(iters.len(), 3, "{out}");
    // The fill moves megabytes, so the first sync always crosses the 256 KiB
    // threshold; a later one may move too little and is then noted, not failed.
    assert!(iters[0].contains("outages 1"), "{out}");
    assert!(root.path().join("run.toml").exists());
}
#[test] fn test_blackout_pull_passes() { /* same, --direction pull, --iterations 1 */ }
#[test] fn too_little_traffic_means_faults_did_not_bite() {
    let (o, _) = run_rig(&["--profile", "test-blackout", "--direction", "push", "--iterations", "1",
                           "--max-total-size", "64K", "--max-file-size", "16K", "--steps", "5"]);
    assert_eq!(o.status.code(), Some(3), "{}", show(&o));
}
#[test] fn dsl_video_pull_passes_without_reconnects() {
    let (o, _) = run_rig(&["--profile", "dsl-video", "--direction", "pull", "--iterations", "1",
                           "--max-total-size", "2M", "--max-file-size", "512K", "--steps", "20"]);
    assert_eq!(o.status.code(), Some(0), "{}", show(&o));
}
#[test] fn a_normal_irsync_is_refused() {   // the workspace's own target/debug/irsync, built without netsim
    let o = run_rig_with_irsync(normal_irsync(), &["--profile", "dsl-video", "--direction", "push"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("--features irsync/netsim"));
}
```

- [ ] **Step 2: Run them to verify they fail** — `cargo test -p netstress --test e2e` → FAIL (`run` is the Task 6 stub).
- [ ] **Step 3: Implement** `run.rs`, `report.rs`, `run_file.rs` as described.
- [ ] **Step 4: Run them to verify they pass** — `cargo test -p netstress` → PASS (about a minute: the dead zones are real time). Then the full Global Constraints list.
- [ ] **Step 5: Commit**

```bash
git add crates/netstress
git commit -m "feat(netstress): the run loop, network report and exit code 3"
```

---

### Task 9: Sweep script, a real run of each profile, and documentation

**Files:**
- Create: `crates/netstress/tools/sweep.sh`
- Create: `crates/netstress/README.md`
- Create: `docs/NETSTRESS_DECISIONS.md`
- Modify: `DEVELOPMENT.md` (the rig beside irsync-stress; the fault build command), `docs/STRESS_BUG_HUNTING.md` (section 6 item 3 points at the second project; a pointer to the rig for network faults), `docs/superpowers/specs/2026-11-06-network-faults-design.md` (status line: implemented, naming this plan)

**Interfaces:**
- Consumes: the `irsync-netstress` binary and the fault build from Task 8.

`sweep.sh`, modelled on `crates/stress/tools/breadth.sh` (same header style, bash 3.2 compatible, `set -u`, roots of passing runs deleted): environment `BIN` [`target/netsim/release`], `OUT` [`$HOME/work/irsync-netstress-<date>`], `SEEDS` [`1 10`], `JOBS` [`4`], `PROFILES` [`4g-vehicle dsl-video`]; runs every seed × profile × direction; refuses to start unless `$BIN/irsync --version` ends with ` [netsim]`; writes `summary.txt`; exits non-zero if any run's exit was not 0.

- [ ] **Step 1: Build the fault binaries and run one real run per profile and direction**

Run: `cargo build --release --workspace --features irsync/netsim --target-dir target/netsim`, then for each of `4g-vehicle` and `dsl-video`, push and pull: `target/netsim/release/irsync-netstress --profile <p> --direction <d> --seed 1 ~/work/netstress-<p>-<d>`.
Expected: each finishes within about 10 minutes; `4g-vehicle` iterations show `outages 1`. Record wall times, outcomes and anything irsync logged in `docs/NETSTRESS_DECISIONS.md`. A failure here is a finding, not a plan failure: record it with its root and stop for Mike.

- [ ] **Step 2: Write `sweep.sh`** and check it: `SEEDS="1 1" PROFILES=dsl-video crates/netstress/tools/sweep.sh` → two runs, `summary.txt` with two lines.

- [ ] **Step 3: Write the documentation** listed above. The README covers: the command, the three profiles (test-blackout marked as for tests), the fault build, exit codes 0/1/2/3/130, the `outages` count, `sweep.sh`. The decisions file records Claude's calls from the spec and this plan (profile numbers, `test-blackout`, the dead-zone threshold, `--reconnect 5m`, stream numbering) and Step 1's measurements.

- [ ] **Step 4: Verify** — the full Global Constraints list, plus `bash -n crates/netstress/tools/sweep.sh`.

- [ ] **Step 5: Commit**

```bash
git add crates/netstress/tools/sweep.sh crates/netstress/README.md docs/NETSTRESS_DECISIONS.md DEVELOPMENT.md docs/STRESS_BUG_HUNTING.md docs/superpowers/specs/2026-11-06-network-faults-design.md
git commit -m "docs(netstress): sweep script, README, decisions and the first real runs"
```
