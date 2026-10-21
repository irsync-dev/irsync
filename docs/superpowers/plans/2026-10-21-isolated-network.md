# Isolated Network Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a daemon bind chosen sockets and report them, and let a client dial a daemon by id plus direct socket addresses with no relay, no address lookup and no port mapper.

**Architecture:** A small `HostPort` type (`ip:port`, `[v6]:port`, `host:port`, resolved with the system resolver) is shared by both config parsers and both CLIs. `EndpointOpts` grows a list of bind addresses and a port-mapper switch, plus an `isolated` constructor the client uses whenever addresses are given. The daemon prints its bound sockets after its id; the client builds the dial target from the id and the resolved addresses. Session code is untouched: it already takes an `EndpointAddr`.

**Tech Stack:** Rust 2021, iroh 1.3 (`presets::Minimal`, `RelayMode::Disabled`, `clear_address_lookup`, `clear_ip_transports` + `bind_addr`, `bound_sockets`, `portmapper_config`, `EndpointAddr::from_parts`), clap 4 derive, serde + toml, tokio.

**Spec:** `docs/superpowers/specs/2026-10-21-isolated-network-design.md`

## Global Constraints

- iroh stays at the workspace version (1.3); no new dependencies.
- Workspace CI must pass on Linux and macOS: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Existing behaviour is unchanged when none of the new settings are used: no `listen`, `portmap = true`, no `Address`, no `--address`, no `--listen`.
- At most one IPv4 and one IPv6 `listen` entry (iroh rejects a second default-route socket per family).
- A client given at least one address uses the network for nothing but dialling those addresses: relay disabled, n0 DNS, DHT, mDNS and port mapper off.
- The daemon's stdout startup lines are exactly `endpoint id: <id>` followed by one `listening on: <socketaddr>` per bound socket; all logging stays on stderr.
- A direct address never replaces the EndpointId; the dial target is always id plus addresses.
- Commit messages follow the repo's conventional style (`feat(net): …`, `docs: …`) and end with the Co-Authored-By trailer in use on this branch.

## Review Focus

1. `--address localhost:PORT` against a daemon bound only on `127.0.0.1`: the resolver yields `::1` and `127.0.0.1`, both go into the dial target, and the sync still succeeds. Pinned by the end-to-end test in Task 6, which dials `localhost:<port>`.
2. `Address` with no port, an out-of-range port, or an unbracketed IPv6 literal must fail at config parse with the line number, not at dial time. Pinned in Task 4.
3. Two `listen` entries of the same family (`"0.0.0.0:0"`, `"127.0.0.1:0"`) must fail at config parse, not with an iroh bind error at startup. Pinned in Task 3.
4. A `Host` block with both `Address` and `Relay` (or a global `Relay` above it) must parse and resolve without error; only `--address` with `--relay` on the command line is rejected. Pinned in Tasks 4 and 5.
5. A hostname in `--address` that does not resolve must produce a clear error before any endpoint is built, naming the address. Pinned in Task 5.

---

### Task 1: `HostPort` address type

**Files:**
- Create: `crates/irsync/src/net/addr.rs`
- Modify: `crates/irsync/src/net/mod.rs` (add `pub mod addr;`)
- Test: `crates/irsync/tests/addr.rs`

**Interfaces:**
- Produces:
  ```rust
  /// `ip:port`, `[v6]:port` or `host:port`.
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum HostPort { Ip(SocketAddr), Name { host: String, port: u16 } }
  #[derive(Debug, thiserror::Error)]
  #[error("invalid address {0:?}: expected ip:port, [ipv6]:port or host:port")]
  pub struct AddrError(pub String);
  impl FromStr for HostPort { type Err = AddrError; }
  impl Display for HostPort;            // round-trips: `[::1]:9`, `localhost:9`
  impl<'de> Deserialize<'de> for HostPort; // from a string, via FromStr
  impl HostPort {
      pub fn port(&self) -> u16;
      /// Every address the system resolver yields, in resolver order; `Ip` yields itself.
      pub async fn resolve(&self) -> std::io::Result<Vec<SocketAddr>>;
  }
  ```

- [ ] **Step 1: Write the failing tests** in `crates/irsync/tests/addr.rs`

