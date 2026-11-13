# Collection of open TODO items, in no particular order

- [ ] OpenTelemetry instrumentation, export on the daemon side, stats printing after a run
- [ ] Auto-update irsync on the remote side, with the correct architecture
    - [ ] Only happens when a run goes through ssh (first sync, `--ssh`, or the
          fallback when the daemon can't be reached); a reachable older daemon is
          not updated on its own
    - [ ] Other OS or CPU: see the release-binary item under "Open items from SSH
          onboarding" below
- [ ] Adversarial fuzz testing / hardening for the daemon mode
- [ ] Performance and resource measurements on daemon mode, potentially re-think execution model (fork vs. threads, ulimits)

# Open items found while working, 2026-10-28

After the single-config work landed on main. Nothing below is tracked
anywhere else: the code has no TODO markers, no ignored tests and no
unfinished plan workspaces. Every fixable entry of the known limitations
(now `book/src/reference/limitations.md`) was fixed by 2026-10-29; what the
book still lists there is by design (relay speed, Linux and macOS only).

## fileset: deferred findings

`docs/superpowers/plans/2026-10-19-fileset-deferred.md` is the ledger of
minors deferred from the fileset reviews. Everything in it has since been
fixed or covered by a test (2026-10-29), except two that need what the code
does not have yet:

- [ ] A test for the once-only "cannot read directory" warning on a `readdir`
      that fails mid-listing: needs fault injection into the walk.
- [ ] A test for a SQLite manifest migrating from an older schema: needs a
      second schema version to exist.

# Open items from SSH onboarding, 2026-11-10

Left over after `docs/superpowers/plans/2026-11-10-ssh-onboarding.md`
landed. Items the final review's fix wave closed are not listed.

## Later item from the spec

- [ ] Install on a server whose OS or CPU differs from the client's: download
      the release binary for the client's exact tag (or `latest` for a dev
      build), check its signature, copy it over ssh. Waits for the release
      pipeline (see the spec's "Later item").

## Grants and automatic exports

- [ ] `grant list WHO` still shows `immediate: rw` and auto-export lines when
      `daemon.immediate = false`, although the daemon ignores them
      (`list_peer` builds its snapshot with immediate on). Over-reports, never
      under-reports.
- [ ] The "no exports defined" startup warning is skipped when `[immediate]`
      has entries, even if `daemon.immediate = false`.
- [ ] GC of an automatic export (often `$HOME`) is not limited to one
      filesystem.
- [ ] A session on an outer export that writes below a nested export's root
      leaves stale records in the outer index that no GC collects (the outer
      GC skips that subtree). Small leak, no data loss.
- [ ] `nested_roots` compares paths as written, so symlinked or
      non-canonical layouts fall back to the old GC behaviour for that pair.
- [ ] A push to a missing directory creates it before the canonical-root
      check and before the export is added; if either fails, the empty
      directory stays.
- [ ] `add_auto_export` stores `to_string_lossy()` and the name drops `.`/`..`;
      callers must pass canonical UTF-8 paths (the daemon does).
- [ ] Error text sent to an immediate peer when the config is locked includes
      the lock file's path.
- [ ] Where `$HOME` sits under a symlink, every connection takes the config
      lock to re-check an automatic export (correct, extra lock traffic).

## ssh-setup and the daemon lock

- [ ] Setup reports "started" once the daemon holds `daemon.lock`, which is
      before the endpoint is bound; a bind failure then looks like a dead
      daemon. Write the lock record (or wait for "endpoint id:") after binding.
      Since 2026-11-10 setup waits up to 10 s for `daemon.addr`, which is
      written after binding, but it still says "started" when the daemon
      dies in that time.
- [ ] Two `ssh-setup` runs at once: a probe can read the lock record as
      "owner unknown" (pid 0, reported as foreign), and the second run can
      fail with "daemon exited before it was ready". Re-probe briefly; treat a
      holder that is our exe and version as Running.
- [ ] An encrypted daemon key is a dead end: setup stops before grant and
      probe on every rerun, even after the user starts the daemon by hand.
      Could read the endpoint id from the `.pub` file. No test for the error.
- [ ] The spawned daemon keeps the ssh session's working directory
      (`current_dir("/")` would be cleaner); `daemon.log` grows without limit.
- [ ] The SIGTERM handler is registered inside a spawned task, so a very early
      SIGTERM uses the default action (harmless: the lock is released anyway).

## ssh client and onboarding

- [ ] A copied binary with the same OS/CPU may still not run (older glibc,
      NixOS build); the probe then sees no version and reinstalls every run.
      Run `--version` after the upload and say "the copied binary does not
      run on <dest>".
- [ ] The first run makes three ssh connections (probe, install, setup), so
      password users get three prompts. Consider ControlMaster or folding
      probe and setup together.
- [ ] Listings (`irsync myserver:`) do not fall back to ssh when the daemon
      is silent; only syncs do.
- [ ] A failed `connect_for` rebuild in the fallback loop returns from `sync`,
      dropping earlier sources' totals and a `SessionEnd`.
- [ ] The client identity is generated before onboarding, even when it then
      stops (other platform).
- [ ] An alias save that fails after setup has changed the server prints no
      summary of what was done there.
- [ ] The launchd plist in the how-to has a literal `$HOME` to replace by hand.
- [ ] The platform refusal omits the raw `uname -sm` text; `replaced` is empty
      when an unreadable file was overwritten; `run_setup` doesn't tell ssh's
      exit 255 from a remote failure; a whitespace-only `--rsh` ignores
      `IRSYNC_RSH`.
- [ ] A failed `cat` during upload leaves `irsync.tmp.$$` behind; the exit-3
      sentinel could misreport a remote failure as a short upload.

## Tests

- [ ] CLI tests for `grant add immediate --ro` (usage error) and
      `grant remove immediate`; `grant_immediate` alias-vs-id dedup.
- [ ] Immediate peers: symlinked paths; `irsync -l newhost:` onboarding.
- [ ] `daemon_lock::acquire`'s retry path and the pid-0 probe result.
- [ ] `tests/fixtures/fake-ssh` takes `$1` as the destination (breaks if
      `--rsh` carries flags) and logs only the destination, not the command.
- [ ] Tidy: redundant `immediate: false` next to `..Grants::default()` in tests.
