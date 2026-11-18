# Supply chain security: decisions

This file records the decisions behind how irsync guards the way from a
commit to a user's install. They come from
`docs/superpowers/specs/2026-11-14-supply-chain-security-design.md` and
`docs/superpowers/plans/2026-11-15-supply-chain-security.md`, plus the calls
made while building it. What is in place is described in
`book/src/contributing/supply-chain.md`. Where the code and this file
differ, the code is what shipped.

## Decisions

1. **Rules a single maintainer can live with.** `main` takes changes only
   through a pull request that passed `CI result`, with 0 required
   approvals, and nobody can bypass that, admins included. The release
   environment lets the person who tagged approve. Why: irsync has one
   maintainer today, and a rule that needs a second person would block all
   work. Cost if wrong: a stolen account can still merge its own pull
   request. Two-factor login is the guard there. When maintainers join,
   approvals and `CODEOWNERS` are settings changes.

2. **Release tags can be created but never moved or deleted, with no
   bypass.** Why: a published version must always mean the same commit.
   Cost if wrong: a mistaken tag can't be cleaned up. The answer is the
   next version.

3. **Release secrets in a `release` environment, `v*` tags only, one
   approval.** Why: a change merged to `main` can't reach the signing key or
   the tokens, and a tag alone isn't enough without the click. Cost if
   wrong: one manual step per stable release.

   The gate and resume call `packages.yml` and `publish-homebrew.yml` with
   `secrets: inherit`. Without it, the called job got the environment's
   secrets empty, even though it names the environment itself (found on the
   v0.1.0-beta.2 resume dry run). `inherit` also passes repo-wide secrets, so
   there must be none: `tools/github_settings.py` checks that.

4. **The resume workflow runs on the tag, not on `main`.** Why: the
   `release` environment opens only for tags. Cost if wrong: a fix to a
   release script on `main` doesn't reach a release already tagged. That
   release gets finished by hand, or the next version carries the fix.

5. **The release gate publishes the Homebrew formula, not dist.** dist
   0.34.0 writes `uses: $/.github/workflows/<job>.yml` for a local custom
   publish job, which GitHub can't run. Its built-in Homebrew job can't use
   an environment. So `publish-jobs = []`, and the gate runs
   `publish-homebrew.yml` next to the packages job, fetching the `irsync.rb`
   that dist attached to the release. Why: `release.yml` needs no secrets,
   and one approval covers both jobs. Cost if wrong: the tap updates
   minutes after the release is announced, not before. Revisit when dist
   fixes the template.

6. **Immutable releases; packages not attached.** Once published, a
   release's files can't change, so the `.deb` and `.rpm` files live only in
   the apt/dnf repo, each with an attestation. Why: nobody, a maintainer
   included, can swap a file on a release. Cost if wrong: people who
   downloaded packages from the release page now use dist.irsync.org.

7. **Attestations, verified by hand.** dist attests every file on the
   release in its host job (`github-attestations-phase = "host"`). Its
   default attests only the per-target archives, which left the installer
   unattested up to v0.1.0-beta.2. `packages.yml` attests the packages. `install.sh` doesn't
   check them. Why: checking in the script would need `gh` on every user's
   machine. Cost if wrong: an install through `install.sh` trusts GitHub's
   release storage, as before.

