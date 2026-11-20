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

7. **Attestations, verified by hand.** dist attests every file it builds
   for the release in its host job (`github-attestations-phase = "host"`).
   The `.minisig` signatures, added later by hand, have no attestation (see
   17). dist's default attests only the per-target archives, which left
   the installer unattested up to v0.1.0-beta.2. `packages.yml` attests
   the packages. `install.sh` doesn't check them. Why: checking in the
   script would need `gh` on every user's machine. Cost if wrong: an
   install through `install.sh` trusts GitHub's release storage, as
   before.

8. **Every Action pinned to a commit.** Hand-written workflows carry a
   `# vX.Y.Z` comment, and Dependabot updates them. `release.yml` gets its
   pins from `[dist.github-action-commits]` in `dist-workspace.toml`
   (dist can't write the comment), and Dependabot leaves it alone, because
   an edit there would fail `.github/scripts/release-yml.sh check`. Cost if wrong: the
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

18. **Release files are signed with an offline minisign key** (issue #89).
    A maintainer signs every file on the draft release on their own
    computer with `.github/scripts/sign-release.sh`, then publishes the
    draft. The public key is in `packaging/signing/release-keys.pub`. Every
    signature has the trusted comment `irsync vX.Y.Z`, so an older
    release's signed files can't be passed off as a newer one. Why the key
    is offline: until now only GitHub vouched for a release, through
    attestations and checksums. Whoever takes over the organisation, a
    maintainer's token or a compromised Action can build and publish a
    release that passes both. A key that never touches GitHub closes that
    gap: nothing that happens on GitHub can make a valid signature. It also
    gives users a check that needs no `gh` and no GitHub account. The
    flow: `release.yml` makes a draft (the gate ends there), the script
    signs and publishes it, which creates the tag, and then starts the
    resume run. A draft takes new files even with immutable releases on, and
    has no tag, so a failed run leaves nothing behind. Cost if wrong: every
    release needs the maintainer, with the USB stick, at their computer.
    Losing the key means a new key and a changed `release-keys.pub`. A
    second maintainer can add their own key to that file.

19. **A stale advisory exception fails the dependency check.** `cargo deny`
    runs with `-D advisory-not-detected`, in `tools/ci.sh` and in
    `advisories.yml`. Why: an exception in `deny.toml` that no longer
    matches anything would stay for ever and hide the next advisory against
    that crate. Cost if wrong: a crate's fix can make the check fail until
    the exception is removed, which is a one-line change.

20. **Fuzzing with cargo-fuzz, in `fuzz/`.** One target per entry point that
    reads bytes from a peer or another program. The seeds checked in under
    `fuzz/seeds/` run on every pull request (`fuzz-build`). Each target is
    fuzzed for ten minutes every night (`nightly.yml`), on a corpus kept
    in the Actions cache. A crash fails the run and the failing inputs are
    uploaded as the `fuzz-artifacts` artifact, which is public. The
    maintainer chose to keep the upload: a red run is useless without the
    input, and anyone can run the same public fuzzer on the same code. The
    book says what to do with a crash that looks exploitable. No hosted
    fuzzing service for now. Cost if wrong: an input that shows a hole is
    public until the run is deleted. Revisit a hosted service if the
    nightly run finds too little or the maintainer wants crashes kept
    private.

21. **Static analysis in layers, with CodeQL advisory first.** Rules:
    `unsafe_op_in_unsafe_fn` and clippy's `undocumented_unsafe_blocks`, so
    every `unsafe` block says why it is sound; crates with no unsafe code
    forbid it. zizmor lints the workflows in the `workflows` job, with
    exceptions in `.github/zizmor.yml` that each give a reason. CodeQL runs
    on the Rust code and the workflows, but is not part of `CI result`
    until it has been quiet for a few weeks. Why: a check that cries wolf
    on its first day would block every pull request. Miri runs the tests it
    can run, on the crates that need no system calls, every night. Why not
    on every pull request: it is about a hundred times slower. Cost if
    wrong: undefined behaviour in unsafe code is found a day late. Making
    CodeQL required is a settings change once it has proved quiet.

22. **Private vulnerability reporting is on, and reports are answered within
    7 days.** `SECURITY.md` offers GitHub's **Report a vulnerability**
    button and `security@irsync.org`. The 7 days is the maintainer's
    number. Why: a reporter should have a private place that is not an
    email address, and a reply time makes the policy something people can
    rely on. `tools/github_settings.py` holds the setting and `check`
    reports it if it is off, so it can't drift unseen. Cost if wrong: one
    maintainer has to keep to 7 days. Turning the setting on needs an org
    admin.

23. **Scorecard comes last, and we do not chase the score.** The OpenSSF
    Scorecard runs weekly, on pushes to `main` and when branch protection
    changes (`scorecard.yml`), and publishes its result for the README
    badge. It was added after the other five changes, so that what it
    measures is already fixed or decided. The score describes the project;
    we change nothing only to raise it. Checks that stay low are listed in
    the book with the reason for each: one maintainer (Code-Review, parts
    of Branch-Protection, Contributors), a young repository (Maintained),
    files written by dist (Token-Permissions, some of Pinned-Dependencies),
    and release history (Signed-Releases). Why: a number we work to raise
    can drift away from what protects users. Cost if wrong: a visitor may
    read a middling score as neglect. The table is there to answer that.

## Left open

- `cargo-vet`, reproducible builds and signed commits. Not now, see the
  spec's non-goals.
