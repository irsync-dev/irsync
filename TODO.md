# Critical

- [ ] Sign release files and verify them against a public key built into
      irsync. Today a release download is checked only by SHA-256 over
      HTTPS, which catches a damaged file but not a swapped one. Applies
      to the cross-platform install over ssh (`ssh/release.rs`) and to
      `site/install.sh`.
    - Since 2026-11-15 every release file has a GitHub attestation, and
      anyone can check one by hand (`book/src/how-to/verify-a-download.md`).
      Neither of the two install paths checks it yet, so this stays open.

# Collection of open TODO items, in no particular order

- [ ] OpenTelemetry instrumentation, export on the daemon side, stats printing after a run
- [ ] Auto-update irsync on the remote side, with the correct architecture
    - [ ] Only happens when a run goes through ssh (first sync, `--ssh`, or the
          fallback when the daemon can't be reached); a reachable older daemon is
          not updated on its own
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

- [ ] A same-platform copy that does not run on the server (older glibc,
      NixOS build) is refused with a clear message since 2026-11-14. It
      could fall back to the release binary, which is static on Linux.
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
- [ ] The exit-3 sentinel could misreport a remote failure as a short
      upload.

## Tests

- [ ] CLI tests for `grant add immediate --ro` (usage error) and
      `grant remove immediate`; `grant_immediate` alias-vs-id dedup.
- [ ] Immediate peers: symlinked paths; `irsync -l newhost:` onboarding.
- [ ] `daemon_lock::acquire`'s retry path and the pid-0 probe result.
- [ ] `tests/fixtures/fake-ssh` takes `$1` as the destination (breaks if
      `--rsh` carries flags) and logs only the destination, not the command.
- [ ] Tidy: redundant `immediate: false` next to `..Grants::default()` in tests.

# Open items from the cross-platform install, 2026-11-14

Left over after `docs/superpowers/plans/2026-11-14-cross-platform-install.md`
landed. Minors from the final review; the signature check is under
"Critical" at the top.

## The download on the client (`ssh/release.rs`)

- [ ] No timeout or retry: a stalled connection hangs after "downloading
      irsync …" until Ctrl-C. Consider curl's `--connect-timeout`, `--retry`
      and `--speed-limit`/`--speed-time`.
- [ ] Ctrl-C during the download or upload leaves `irsync-release.*` (the
      tarball and the binary) in `TMPDIR`.
- [ ] The `wget` fallback runs with `-q`, so it prints nothing of its own on
      failure, and it has no https-only limit on redirects as curl does.
- [ ] A missing `tar`, or a GNU tar without `xz`, is noticed only after both
      downloads, and tar's multi-line error text lands in a one-line message.
- [ ] "Not in the tarball" is told from "tar failed" by tar's English "not
      found in archive"; under another locale a missing binary reads as
      "cannot unpack". Run tar with `LC_ALL=C`.
- [ ] GNU tar takes a `:` in the archive path for a remote host, so a
      `TMPDIR` with a colon breaks the unpack. Use `--force-local`, or run
      tar inside the temp directory with a relative name. bsdtar is fine.
- [ ] `~/.curlrc` is honoured, so an odd user config can change the
      download. `curl -q` would ignore it, proxy settings included.
- [ ] A clean build ahead of its release (`cargo install --git`, a committed
      branch) counts as a release build: every ssh run downloads and uploads
      the release binary before the server rejects it. Correct message, slow.
- [ ] The "downloading irsync …" line always goes to stderr; nobody checked
      whether a quiet flag should silence it.

## The upload command on the server (`ssh/client.rs`)

- [ ] A rejected install on a fresh server leaves an empty
      `~/.local/share/irsync/bin`, although the message says nothing was
      changed.
- [ ] A failed `cat`, `chmod` or `mv` (disk full) leaves the `.tmp.$$` file;
      only the size and version checks remove it. A `trap 'rm -f …' EXIT`
      would cover all of them. - [ ] A CPU that irsync does not know is reported as "an unknown CPU";
      pass the raw `uname -m` text through.

# Open items from the supply chain work, 2026-11-15

What the supply chain work (`docs/SUPPLY_CHAIN_DECISIONS.md`) left open:
decisions, checks it couldn't run, and the minor findings of the final
review, deferred.

## Decisions

- [ ] RUSTSEC-2026-0459 (bundled SQLite) has a TEMPORARY exception in
      `deny.toml`. The fix, rusqlite 0.41 with SQLite 3.54, breaks
      `lookups_are_never_re_prepared`, which is why rusqlite is held at 0.40.
      `fileset` opens manifests that may come from someone else, so the hole
      is real. Either take the re-prepare cost (or work around it) and
      upgrade, or write down why the exception stands.

## Checks not yet run

- [ ] Check an `.rpm` signed by the signing subkey with `rpm -Kv` on Fedora and
      on the oldest supported RHEL, after `rpm --import` of `irsync.asc`. The
      beta.3 dry run checked the apt metadata (good signature by subkey
      `561F2581…B9D0`), but not the `.rpm` files, and `packages.yml` doesn't
      check them either. Do it before the first stable release.
- [ ] `itemize_over_a_slow_link_stays_within_the_out_budget` (backpressure)
      fails now and then on ubuntu-latest with "daemon session did not
      finish": 2026-11-12 three times, 2026-11-15 once, each passing on a
      re-run. The release gate runs it too, so a flake stops a release.

## Deferred review findings

- [ ] `[dist.github-action-commits]` in `dist-workspace.toml` doesn't follow
      Dependabot: when it bumps checkout or the artifact Actions elsewhere,
      `release.yml` stays on the old commits. A policy test that compares
      the SHAs would flag the drift on the Dependabot PR.
- [ ] `tools/github_settings.py` edges:
    - [ ] The `release` reviewer is whoever runs `apply`. A second
          maintainer running it would replace the first. Keep the list in
          the file instead.
    - [ ] `two_factor_requirement_enabled` is visible only to org owners;
          for anyone else the check wrongly reports 2FA as off.
    - [ ] The required `CI result` check has no `integration_id`, so any app
          with write access could post it. Pin it to GitHub Actions (15368).
    - [ ] `repos/{repo}/rulesets` lists org rulesets too; one with the same
          name as a repo ruleset would make `apply` PUT to the wrong ID.
- [ ] A tag on any commit, not only on `main`, can start a release, and a
      pre-release is public before any approval. Fine while there's one
      maintainer. For the group phase, add a tag ruleset with a `creation`
      rule and a maintainer bypass (see `book/src/contributing/supply-chain.md`,
      "When more maintainers join").
- [ ] Releases tagged before 2026-11-15 can't be finished with the resume
      workflow: their copy requires `main`, the new one refuses it. Only
      pre-releases exist from then, so nothing needs it today.
- [ ] `book/src/how-to/verify-a-download.md` calls the tap `irsync-dev/tap`
      (Homebrew's short name). The repository is `irsync-dev/homebrew-tap`.