```rust
use irsync::net::addr::HostPort;
use std::net::SocketAddr;

#[test]
fn parses_ipv4_ipv6_and_names() {
    assert_eq!("127.0.0.1:9".parse::<HostPort>().unwrap(), HostPort::Ip("127.0.0.1:9".parse().unwrap()));
    assert_eq!("[::1]:9".parse::<HostPort>().unwrap(), HostPort::Ip("[::1]:9".parse().unwrap()));
    assert_eq!("nas.local:9".parse::<HostPort>().unwrap(), HostPort::Name { host: "nas.local".into(), port: 9 });
    assert_eq!("127.0.0.1:0".parse::<HostPort>().unwrap().port(), 0);
}

#[test]
fn rejects_missing_port_bad_port_and_bare_ipv6() {
    for bad in ["127.0.0.1", "host", "host:", ":9", "host:99999", "::1:9", "[::1]", ""] {
        assert!(bad.parse::<HostPort>().is_err(), "{bad:?} parsed");
    }
}

#[test]
fn display_round_trips() {
    for s in ["127.0.0.1:9", "[::1]:9", "nas.local:9"] {
        assert_eq!(s.parse::<HostPort>().unwrap().to_string(), s);
    }
}

#[test]
fn deserializes_from_toml_string() {
    #[derive(serde::Deserialize)]
    struct T { a: Vec<HostPort> }
    let t: T = toml::from_str("a = [\"127.0.0.1:1\", \"[::1]:2\"]").unwrap();
    assert_eq!(t.a.len(), 2);
    assert!(toml::from_str::<T>("a = [\"nope\"]").is_err());
}

#[tokio::test]
async fn resolves_ip_to_itself_and_localhost_to_loopback() {
    let ip: HostPort = "127.0.0.1:7".parse().unwrap();
    assert_eq!(ip.resolve().await.unwrap(), vec!["127.0.0.1:7".parse::<SocketAddr>().unwrap()]);
    let lo: HostPort = "localhost:7".parse().unwrap();
    let v = lo.resolve().await.unwrap();
    assert!(!v.is_empty() && v.iter().all(|a| a.ip().is_loopback() && a.port() == 7), "{v:?}");
    assert!("no-such-host.invalid:7".parse::<HostPort>().unwrap().resolve().await.is_err());
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p irsync --test addr`
Expected: compile error, `irsync::net::addr` not found.

- [ ] **Step 3: Implement `crates/irsync/src/net/addr.rs`**

Parse: try `SocketAddr::from_str` first; otherwise split on the last `:`, require a non-empty host that contains no `:` or `[`/`]`, and a `u16` port. Resolve `Name` with `tokio::net::lookup_host((host.as_str(), port))`. `Display` prints `Ip` with `SocketAddr`'s own formatting (which brackets IPv6). Add `pub mod addr;` to `net/mod.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p irsync --test addr`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/net/addr.rs crates/irsync/src/net/mod.rs crates/irsync/tests/addr.rs
git commit -m "feat(net): HostPort address type for direct addresses"
```

---

### Task 2: Endpoint options: several bind sockets, port-mapper switch, isolated preset

**Files:**
- Modify: `crates/irsync/src/net/endpoint.rs` (`EndpointOpts`, `build_endpoint`)
- Modify: `crates/irsync/src/daemon.rs:356-364`, `crates/irsync/src/main.rs:313-321` (field rename only)
- Modify: `crates/irsync/tests/common/mod.rs:42-51` (`offline_opts`)
- Test: `crates/irsync/tests/net.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct EndpointOpts {
      pub secret: SecretKey,
      pub relay: RelayChoice,
      pub dht: bool, pub dns: bool, pub mdns: bool, pub publish: bool,
      /// Bind exactly these instead of iroh's unspecified IPv4/IPv6 defaults. Empty keeps the defaults.
      pub bind_addrs: Vec<SocketAddr>,
      /// iroh's UPnP/PCP/NAT-PMP port mapper.
      pub portmap: bool,
  }
  impl EndpointOpts {
      /// No relay, no lookups, no publishing, no port mapper, default binds.
      pub fn isolated(secret: SecretKey) -> EndpointOpts;
  }
  ```
  `build_endpoint` unchanged in signature.

- [ ] **Step 1: Write the failing tests** (append to `crates/irsync/tests/net.rs`)

```rust
use irsync::net::EndpointOpts;
use iroh::SecretKey;

