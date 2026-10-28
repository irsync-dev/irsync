# Collection of open TODO items, in no particular order

- [ ] rsync-like "ad-hoc" mode that automatically spawns (and shuts down) a daemon on the other side
    - Boot-strap via ssh (similar to mosh)
    - Don't register in DHT for ad-hoc mode
- [ ] OpenTelemetry instrumentation, export on the daemon side, stats printing after a run
- [ ] Auto-update mode that updates irsync on the remote side, with correct architecture (via ssh / scp)
- [ ] Adversarial fuzz testing / hardening for the daemon mode
- [ ] Performance and resource measurements on daemon mode, potentially re-think execution model (fork vs. threads, ulimits)

