# Collection of open TODO items, in no particular order

- [ ] rsync-like "ad-hoc" mode that automatically spawns (and shuts down) a daemon on the other side
    - Boot-strap via ssh (similar to mosh)
    - Don't register in DHT for ad-hoc mode
- [ ] OpenTelemetry instrumentation, export on the daemon side, stats printing after a run
- [ ] Auto-update mode that updates irsync on the remote side, with correct architecture (via ssh / scp)
- [ ] Adversarial fuzz testing / hardening for the daemon mode
- [ ] Performance and resource measurements on daemon mode, potentially re-think execution model (fork vs. threads, ulimits)

# Open items found while working, 2026-10-28

After the single-config work landed on main. Nothing below is tracked
anywhere else: the code has no TODO markers, no ignored tests and no
unfinished plan workspaces.

## Known limitations (README "Known limitations in version one")

Rough cost next to each. The last two are not things to fix.

- Relays are a fallback and much slower than direct connections. By design.
- Linux and macOS only. A port, not a fix.

## fileset: deferred findings

`docs/superpowers/plans/2026-10-19-fileset-deferred.md` is the ledger of
minors deferred from the fileset reviews. Everything in it has since been
fixed or covered by a test (2026-10-29), except two that need what the code
does not have yet:

- [ ] A test for the once-only "cannot read directory" warning on a `readdir`
      that fails mid-listing: needs fault injection into the walk.
- [ ] A test for a SQLite manifest migrating from an older schema: needs a
      second schema version to exist.
