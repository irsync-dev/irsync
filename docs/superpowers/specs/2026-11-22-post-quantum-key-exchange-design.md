# Post-quantum key exchange for irsync

Date: 2026-11-22. Status: approved by the maintainer. Step 1 is done
(issue 240), and the design of step 2 was approved on 2026-11-23 with the
setting named `pq_key_exchange` (issue 241).

## Why

Someone who records an irsync connection today could decrypt it later, once
a large enough quantum computer exists. The key exchange irsync uses now
(X25519) does not hold against that. The threat model promises that nobody
between the client and the daemon can read the data, and until now said
nothing about this case.

## Decision

Option 3 of issue 219: **prefer the hybrid key exchange, always on, plus a
"require" setting.**

- irsync offers `X25519MLKEM768` first. This is a hybrid: it stays safe as
  long as either X25519 or ML-KEM holds. A peer that does not know it (an
  older irsync) still connects, with X25519.
- A daemon or a client can be set to refuse a connection that did not
  negotiate the hybrid. Without this setting the hybrid is a best effort,
  since a peer can simply not offer it.

Turned down:

- **Do nothing, document it.** No protection.
- **Prefer only.** Costs the same as the chosen option and gives no
  guarantee.
- **Hybrid only.** Breaks older peers. It also breaks n0's relays and lookup
  servers, which do not support the hybrid yet, because iroh uses the same
  crypto provider for them.

## What it does not cover

- **Identity stays Ed25519.** A future quantum attacker could forge a daemon
  or client key. That matters only once such a machine exists, and it needs
  a new key type in iroh, not in irsync.
- **Who talks to whom** is as visible as before.
- Release signatures and fileset manifests are a separate matter.

The threat model says so.

## Findings

Measured on 2026-11-22 on x86_64 Linux, with iroh 1.5.0 (commit 970c569),
iroh's `tls-aws-lc-rs` feature in place of `tls-ring`, and the hybrid
first in the key exchange list.

| Question | Finding |
| --- | --- |
| Build | aws-lc-rs builds for x86_64 Linux, glibc and musl, with only a C compiler: no cmake, no nasm. |
| One backend or two | One. ring leaves the dependency tree completely, also for OpenTelemetry (`opentelemetry-otlp` has a `tls-aws-lc` feature). |
| Supply chain | Six new crates in `Cargo.lock`: aws-lc-rs, aws-lc-sys, cmake, dunce, fs_extra, jobserver. `cargo deny --locked check` passes unchanged. |
| Binary size | 18.5 MB to 19.2 MB (+4%), release build, glibc. |
| Build time | About the same: 4m18s with ring (glibc), 4m28s with aws-lc-rs (musl), both clean. |
| Bulk speed | About 5% slower. A 3 GB pull over loopback, five interleaved runs each: 13.3 to 13.6 s with ring, 13.8 to 14.4 s with aws-lc-rs. CPU time 3 to 4% higher on both sides. |
| Negotiated group | Readable on both sides: `Connection::handshake_data()`, downcast to `noq_proto::crypto::rustls::HandshakeData`, field `negotiated_key_exchange_group`. |
| Handshake | Hybrid on both sides negotiates `X25519MLKEM768`. If either side is classical only, X25519. No difference in connect time that loopback could show. |
| Poor links | Every irsync test passes with the hybrid on, including the in-process netsim tests (`cargo test -p irsync --features netsim`). |
| Resumed connections | A resumed connection reports its own negotiated group. TLS 1.3 as rustls implements it does a fresh key exchange on every resumed handshake (not tested here, it follows from the protocol). irsync does not use 0-RTT, which would send data under an older session's keys. |

Loopback is the worst case for the bulk speed, because there the CPU is the
limit.

Not checked: macOS, aarch64 Linux and FreeBSD builds; `tools/ci.sh
release-build`; the five slow netstress tests; real relays; connect time on
a real link; bulk speed on aarch64.

One thing seen along the way: a client that offers *only* the hybrid hangs
against a classical-only server until it times out, while the reverse fails
at once with `NoKxGroupsInCommon`. The design below does not depend on it.

## Design

### Step 1: prefer the hybrid

- `Cargo.toml`: iroh with `default-features = false` and `metrics`,
  `fast-apple-datapath`, `portmapper`, `tls-aws-lc-rs`; noq-proto with
  `rustls`, `aws-lc-rs`, `tracing-log`, `bloom`; `opentelemetry-otlp`
  with `tls-aws-lc` in place of `tls-ring`; rustls with `aws-lc-rs` in
  place of `ring`, and no longer only for the `otel` feature.
- `net/endpoint.rs`: pass `Endpoint::builder(..).crypto_provider(..)` an
  aws-lc-rs provider whose `kx_groups` are `X25519MLKEM768`, `X25519`,
  `SECP256R1`, `SECP384R1`, in this order. Set the list ourselves: rustls
  puts the hybrid first only when built with `prefer-post-quantum`.
- `telemetry/otel.rs`: install the aws-lc-rs provider.
- Show the negotiated group: in the `-vv` session summary and the daemon's
  connection log line.
- `tools/sync-fuzz-lock.sh`, and the new crates in
  `book/src/contributing/supply-chain.md`.

### Step 2: the "require" setting

**It is a check after the handshake, not a shorter offer.** Both sides keep
offering the classical groups, and the side that requires the hybrid looks
at the negotiated group before it sends or reads any irsync message. This

- keeps relay and lookup TLS working, which share the provider and cannot do
  the hybrid yet,
- lets the refusing side say why, instead of a failed handshake,
- and is as safe as a shorter offer: nothing is sent before the check, and
  TLS 1.3 signs the whole handshake, so nobody on the path can remove the
  hybrid from the offer without breaking the handshake.

**Where it lives:**

- The setting is `pq_key_exchange`, not `key_exchange` as first proposed:
  the name says what it is about.
- Daemon: `[daemon] pq_key_exchange = "prefer" | "require"`. For the whole
  daemon, for now: the key exchange happens once per connection, before any
  path is named, and one rule is easier to reason about than a mix.
- Client: `pq_key_exchange` in `[client]` and per host entry, and a
  `--pq-key-exchange` flag.
- The default is `prefer` on both sides, for now. `require` as the default
  would lock out every irsync up to 0.1.0-beta.6.

**Left for later: a finer setting on the daemon.** A setting per export or
per grant can be added without changing this one. The daemon knows the
negotiated group for the whole connection and learns the export from
`Hello`, so it would refuse there, with an error message instead of a
closed connection. `[daemon] pq_key_exchange = "require"` would stay the
stricter rule that needs no `Hello`.

**The error:**

- The daemon checks the grant first, as today, so a stranger still learns
  nothing. A granted client without the hybrid gets the connection closed
  with a reason that the client prints: this daemon requires post-quantum
  key exchange, upgrade irsync. The close uses the code for a rejected
  connection (2), which every client, older ones too, takes as final and
  prints with its reason; another code would make an older client redial
  for its whole reconnect window.
- A client that requires it and did not get it closes and says the same
  about the daemon. Exit code as for other connection errors.

### Threat model

After step 1, "Data on the way" covers recorded traffic between two current
irsync versions. After step 2 a user can rely on it. The Ed25519 limit stays
under "What is left".

## Follow-up work

1. Switch to aws-lc-rs and prefer the hybrid (step 1), with the checks left
   open above.
2. The "require" setting (step 2).