#[tokio::test(flavor = "multi_thread")]
async fn binds_each_requested_socket_and_reports_real_ports() {
    let mut opts = EndpointOpts::isolated(SecretKey::generate());
    opts.bind_addrs = vec!["127.0.0.1:0".parse().unwrap(), "[::1]:0".parse().unwrap()];
    let ep = irsync::net::build_endpoint(opts).await.unwrap();
    let bound = ep.bound_sockets();
    assert_eq!(bound.len(), 2, "{bound:?}");
    assert!(bound.iter().all(|a| a.ip().is_loopback() && a.port() != 0), "{bound:?}");
    assert!(bound.iter().any(|a| a.is_ipv4()) && bound.iter().any(|a| a.is_ipv6()));
    ep.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn isolated_opts_have_no_relay_and_no_lookup() {
    let opts = EndpointOpts::isolated(SecretKey::generate());
    assert!(!opts.dht && !opts.dns && !opts.mdns && !opts.publish && !opts.portmap);
    assert_eq!(opts.relay, irsync::net::RelayChoice::Disabled);
    let ep = irsync::net::build_endpoint(opts).await.unwrap();
    assert!(ep.relay_map().is_empty());
    assert!(ep.address_lookup().unwrap().is_empty());
    ep.close().await;
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p irsync --test net`
Expected: compile error, no `isolated` / `bind_addrs`.

- [ ] **Step 3: Implement**

In `endpoint.rs`: replace `bind_addr: Option<SocketAddr>` with `bind_addrs: Vec<SocketAddr>`, add `portmap: bool`, add `EndpointOpts::isolated`. In `build_endpoint`: when `bind_addrs` is non-empty call `clear_ip_transports()` once then `bind_addr(a)?` per entry; when `!opts.portmap` call `.portmapper_config(iroh::endpoint::PortmapperConfig::Disabled)` (check the re-export path; it is `iroh::portmapper::PortmapperConfig` if not re-exported under `endpoint`). Update the two call sites in `daemon.rs` and `main.rs` to `bind_addrs: Vec::new(), portmap: true` (behaviour unchanged for now). Update `offline_opts()` in the test harness to `bind_addrs: vec!["127.0.0.1:0".parse().unwrap()], portmap: false`.

- [ ] **Step 4: Run the whole crate's tests**

Run: `cargo test -p irsync`
Expected: all pass, including the two new ones.

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/net/endpoint.rs crates/irsync/src/daemon.rs crates/irsync/src/main.rs crates/irsync/tests/common/mod.rs crates/irsync/tests/net.rs
git commit -m "feat(net): bind address list, port-mapper switch and isolated endpoint options"
```

---

### Task 3: Daemon `listen`, `portmap`, `--listen`, and the `listening on:` lines

**Files:**
- Modify: `crates/irsync/src/config/daemon.rs` (`DaemonConfig`)
- Modify: `crates/irsync/src/cli/mod.rs:116-127` (`DaemonArgs`)
- Modify: `crates/irsync/src/daemon.rs:339-367` (`run_from_config`)
- Modify: `crates/irsync/src/main.rs:479-497` (`run_daemon`: apply `--listen`)
- Modify: `README.md:45-60` (daemon config example), `README.md:171` (subcommand line)
- Test: `crates/irsync/tests/config.rs`, `crates/irsync/tests/cli.rs`

**Interfaces:**
- Consumes: `HostPort` (Task 1), `EndpointOpts.bind_addrs` / `.portmap` (Task 2).
- Produces:
  ```rust
  // config/daemon.rs
  pub struct DaemonConfig { /* existing */ pub listen: Vec<HostPort>, pub portmap: bool, /* … */ }
  // Deserialised with a `deserialize_with` that rejects two numeric entries of one family
  // with the message "at most one IPv4 and one IPv6 listen address" (serde gives the line).
  /// Resolves `listen` to the sockets to bind: each entry contributes its first IPv4 and
  /// first IPv6 address; two entries landing in one family is an error naming both.
  pub async fn listen_sockets(listen: &[HostPort]) -> anyhow::Result<Vec<SocketAddr>>;
  // cli/mod.rs
  pub struct DaemonArgs { /* existing */ #[arg(long, value_name = "ADDR")] pub listen: Vec<HostPort> }
  ```

- [ ] **Step 1: Write the failing tests**

Append to `crates/irsync/tests/config.rs`:

```rust
use irsync::net::addr::HostPort;

#[test]
fn daemon_listen_and_portmap_parse() {
    let cfg = DaemonConfig::parse("listen = [\"127.0.0.1:0\", \"[::1]:4000\"]\nportmap = false\n").unwrap();
    assert_eq!(cfg.listen, vec!["127.0.0.1:0".parse::<HostPort>().unwrap(), "[::1]:4000".parse().unwrap()]);
    assert!(!cfg.portmap);
    let d = DaemonConfig::parse("").unwrap();
    assert!(d.listen.is_empty() && d.portmap);
}

#[test]
fn daemon_listen_rejects_two_of_one_family_with_line() {
    let err = DaemonConfig::parse("\nlisten = [\"0.0.0.0:0\", \"127.0.0.1:0\"]\n").unwrap_err().to_string();
    assert!(err.contains("at most one IPv4 and one IPv6"), "{err}");
    assert!(err.contains("line 2"), "{err}");
    assert!(DaemonConfig::parse("listen = [\"nope\"]\n").is_err());
}

#[tokio::test]
async fn listen_sockets_takes_first_per_family_and_rejects_collisions() {
    use irsync::config::daemon::listen_sockets;
    let v = listen_sockets(&["localhost:5".parse().unwrap()]).await.unwrap();
    assert!(v.iter().all(|a| a.ip().is_loopback() && a.port() == 5));
    assert!(v.len() <= 2 && v.iter().filter(|a| a.is_ipv4()).count() <= 1);
    let err = listen_sockets(&["localhost:5".parse().unwrap(), "127.0.0.1:5".parse().unwrap()]).await.unwrap_err();
    assert!(err.to_string().contains("127.0.0.1:5"), "{err}");
    assert!(listen_sockets(&["no-such-host.invalid:5".parse().unwrap()]).await.is_err());
}
```

Append to `crates/irsync/tests/cli.rs`:

```rust
#[test]
fn daemon_listen_flag_repeats() {
    match parse(&["daemon", "--listen", "127.0.0.1:0", "--listen", "[::1]:0"]).cmd {
        Some(Command::Daemon(d)) => assert_eq!(d.listen.len(), 2),
        c => panic!("{c:?}"),
    }
    assert!(Args::try_parse_from(["irsync", "daemon", "--listen", "nope"]).is_err());
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p irsync --test config --test cli`
Expected: compile errors for `listen`, `portmap`, `listen_sockets`.

- [ ] **Step 3: Implement**

`DaemonConfig`: add `listen` (default empty, `#[serde(deserialize_with = "de_listen")]`) and `portmap` (default `true`), in both the struct and its manual `Default` impl; `de_listen` deserialises `Vec<HostPort>` then checks numeric entries per family and returns `serde::de::Error::custom("at most one IPv4 and one IPv6 listen address")`. `listen_sockets`: resolve each entry with `HostPort::resolve`, keep the first v4 and first v6 it yields, and if a family is already taken error with `anyhow!("listen: {this} and {previous} are both {family}")`-style wording naming both entries. `DaemonArgs.listen` with `value_parser = clap::value_parser!(HostPort)` (works through `FromStr` since `AddrError: std::error::Error`). In `run_daemon` (main.rs), after loading the config: `if !d.listen.is_empty() { cfg.listen = d.listen.clone(); }`. In `run_from_config`: `bind_addrs: listen_sockets(&cfg.listen).await?`, `portmap: cfg.portmap`; after the existing `println!("endpoint id: …")` add one `println!("listening on: {addr}")` per `endpoint.bound_sockets()`, before `Daemon::new`. README: add `listen = ["0.0.0.0:0", "[::]:0"]  # optional: pin sockets; port 0 = daemon picks` and `portmap = true  # UPnP/PCP/NAT-PMP` lines to the daemon config example, and `[--listen ADDR]...` to the subcommand line.

- [ ] **Step 4: Run tests**

Run: `cargo test -p irsync --test config --test cli --test gc`
Expected: all pass (`daemon_show_id_prints_only_the_id` still prints one line: `--show-id` returns before binding).

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/config/daemon.rs crates/irsync/src/cli/mod.rs crates/irsync/src/daemon.rs crates/irsync/src/main.rs crates/irsync/tests/config.rs crates/irsync/tests/cli.rs README.md
git commit -m "feat(daemon): listen sockets, portmap switch, and bound-address report"
```

---

### Task 4: Client config `Address` and `--address`

**Files:**
- Modify: `crates/irsync/src/config/client.rs` (`HostConfig`, `set_option`, `fill_from`)
- Modify: `crates/irsync/src/cli/mod.rs` (`SyncArgs`, `SyncOptions`, `From` impl around lines 310-376)
- Modify: `README.md:93-110` (client config example), `README.md:160-176` (flag table)
- Test: `crates/irsync/tests/config.rs`, `crates/irsync/tests/cli.rs`

**Interfaces:**
- Consumes: `HostPort` (Task 1).
- Produces:
  ```rust
  // config/client.rs
  pub struct HostConfig { /* existing */ pub addresses: Vec<HostPort> }   // merged by extend, not first-wins
  // cli/mod.rs
  pub struct SyncArgs { /* … */ #[arg(long, value_name = "ADDR", conflicts_with = "relay")] pub address: Vec<HostPort> }
  pub struct SyncOptions { /* … */ pub addresses: Vec<HostPort> }
  ```

- [ ] **Step 1: Write the failing tests**

Append to `crates/irsync/tests/config.rs`:

```rust
#[test]
fn client_address_lines_accumulate_and_merge_across_blocks() {
    let text = "Host *\n    Address 10.0.0.1:1\nHost nas\n    Address 127.0.0.1:2\n    Address [::1]:2\n    Relay https://r.example\n";
    let cfg = ClientConfig::parse(text).unwrap();
    let nas = cfg.resolve("nas");
    assert_eq!(nas.addresses, vec![
        "10.0.0.1:1".parse::<HostPort>().unwrap(),
        "127.0.0.1:2".parse().unwrap(),
        "[::1]:2".parse().unwrap(),
    ]);
    assert!(nas.relay.is_some(), "Address next to Relay is not a parse error");
    assert!(cfg.resolve("other").addresses.len() == 1);
}

#[test]
fn client_address_bad_values_report_line() {
    for (text, line) in [("Host a\nAddress 127.0.0.1\n", 2), ("Address host:99999\n", 1), ("\nAddress ::1:9\n", 2)] {
        match ClientConfig::parse(text) {
            Err(ConfigError::BadValue { line: l, keyword, .. }) => { assert_eq!(l, line); assert_eq!(keyword, "Address"); }
            other => panic!("{text:?}: {other:?}"),
        }
    }
}
```

Append to `crates/irsync/tests/cli.rs`:

```rust
#[test]
fn address_flag_repeats_and_conflicts_with_relay() {
    let o = opts(&["--address", "127.0.0.1:9", "--address", "localhost:9", "a", "h:m"]);
    assert_eq!(o.addresses.len(), 2);
    assert!(Args::try_parse_from(["irsync", "--address", "127.0.0.1:9", "--relay", "https://r", "a", "h:m"]).is_err());
    assert!(Args::try_parse_from(["irsync", "--address", "127.0.0.1", "a", "h:m"]).is_err());
    assert!(opts(&["--address", "127.0.0.1:9", "--no-dht", "a", "h:m"]).no_dht, "redundant --no-dht is accepted");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p irsync --test config --test cli`
Expected: compile errors for `addresses` / `--address`.

- [ ] **Step 3: Implement**

`HostConfig.addresses: Vec<HostPort>`; in `fill_from` extend it with `other.addresses` (keep the `fill!` macro for the rest); in `set_option` handle `"address"` by parsing and pushing (not `first!`), mapping `AddrError` to its `to_string()` so `ConfigError::BadValue` carries it. `SyncArgs.address` as above; `SyncOptions.addresses` filled from `a.address.clone()`. README: add `Address 192.168.1.10:4000   # optional, repeatable: dial directly, no relay or lookup` to the `Host home` example and a table row `--address ADDR` / `dial this ip:port or host:port directly (repeatable); turns relay, DNS, DHT, mDNS and the port mapper off`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p irsync --test config --test cli`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/config/client.rs crates/irsync/src/cli/mod.rs crates/irsync/tests/config.rs crates/irsync/tests/cli.rs README.md
git commit -m "feat(cli): Address config option and --address flag"
```

---

### Task 5: Client dials directly when addresses are given

**Files:**
- Modify: `crates/irsync/src/main.rs:296-321` (`sync`: endpoint options) and `:445-458` (dial target)

**Interfaces:**
- Consumes: `SyncOptions.addresses`, `HostConfig.addresses` (Task 4); `EndpointOpts::isolated` (Task 2); `HostPort::resolve` (Task 1); `iroh::EndpointAddr::from_parts`, `iroh::TransportAddr::Ip`.
- Produces: nothing new for later tasks; behaviour only.

- [ ] **Step 1: Implement the isolated path in `sync`**

Right after `let hc = cfg.resolve(host);`: `let addresses = if opts.addresses.is_empty() { hc.addresses.clone() } else { opts.addresses.clone() };`. Resolve every entry with `HostPort::resolve`, collecting into `Vec<SocketAddr>`; a resolver error becomes `anyhow!("resolving address {entry}: {e}")` before any endpoint is built (Review Focus 5). Then:

- if `addresses` is non-empty: `EndpointOpts::isolated(secret)`; `tracing::info!(addresses = ?resolved, "isolated mode: dialling directly, relay and lookups off")`; if `hc.relay`, `hc.dht` or `hc.mdns` is set, `tracing::debug!("Address given: ignoring Relay/Dht/Mdns from the client config")`; the dial target becomes `EndpointAddr::from_parts(target, resolved.iter().map(|a| TransportAddr::Ip(*a)))` (pass it to `run_with_reconnect` in place of `EndpointAddr::new(target)`, threading the resolved list into `run_session` or whatever helper builds the target around line 457).
- else: the existing options, with `bind_addrs: Vec::new(), portmap: true`.

- [ ] **Step 2: Build and run clippy**

Run: `cargo clippy -p irsync --all-targets -- -D warnings && cargo test -p irsync`
Expected: clean; all existing tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/irsync/src/main.rs
git commit -m "feat(cli): dial the daemon directly, with relay and lookups off, when addresses are given"
```

---

### Task 6: End-to-end test through the real binaries, and the docs recipe

**Files:**
- Create: `crates/irsync/tests/isolated_e2e.rs`
- Modify: `docs/daemon.md:133-152` ("Relays, DHT and discovery")

**Interfaces:**
- Consumes: `irsync daemon --config … --listen 127.0.0.1:0` stdout lines (Task 3); `irsync --address … --identity … -F …` (Tasks 4 and 5); `irsync keygen --out PATH` (prints the public key line, then the id).

- [ ] **Step 1: Write the test** in `crates/irsync/tests/isolated_e2e.rs`

One `#[test]` (plain, not tokio) `client_and_daemon_sync_over_localhost_with_no_relay_or_lookup`:

1. `tempdir`; `src/` with `a.txt` (`b"hello"`), `sub/b.bin` (64 KiB of deterministic bytes), an empty `dst/`.
2. Run `CARGO_BIN_EXE_irsync keygen --out <tmp>/client_key`; take stdout line 1 as the public key line.
3. Write `<tmp>/authorized_keys` as `modules="m:rw" <public key line>\n`.
4. Write `<tmp>/d.toml`:
   ```toml
   identity = "<tmp>/daemon_key"
   authorized_keys = "<tmp>/authorized_keys"
   state_dir = "<tmp>/state"
   relay_mode = "disabled"
   portmap = false
   [discovery]
   dht = false
   dns = false
   mdns = false
   [modules.m]
   path = "<tmp>/dst"
   ```
5. Spawn `irsync daemon --config <tmp>/d.toml --listen 127.0.0.1:0` with stdout piped, stdin null; hold the child in a guard struct whose `Drop` calls `kill()` and `wait()`. A thread reads stdout lines into an `mpsc` channel. With `recv_timeout(Duration::from_secs(15))`, collect until a line starting `listening on: ` arrives; assert an earlier line started `endpoint id: ` and parse both (the id with `irsync::identity::parse_endpoint_id`, the address as `SocketAddr`). Assert the address is `127.0.0.1` with a non-zero port.
6. Run `irsync -a --address localhost:<port> --identity <tmp>/client_key -F <tmp>/empty_config <tmp>/src/ <id>:m/` with `HOME=<tmp>/home` and `XDG_STATE_HOME=<tmp>/xdg` (both created), `IRSYNC_PASSPHRASE` removed, stdin null. Assert exit status success, printing stderr on failure.
7. Assert `dst/a.txt` and `dst/sub/b.bin` equal the source bytes.
8. Run the client once more with the same arguments; assert success (a no-op second sync through the same addresses).

- [ ] **Step 2: Run it**

Run: `cargo test -p irsync --test isolated_e2e -- --nocapture`
Expected: PASS within a few seconds. If it fails on step 5, the daemon's stderr (inherited) shows why.

- [ ] **Step 3: Update `docs/daemon.md`**

In "Relays, DHT and discovery", add a subsection "Isolated operation (localhost, LAN, no internet)" with the four-line daemon recipe from the spec plus `listen`, the exact two stdout lines a parent process should read, and the client side: `Address` in the `Host` block or `--address`, and the sentence that giving an address turns off relay, n0 DNS, DHT, mDNS and the port mapper on the client. Note that `listen` alone pins sockets and does not isolate the daemon.

- [ ] **Step 4: Full CI locally**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all clean and green.

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/tests/isolated_e2e.rs docs/daemon.md
git commit -m "test(net): end-to-end isolated sync through the binaries; document the recipe"
```
