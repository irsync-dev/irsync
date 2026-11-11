# Collection of open TODO items, in no particular order

- [x] rsync-like "ad-hoc" mode, boot-strapped via ssh: done by SSH onboarding
      (2026-11-10), but as a daemon that stays installed and running rather than
      one spawned and shut down per run.
    - [x] DHT: such daemons publish by default, since later runs must find them
          without ssh; `--no-remote-dht` turns it off, `--ssh` forces the ssh
          step (2026-11-10)
- [ ] OpenTelemetry instrumentation, export on the daemon side, stats printing after a run
- [ ] Auto-update irsync on the remote side, with the correct architecture
    - [x] Same OS and CPU: onboarding copies the client binary over ssh when the
          remote version differs (2026-11-10)
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

# Torn file when the source changes during hashing, 2026-11-10

- [x] A source file written while the sender is still hashing it can land at
      the destination as a mix of old and new content that the source never
      held, with no error, no warning and exit 0.

      **What happens.** The sender hashes the file in one pass and does not
      stat it again afterwards (`crates/irsync/src/hasher.rs:282-292`). If a
      writer changes the start after the pass has read it and the end before
      the pass reaches it, the digest holds old-start + new-end. The receiver
      takes the old start from its own old copy (local chunks verify against
      the digest) and the sender sends the new end, which also verifies in
      `read_chunk` (`crates/irsync/src/send.rs:640-675`). The whole-file hash
      matches the digest too. Nothing re-stats the source after the send or
      at `FileAck DONE` (`crates/irsync/src/session/sender_role.rs:870-903`).
      The comment at `hasher.rs:225-229` ("so that never lands") holds only
      for changes after hashing in chunks the sender reads. Those are
      caught: chunk mismatch or
      truncation → `InvalidData` → `retry` once (`sender_role.rs:425-459`,
      `854-857`, `MAX_RETRIES` at `:49`) → `SOURCE_ERROR` and exit 23 if it
      keeps changing.

      **Reproduction.** A temporary integration test in `crates/irsync/tests/`
      using `common::Harness` (`--features test-hooks`), push, `archive_opts()`.
      Source and destination both hold the same 512 MiB pseudo-random
      `x.bin`; the destination's mtime is set to 1e9 s so the planner picks a
      delta. A thread waits T ms after the sync starts, then opens the source
      `x.bin` for writing and, with `write_all_at`, writes 256 KiB of other
      data at offset 0 and then 256 KiB of other data at offset size-256 KiB.
      For T = 50, 150, 300, 500, 700 and 900 ms the sync (about 2 s) ended with
      `out.errors` empty, and the destination held the old first 256 KiB and
      the new last 256 KiB. A second `h.run` made it equal to the source.

      **Why the next run usually repairs it.** A retry or a torn send applies
      the listing's metadata, so the destination keeps the mtime the walk saw
      (`recv_io.rs:1091-1115`); the source's mtime has moved on, and the quick
      check sends the file again. It does not when the last write falls in
      the same timestamp tick as the walk's stat, or the writer resets the
      mtime; then the torn file stays until a `-c` run.

      **Candidate fixes.** (a) After hashing, fstat the open file and compare
      size, mtime, ctime and inode with the stat taken at open; on a
      difference, hash again or re-offer the file (counting against
      `MAX_RETRIES`), and fail with exit 23 if it does not settle. (b) At
      `FileAck DONE`, re-stat the source and compare with the stat the digest
      was taken under; on a difference, warn or exit 23. rsync has the same
      weakness, so neither is needed for parity, only for correctness.

      Needs its own design, a regression test (the reproduction above, made
      deterministic with a hook inside the hash pass), and a docs update once
      fixed: the "Files that change during a sync" section of
      `book/src/explanation/correctness.md`, and the "Change during transfer"
      row and Integrity bullet of `book/src/explanation/transfer-algorithm.md`.

      **Closed 2026-11-11.** Fixed on the `source-change-detection` branch
      (commits 97f253b to 1f80dd3): the sender stats the file after hashing,
      and the receiver asks before placing; one retry, then exit 23. New
      `--paranoid` sends from reflink clones or hashes again, and re-checks
      every listed file at the end (exit 24 if files vanished). Design:
      `docs/superpowers/specs/2026-11-10-source-change-detection-design.md`.
      Limits (memory-mapped writers without `--paranoid`, coarse timestamps)
      are documented in the correctness page.