8. **Every Action pinned to a commit.** Hand-written workflows carry a
   `# vX.Y.Z` comment, and Dependabot updates them. `release.yml` gets its
   pins from `[dist.github-action-commits]` in `dist-workspace.toml`
   (dist can't write the comment), and Dependabot leaves it alone, because
   an edit there would fail `dist generate --check`. Cost if wrong: the
   dist table is bumped by hand.

9. **cargo-deny fails on any advisory; exceptions need a reason.**
   Unmaintained and yanked crates fail too. Only crates built for the
   platforms irsync is built for count (`targets` in `deny.toml`); the
   licenses of all the others are still checked before a release
   (`deny-license-check.toml`). Cost if wrong: a platform we build for
   but forgot to list goes unchecked. Accepted today:
   - `paste` (RUSTSEC-2024-0436) and `unsafe-libyaml` (RUSTSEC-2024-0449):
     unmaintained, with no known hole. One is a build-time macro reached
     through iroh. The other is used only by the unshipped `filegen`.
   - SQLite (RUSTSEC-2026-0459), **temporarily**: the fix (rusqlite 0.41,
     SQLite 3.54) breaks `lookups_are_never_re_prepared`, which is why
     rusqlite is held at 0.40. `fileset` opens manifests that may come from
     someone else, so this needs a decision, not a standing exception.

   Cost if wrong: a known hole ships.

10. **Licenses: today's set, all compatible with MIT.** Crates offering a GPL
    or LGPL alternative are used under their MIT or Apache option. MPL-2.0
    (`attohttpc`, `option-ext`) is allowed: it's file-level copyleft, and
    crates.io keeps the sources available. Cost if wrong: one entry to
    remove.

11. **Dependabot waits 7 days**, weekly, one grouped PR per ecosystem;
    security updates don't wait. Why: most hijacked releases are found
    within days. Cost if wrong: a wanted update arrives a week later.

12. **`install.sh` refuses plain HTTP.** It uses curl with `--proto =https`,
    or wget with `--https-only`. BusyBox wget has no such flag, so with only
    that the script asks for curl. Cost if wrong: Alpine users without curl
    install it first.

13. **The signing key's primary part stays offline.** GitHub holds a
    signing subkey. Users trust the primary fingerprint. Why: a leaked
    subkey can be revoked and replaced without every user importing a new
    key. Cost if wrong: one more step when making the key.

14. **Settings as code.** `tools/github_settings.py` holds the desired
    state for the four repositories and can check it at any time. Why:
    settings changed by hand drift unseen. The org's two-factor requirement
    has no API, so the script only reports it.

15. **The tap formula is the attested `irsync.rb` plus two lines.**
    `publish-homebrew.yml` runs `.github/scripts/formula-man-pages.sh` on the
    formula it downloads from the release, before it pushes to the tap. It
    runs before the tap is checked out, so no repo code runs while
    `HOMEBREW_TAP_TOKEN` is on disk. The
    script inserts `man1.install` and `man5.install` lines ahead of dist's
    `leftover_contents` line. Why: dist has no hook for this, and its
    formula puts every leftover file in `pkgshare`, where `man` doesn't look.
    The script comes from the release's own tag. It fails if dist's template
    changes and the anchor line goes missing. Cost if wrong: someone
    comparing the tap to the release's `irsync.rb` sees a difference. The
    script and its test show exactly which lines.

16. **`irsync-installer.sh` is ours, not dist's.** Since the release after
    0.1.0-beta.3, `packaging/installer/build-installer.sh` generates it,
    run by dist through `extra-artifacts` in the `build-global-artifacts`
    job. It has the SHA-256 checksums of the three tarballs written into
    it, and it is attested with the other release files like before. Why:
    dist's installer can't install the man pages. Cost if wrong: we maintain a small POSIX
    shell script ourselves, and a change to the tarball names must change it
    too.

17. **Developer tools are downloaded and checked against hashes in the
    repo.** `tools/dev-setup.sh` fetches cargo-nextest, cargo-deny, dist,
    mdbook and actionlint as release binaries and refuses one whose SHA-256
    is not the one in the script. A refused or failed download is reported;
    it never turns into a build from source. Why: building them took
    minutes on every new machine and version bump. cargo-binstall was
    turned down: it verifies minisign signatures only for a tool whose
    `Cargo.toml` names a key, and none of these does, so it would check
    nothing beyond HTTPS. Cost if wrong: the hashes are replaced by hand on
    each bump, and mdbook publishes none, so its hashes are our own first
    download. Revisit if the tools start signing.

## Left open

- `cargo-vet`, reproducible builds and signed commits. Not now, see the
  spec's non-goals.
- The SQLite exception (decision 9).
