# Known limitations

- **Idle connections are dropped.** The daemon drops an authenticated
  connection that has had no traffic and no local progress, such as hashing or
  writing, for `idle_timeout`, 30 minutes by default. The client reconnects
  and resumes. See [Configuration](configuration.md#daemon).
- **Only file-based Ed25519 keys.** Hardware keys and ssh-agent keys cannot be
  used. Of the keys under `~/.ssh`, only `id_ed25519` is tried, unless
  `identity` in the config file or `--identity` names another. See
  [Configuration](configuration.md#identity-and-state).
- **Relays are slow.** The throughput target, filling a 1 Gbit/s link, applies
  to direct connections. Relays are a fallback and are much slower. Behind a
  symmetric NAT, where direct connections fail, run a self-hosted relay and
  point `relay` in [`[client]`](configuration.md#client) and
  [`[daemon]`](configuration.md#daemon) at it.
- **No compression.** `irsync` has no counterpart to rsync's `-z`; data
  crosses the link as it is stored.
- **Linux and macOS only**, on x86_64 and aarch64, as client and as daemon.
