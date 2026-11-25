# Releasing

You start a release by hand, with one command. GitHub Actions tests the
commit and builds the files, and leaves a draft GitHub Release. Then you sign
the files on your own computer with an offline key, and publish the draft.
Publishing creates the tag. You never push a release tag. This page says how
to cut a release, what the workflows do, what to do when one fails, and what
to set up once.

Only three kinds of version release anything:

| Version | What it is | Where it goes |
| --- | --- | --- |
| `X.Y.Z` | A stable release | GitHub Release, Homebrew, apt, dnf |
| `X.Y.Z-beta.N` | A beta | GitHub Release only |
| `X.Y.Z-alpha.N` | An alpha | GitHub Release only |

The tag is the version with a `v` in front: `vX.Y.Z`. Any other version
stops the gate with an error within seconds, before the tests. Nothing is
published.

## Where a release is cut from

- A pre-release (`X.Y.Z-beta.N`, `X.Y.Z-alpha.N`) and a new minor or major
  version (`X.Y.0`) are cut from `main`.
- A patch release (`X.Y.Z` with `Z` above 0) is cut from the branch
  `release-X.Y`. The resume run creates that branch after `X.Y.0` is
  published.

`main` and every `release-X.Y` branch only take changes through a pull
request that passed the `CI result` check. The gate refuses to run on the
wrong branch.

## Cutting a release

The three cases differ only in the branch. All the steps are the same.

1. Switch to the branch the release is cut from, up to date and with no
   uncommitted changes: `main` for a pre-release or `X.Y.0`, `release-X.Y`
   for a patch. Run the bump script with the new version, without the `v`:

   ```sh
   git switch main && git pull --ff-only     # or release-1.2
   .github/scripts/bump-version.sh 1.2.3
   ```

   It stops if you are on the wrong branch. It sets the version in
   `Cargo.toml` (`[workspace.package]`) and `packaging/dist/dist.toml`, and
   updates `Cargo.lock` and `fuzz/Cargo.lock` to match. It commits the four
   files as `Release v1.2.3` on a new branch, `bump-v1.2.3`. It doesn't
   push or tag. It prints the commands for the next steps.
2. Write the release notes: a section `## v1.2.3` in `CHANGELOG.md`, above
   the older ones, and commit it on the bump branch. See
   [The release notes](#the-release-notes) for what goes in it.

   ```sh
   git commit -m 'Release notes for v1.2.3' -- CHANGELOG.md
   ```

3. Open a pull request against the branch the release is cut from. Use
   `--base`:

   ```sh
   git push -u origin bump-v1.2.3
   gh pr create --fill --base main           # or release-1.2
   ```

4. Wait for its `CI result` check to pass. That includes the `release build`
   jobs, which run the Linux release build the way the release will. Then
   merge it: `gh pr merge --merge`. On `main`, the merge also rebuilds the
   site: [dev.irsync.org](https://dev.irsync.org/) shows the new version
   and its documentation from then on, before it is published (see
   [The release versions](website.md#the-release-versions) and
   [The books](website.md#the-books)). Until the release is published,
   the site cannot be promoted to irsync.org.
5. Start the release gate on that branch, with the version:

   ```sh
   gh workflow run release-gate.yml --ref main -f version=1.2.3
   ```

   For a patch, use `--ref release-1.2`. In the browser: Actions, Release
   gate, Run workflow, pick the branch, enter the version.
6. When the gate is done, it prints `Draft ready. Sign it:` and the
   commands. Plug in the USB stick with the release key and sign the draft,
   with the version:

   ```sh
   .github/scripts/sign-release.sh --key /media/$USER/irsync-key/irsync-release.key 1.2.3
   ```

   It checks the draft, asks you to confirm, asks for the key's password
   once, signs every file and uploads the signatures. The release stays a
   draft. Look at it on GitHub.
7. Publish it by running the same command with `--publish`:

   ```sh
   .github/scripts/sign-release.sh --key /media/$USER/irsync-key/irsync-release.key --publish 1.2.3
   ```

   It signs again, publishes the release and starts the resume run. It
   also re-runs the newest staging build of the site, which now reads the
   release from its tag. See [Signing the release](#signing-the-release).
8. Put the new version on irsync.org. The script ends by printing the
   command. Wait for the staging run it started, check that
   [dev.irsync.org](https://dev.irsync.org/) shows the version and that
   `/book/` there documents it, then run:

   ```sh
   gh workflow run site.yml --ref main
   ```

   See [Promoting to irsync.org](website.md#promoting-to-irsyncorg).

This is the same for a patch release. Its bump lands on `release-X.Y`,
which does not rebuild the site, but publishing it does.

For a pre-release, use a version such as `1.2.3-beta.1` or `1.2.3-alpha.1`.
For `1.3.0`, the resume run also creates the branch `release-1.3` at the
released commit.

Don't start the gate twice for the same version while one run is going. The
second run deletes the draft the first one made.

## The release notes

Every release has a section in `CHANGELOG.md`, at the root of the
repository, under a heading that is exactly `## v` and the version:

```markdown
## v1.2.3

One or two sentences on what this release is about.

### Upgrading

- What someone who runs the previous release has to do or know.

### New

### Fixed
```

It is written for people who run irsync, not for people who read the code:
what is new, what changes for them, what was fixed. Leave out a part that
has nothing in it. A stable release sums up the betas and alphas before
it, since most people go from one stable release to the next.

dist reads the section and puts it at the top of the GitHub Release, under
`Release Notes`, so it is in the draft you look at and sign. The full list
of changes still follows it (see below). These checks keep a release from
going out without its notes:

- A test (`crates/irsync/tests/release_version.rs`) fails when
  `CHANGELOG.md` has no section for the version in `Cargo.toml`, so the
  pull request with the bump cannot pass `CI result` without one.
- The release gate stops within seconds when the section is missing or
  empty (`.github/scripts/changelog-section.sh`).

A pull request that only touches Markdown skips the tests. So CI also runs
that script on every pull request, and an edit to `CHANGELOG.md` that
breaks the section of the current version fails `CI result` too.

A change merged after the notes were written and before the gate starts is
in the release and not in the section. The full list below it has it.

## Changing the version by hand

If you change the version by hand instead, keep all four files in step.
These checks catch a mismatch:

- A test (`crates/irsync/tests/release_version.rs`) fails if `Cargo.toml` and
  `dist.toml` differ.
- CI builds with `--locked`, so it fails if `Cargo.lock` is out of date. The
  release build uses `--locked` too, and would fail later. The `check` and
  `fuzz-build` jobs do the same for `fuzz/Cargo.lock`.
- The release gate stops within seconds if the two files don't hold the
  version you gave it.

## Copying a fix to a release branch

Only the newest `release-X.Y` branch gets fixes. Land the fix on `main`
first. Then copy it to the release branch with a pull request (here for
`release-1.2`):

```sh
git switch release-1.2 && git pull --ff-only
git switch -c fix-short-name
git cherry-pick -x <commit>
git push -u origin fix-short-name
gh pr create --fill --base release-1.2
```

`-x` writes the original commit into the message. Merge the pull request
once `CI result` passes. Then release the patch as above, from
`release-1.2`.

## What the workflow does

You start `.github/workflows/release-gate.yml` by hand. It is the only way a
release starts. It does this, in order:

1. **Check.** It checks the version, that it runs on the right branch, and
   that the version in `Cargo.toml` and `packaging/dist/dist.toml` is the
   one you gave. It checks that `CHANGELOG.md` has the release notes of
   that version (see [The release notes](#the-release-notes)). It checks that the tag doesn't exist yet and that there are
   no stray release tags (see
   [Protecting the release tags](#protecting-the-release-tags)). For `X.Y.0`
   it checks that `release-X.Y` doesn't exist yet. For a patch it checks
   that `release-X.Y` does. This takes seconds.
2. **Test.** It runs `ci.yml` on the branch's commit, with the musl release
   targets added. If anything fails, it stops. Nothing is published, and no
   tag exists.
3. **Pin.** It makes a branch `pinned/vX.Y.Z` at the tested commit, so the
   release builds exactly that commit even if the branch moves on. Don't
   merge a change to a workflow file into the branch while the gate runs:
   GitHub then refuses to let the gate make this branch (see
   [If a release fails](#if-a-release-fails)). It deletes a `pinned/vX.Y.Z` left over from an earlier failed attempt. It
   refuses to go on if a `release.yml` run from an earlier attempt is still
   going. It also deletes a draft release left over for the tag.
4. **Draft.** It launches `release.yml` on the pinned branch and waits for
   it. `release.yml` builds `irsync` and `fileset` for Linux x86_64, Linux
   arm64 and macOS on Apple Silicon. Each tarball also holds the man pages
   `irsync(1)`, `fileset(1)` and `irsync.toml(5)`, built from the book with
   pandoc. It creates a **draft** GitHub Release with the tarballs,
   checksums, our installer `irsync-installer.sh` and the Homebrew formula,
   each with an attestation. (The signatures added later have none.) A
   draft is not public and has no tag. It still takes new files. The gate
   then deletes the `pinned/` branch and prints the command that signs and
   publishes the draft. It ends there.

You then run `.github/scripts/sign-release.sh`, on your own computer (see
[Signing the release](#signing-the-release)). It signs the files and
publishes the draft. Publishing creates the tag `vX.Y.Z`. That is the point
of no return: the release is immutable from then on, and no file on it can
be replaced or added. The script then starts
`.github/workflows/release-resume.yml` on the new tag. That run does the
rest, because the secrets of the `release` environment open only for runs
on a `v*` tag:

- **Branch** (`X.Y.0` only). It creates `release-X.Y` at the released
  commit.
- **Packages and Homebrew** (stable releases only). Both jobs wait for
  you to approve the `release` environment. Open the resume run, click
  "Review deployments", tick `release` and approve: one click lets both
  start.
  - `.github/workflows/packages.yml` builds the `.deb` and `.rpm` files
    from the released Linux tarballs. It signs the `.rpm` files, adds
    everything to `irsync-dev/packages`, signs the apt and dnf metadata
    and pushes. It keeps the newest 3 versions. GitHub Pages serves the
    result at `dist.irsync.org`. The packages get attestations, but are
    not attached to the release.
  - `.github/workflows/publish-homebrew.yml` pushes the formula from the
    release to `irsync-dev/homebrew-tap`. First it runs
    `.github/scripts/formula-man-pages.sh`, which adds two lines to the
    formula (`man1.install` and `man5.install`), so brew puts the man
    pages where `man` looks. This happens before the tap is checked out
    with its token. The script fails if dist's formula template changed,
    so look at the formula then.
- **Notes.** It adds GitHub's list of changes to the release notes, below
  what the draft already has, under the heading `## What's changed`. For a stable release the list starts
  at the previous stable release, so it covers the betas in between too.

The release tag is a lightweight tag made by GitHub, not an annotated one.

The install script `site/install.sh` (served as `irsync.org/install.sh`)
picks the newest release for its channel: stable by default, `--beta` for
betas and stable, `--alpha` for all. `--require-signature` makes it fail
when it cannot check the installer's minisign signature. Other arguments
go to the installer `irsync-installer.sh` that `release.yml` attached to
the release. It puts the programs in `~/.local/bin` and the three man
pages in `~/.local/share/man/man1` and `man5`, and has no updater. It writes no
receipt and never edits the shell's startup files. If `~/.local/bin` is not
on `PATH`, it prints the line to add. `--uninstall` removes the two
programs and the three pages. Before it places
anything, it checks that it can write to `~/.local/bin` and both man
directories. If it cannot write to `~/.local/bin`, it stops and changes
nothing. If it cannot write to a man directory, it installs the programs
without the pages and prints a warning that names the directory.

It accepts the flags dist's installer took (`--no-modify-path`, `--quiet`,
`-q`, `--verbose`, `-v`) and `IRSYNC_NO_MODIFY_PATH`, and ignores them, so
scripts written for dist's installer keep working; `-h` is `--help`. dist's
install directory variables (`IRSYNC_INSTALL_DIR`,
`CARGO_DIST_FORCE_INSTALL_DIR` and `IRSYNC_UNMANAGED_INSTALL`) are refused:
when one is set, the installer stops before it does anything, since it
always installs under `~/.local`. On an Apple Silicon Mac under
Rosetta it installs the Apple Silicon build. Intel Macs are not supported.
Its source is `packaging/installer/irsync-installer.sh.in`.

## The release workflow is generated

`.github/workflows/release.yml` is made by
`.github/scripts/release-yml.sh` from what `dist generate` writes (cargo
dist 0.34.0), with `--draft` added so the release is created as a draft.
dist does not manage the file any more (`allow-dirty = ["ci"]` in
`dist-workspace.toml`). Never edit it by hand. The settings are in
`dist-workspace.toml`, and the package is described in
`packaging/dist/dist.toml`. After you change `dist-workspace.toml`, run:

```sh
.github/scripts/release-yml.sh write
```

and commit the result. `.github/scripts/release-yml.sh check` says whether
the file is up to date; CI runs it.

The installer is not dist's. `dist-workspace.toml` lists
`irsync-installer.sh` under `extra-artifacts`, and dist builds it with
`packaging/installer/build-installer.sh`, which fills in the version and the
checksums of the three tarballs. The man pages get into the tarballs through
`packaging/dist/build.sh`, which runs `tools/build-man.sh`. The build
runners need pandoc, which `dist-workspace.toml` lists as a dependency.
This needed no change to `release.yml`: the workflow
reads the runners' packages and the extra artifact from `dist plan` when it
runs.

`release.yml` only starts when the gate launches it. **Never launch it by
hand from the Actions tab.** That skips the tests.

## If a release fails

The tag is created when you publish the draft, so a failure before that
leaves no tag behind. Fix the cause and start the gate again with the same
version. It deletes a leftover draft.

**The site cannot be promoted.** From the merge of the bump until the
release is published, the promote refuses the staging build: it shows a
version nobody can install yet. Publish the release, or, if the release is
abandoned, land the next bump or promote with `allow-unpublished` (see
[Promoting to irsync.org](website.md#promoting-to-irsyncorg)).

**The check failed.** Nothing ran. Read the error. Usually the version in
the two files does not match, `CHANGELOG.md` has no notes for the version,
or you are on the wrong branch, or the branch `release-X.Y` is missing or
already there. Fix it and start the gate again.
A stray tag is different: see
[Protecting the release tags](#protecting-the-release-tags).

**The test step failed.** Nothing was published, and no tag exists. If it
was a flaky test, use "Re-run failed jobs" on the gate run. If the failure
is real, fix it on the branch through a pull request, then start the gate
again with the same version.

**The pin step failed.** Nothing was built or published, and no tag
exists. The usual cause: a change to a workflow file was merged into the
branch while the tests ran. GitHub does not let the gate's built-in token
create a branch then, and answers "Resource not accessible by
integration". The error names the workflow files that changed and the
command that starts the gate again; run it, on the branch's new commit.

**The release step failed.** Look at the `release.yml` run it launched. The
link is in the gate's log. If the gate could not find the run, look in the
Actions tab under the release workflow: it may still be running.

- If the draft was not created, or you don't trust it, nothing was published
  and there is no tag. Fix the cause, then start the gate again with the
  same version. A leftover `pinned/vX.Y.Z` branch and a leftover draft are
  deleted by the next run. If a `release.yml` run is still going, the gate
  refuses to start: cancel that run first (`gh run cancel <id>`), or wait
  for it to end.
- If the draft was created but the gate lost track of the `release.yml`
  run, the draft is fine. Sign it with `sign-release.sh`, then publish it
  with `sign-release.sh --publish`.

**Signing failed.** See [Signing the release](#signing-the-release).

**The tag exists, but the release is not finished.** The gate refuses to
start again, since the tag can't be reused. Finish the release with the
resume workflow (see below). For `X.Y.0`, also create the branch
`release-X.Y` at the released commit by hand, if it is missing (see
[Creating `release-X.Y` by hand](#creating-release-xy-by-hand)).

**The packages or Homebrew step failed.** Fix the cause (often a token in
the `release` environment), then re-run that job from the resume run, or
start a new resume run. Both are safe to run again. If the packages repo
already has this version, the packages job builds nothing and changes
nothing there. If the tap already has the formula, the Homebrew job pushes
nothing.

**The notes step failed.** Re-run that job, or start a new resume run. It
does nothing if the heading is already there.

**The release is out, but the branch, packages, Homebrew or the notes did
not happen.** This is what you see if the resume run was cancelled, or
`sign-release.sh` could not start it. Start the
resume run by hand:

```sh
gh workflow run release-resume.yml --ref v1.2.3 -f tag=v1.2.3
```

Two traps:

- **Cancelling the gate run does not stop `release.yml`** once it has been
  launched. Cancel that run too, from the Actions tab.
- **Starting the gate twice for the same version** makes the second run
  delete the first run's draft. Don't.

### Finishing a release with the resume workflow

`.github/workflows/release-resume.yml` is the second half of every release:
`sign-release.sh` starts it on the new tag. You start it yourself to
finish a release that already exists but whose resume run failed or never
ran. It runs the same branch, packages, Homebrew and notes steps. It never
builds or publishes a release. Start it **on the tag**, so the `release`
environment opens. In the browser, go to Actions, Release resume, Run
workflow, and pick the tag under "Use workflow from". Or run:

```sh
gh workflow run release-resume.yml --ref v1.2.3 -f tag=v1.2.3
```

It stops with an error if it runs on anything but that tag. It checks that
the tag is a release tag and that its GitHub Release exists and is
published. For a stable release it runs the packages and Homebrew steps
(after you approve `release`) and the notes. For a pre-release it only adds
the notes. It runs the scripts as they were at the tag. A fix to a release
script on `main` reaches the next release, not this one.

With `-f dry-run=true` it builds and signs the packages, for any release,
pre-releases too. It checks that the token may push to the packages repo,
but publishes nothing: no push, no Homebrew, no notes. The packages and the
updated repo are kept as artifacts of the run, for you to look at. Use it
on a pre-release to check the key and tokens before the first stable
release.

## Signing the release

Every file on a release gets a [minisign](https://jedisct1.github.io/minisign/)
signature, `FILE.minisig`, made with a key that never goes near GitHub. The
`.sha256` files are signed too. Someone who takes over the GitHub
organisation or a workflow can build a release, but can't sign it. The
reasons are in [Supply chain security](supply-chain.md). The one-off key
setup is in [The release signing key](release-signing-key.md).

`sign-release.sh` runs on your computer, with the key's USB stick plugged
in. It stops at the first check that fails:

1. The draft holds exactly the files a release should have.
2. Every checksum matches, and every file has an attestation from
   `release.yml`.
3. You confirm the list of files.
4. minisign signs all files in one go and asks for the password once. The
   trusted comment of every signature is `irsync vX.Y.Z`.
5. Every signature checks out against
   `packaging/signing/release-keys.pub`. A wrong key is caught here.
6. It uploads the `.minisig` files and checks that every file has one.
7. It checks that the tag still does not exist. Then it publishes the
   draft, which creates the tag, checks that the release is immutable and
   starts `release-resume.yml` on the tag. Then it re-runs the newest
   staging build of the site, so the site reads this release from its tag.
   If that cannot be started, for example because that run is still going,
   it prints the commands to do it by hand; the release is published either
   way. GitHub lets a run be re-run for 30 days. After that, a push to
   `main` that touches `site/` or `book/` rebuilds the site. It ends by printing the command that promotes the site to
   irsync.org; it does not run it.

Step 7 only happens with `--publish`. Without it, the script stops after
step 6 and the release stays a signed draft: look at it, then run the
script again with `--publish`. Publishing can't be undone, so it never
happens by accident.

`--key PATH` names the secret key file. Without it, minisign uses
`~/.minisign/minisign.key`.

**Where the key lives.** The secret key is a password-protected file on a
USB stick, and a copy is on a second stick kept somewhere else. The password
is in a password manager. Nothing of it is on GitHub. The public key is in
`packaging/signing/release-keys.pub`.

**A second key.** Another maintainer makes their own key, as in
[A second key](release-signing-key.md#a-second-key), and a pull request
adds their public key to `release-keys.pub` below the first one. A release
signed with either key passes. The new key can sign from the first release
that lists it.

**If signing fails.** Nothing is public until the last step, and the script
only uploads files to the draft.

- A check fails before the upload (files, checksums, attestations, the
  key): read the message, fix the cause and run the script again.
- Something fails after the upload but before the publish: run the script
  again. It replaces the signatures.
- You can't fix it, or the build itself is wrong: delete the draft
  (`gh release delete vX.Y.Z`) and start the gate again with the same
  version. It deletes a leftover draft anyway.
- The release is published, but the resume run did not start: start it by
  hand (see
  [Finishing a release with the resume workflow](#finishing-a-release-with-the-resume-workflow)).
  Never run the script on a published release: it refuses.

Once the release is published, its files can't change. A wrong signature
means a new version.

## Testing a version on FreeBSD

FreeBSD is not a release target yet. Until it is, one workflow checks a
version there by hand and gives you a tarball to try. It never blocks a
pull request or a release, and nothing starts it for you.

Start it with the tag or branch to test, then fetch the result:

```sh
gh workflow run freebsd.yml -f ref=v0.1.0-beta.4
gh run watch
gh run download <run-id> -n irsync-x86_64-unknown-freebsd
```

Without `-f ref=...` it tests the branch the run starts on, `main` by
default. The download holds `irsync-x86_64-unknown-freebsd.tar.xz`, laid out
like the release tarballs, and its `.sha256` file.

The run builds on FreeBSD 14: a binary built on 14 also runs on newer
releases, and not the other way round. `-f freebsd=15` picks another
release.

Inside the VM it runs `tools/ci.sh check freebsd-build` as an ordinary
user, with the Rust version in `rust-toolchain.toml`. That is the full
regression, then the release build. A failing test means no tarball.
Nothing is cached in the VM, so a run takes a while.

The limits:

- x86_64 only.
- A version from before this workflow existed can't be tested. The run
  uses the scripts of the version under test, and stops with a message
  when they are missing.
- The tarball is not on the release page, and it has no attestation.
- irsync does not upload itself to a FreeBSD server over ssh. Install the
  two programs there by hand.

## One-time setup

Do this once, before the first release.

**Repositories.** Create two public repositories in the `irsync-dev`
organisation. Create both with a README, so they have a `main` branch:

- `homebrew-tap`, for the Homebrew formula.
- `packages`, for the apt and dnf repo.

**GitHub Pages.** In the `packages` repository, turn on Pages and serve the
default branch. Set the custom domain to `dist.irsync.org` and turn on
"Enforce HTTPS". You don't fill the repository by hand. Each stable release
copies `index.html`, `irsync.repo` and `CNAME` from `packaging/repo/` into
it, and exports the public key as `irsync.asc` (for dnf) and `irsync.gpg`
(for apt).

**DNS.** Add a `CNAME` record that points `dist.irsync.org` at
`irsync-dev.github.io`. If the DNS is on Cloudflare, set the record to
"DNS only" (grey cloud), so GitHub can issue the HTTPS certificate.

**Signing key.** Make a GPG key with the user ID
`irsync releases <releases@irsync.org>`, step by step in
[Making the signing key](#making-the-signing-key). Set up the
`releases@irsync.org` mailbox too, so mail to that address reaches someone.
The workflow uses the fingerprint of the key it imports as `GPG_KEY_ID`, so
there is nothing else to set.

**Release signing key.** Make the minisign key that signs every file on a
release, step by step in
[The release signing key](release-signing-key.md). It stays offline, on a
USB stick, and never goes into GitHub.

**Tokens and secrets.** Make one fine-grained token per repository, and put
the secrets in the `release` environment, not the repository. Step by step
in [Supply chain security](supply-chain.md#maintainer-steps), steps 2 to 5:

| Secret (environment `release`) | What it holds |
| --- | --- |
| `PACKAGES_GPG_PRIVATE_KEY` | The signing **subkey** only: `private-subkey.asc` |
| `PACKAGES_GPG_PASSPHRASE` | The key's passphrase: `passphrase.txt`, with no newline at the end |
| `PACKAGES_REPO_TOKEN` | A token that can push to `irsync-dev/packages` and nothing else |
| `HOMEBREW_TAP_TOKEN` | A token that can push to `irsync-dev/homebrew-tap` and nothing else |

**Old `release-*` branches.** Before you apply the settings, delete every
remote branch that matches `release-*` and is not a `release-X.Y` release
branch. Examples are old PR branches like `release-v0.1.0-beta.3` and
`release-via-pr`. The `Release branches` ruleset would make them
undeletable. List them with:

```sh
git ls-remote --heads origin 'release-*'
```

and delete each one with `git push origin --delete <name>`.

**Repository settings.** `tools/github_settings.py apply` sets the
rulesets (including [the release tags](#protecting-the-release-tags) and
the release branches), the environments, secret scanning and immutable
releases. See [Checking the settings](supply-chain.md#checking-the-settings).

## Creating `release-X.Y` by hand

The resume run creates `release-X.Y` after the release is public. If it
fails, or the branch is missing, create it at the released commit:

```sh
git push origin vX.Y.Z:refs/heads/release-X.Y
```

The `Release branches` ruleset may refuse to create the ref. If it does, an
admin disables that ruleset for a moment, pushes the branch, and turns the
ruleset on again with `tools/github_settings.py apply`.

**Watch the first stable release.** The first `X.Y.0` is the first time the
resume run creates `release-X.Y`, and it runs after the release is
public. The `Release branches` ruleset may block it. If it does, create the
branch by hand as above, and fix the ruleset or the workflow.

**Watch the first patch release.** The first release from a `release-X.Y`
branch should be watched. The gate creates `pinned/vX.Y.Z` with the
workflow's built-in token, and GitHub may refuse that if the workflow files
on the branch differ from those on `main`. If it does, that is a bug to
fix. Don't work around it by pushing tags.

## The first release

Do the one-time setup first. Then:

1. **Merge to `main`.** (Done.) The gate can only launch `release.yml` once it is on
   the default branch, so the merge comes first. The install steps in the
   README and the book go live with it. They say that Homebrew, apt and dnf
   start with v0.1.0, and that `--alpha` works before that.
2. **Put the install script on irsync.org.** (Done.) The merge deploys the site,
   with `install.sh`, to staging. Check that
   `https://dev.irsync.org/install.sh` is there, then promote the site (see
   [Promoting to irsync.org](website.md#promoting-to-irsyncorg)).
3. **Rehearse with a pre-release.** Done: `0.1.0-alpha.1` was rehearsed
   this way, with the old flow. Its tag exists and it has no release, so
   the start check refuses that version now. The next rehearsal uses the
   next pre-release, for example `0.1.0-beta.4`. Follow
   [Cutting a release](#cutting-a-release) with that version, including the
   signing step. Publishing the draft creates the tag.

   Check that:
   - the gate passes and leaves a draft, and `sign-release.sh` publishes it
     and creates the tag `v0.1.0-beta.4`;
   - the GitHub Release is marked as a pre-release and has the tarballs, the
     checksums, the installer and a `.minisig` file for each of them, and
     they verify as in [Verify a download](../how-to/verify-a-download.md);
   - its notes end with `## What's changed`;
   - the install script with `--alpha` installs it, and without `--alpha`
     says there is no stable release yet;
   - after that install, `man irsync` shows the released version in its
     footer, and `man fileset` and `man irsync.toml` work;
   - `--uninstall` (with `--alpha`) removes the programs and the three
     pages;
   - nothing went to Homebrew, apt or dnf.

   If a Mac with Rosetta is at hand, also run the install script once in a
   Rosetta shell (`arch -x86_64 sh`). It should say it runs under Rosetta
   and install the Apple Silicon build. The tests only fake this, with a
   stand-in `sysctl`.

   Then install the pre-release's formula with real Homebrew, with the man
   page lines added the way `publish-homebrew.yml` adds them. It goes
   into a throwaway local tap, since Homebrew may refuse a formula file
   that is not in a tap. On a Mac with Homebrew, from a clone of this
   repository:

   ```sh
   gh release download v0.1.0-beta.4 --repo irsync-dev/irsync --pattern irsync.rb
   .github/scripts/formula-man-pages.sh irsync.rb
   brew tap-new --no-git local/rehearsal
   cp irsync.rb "$(brew --repository local/rehearsal)/Formula/"
   brew install local/rehearsal/irsync
   man -w irsync fileset irsync.toml
   brew uninstall irsync
   brew untap local/rehearsal
   rm irsync.rb
   ```

   `man -w` should print a path under Homebrew's `share/man` for each of
   the three pages.
4. **Dry-run the packages on the pre-release.** A pre-release skips
   Homebrew, apt and dnf, so the steps only stable releases run have not
   run yet. Once the rehearsal release from step 3 is published, run them
   without publishing:

   ```sh
   gh workflow run release-resume.yml --ref v0.1.0-beta.4 -f tag=v0.1.0-beta.4 -f dry-run=true
   ```

   This uses the GPG secrets, the packages repo token, nfpm and Ubuntu's
   rpm. Check that the run passes. Download its two artifacts and look at
   them: the `.deb` files list `/usr/bin/irsync` and `/usr/bin/fileset`
   and the three man pages under `/usr/share/man` (`dpkg-deb -c`), and the `.rpm` files are signed (`rpm -Kv`, after
   `rpm --import` of `irsync.asc` from the repo artifact).
5. **Check the Homebrew token.** Nothing has used `HOMEBREW_TAP_TOKEN` yet.
   Push a throwaway branch to the tap with it, then delete the branch. When
   git asks, give any user name and the token as the password. The
   `credential.helper=` setting stops git from using your own login instead:

   ```sh
   git clone https://github.com/irsync-dev/homebrew-tap
   cd homebrew-tap
   git -c credential.helper= push origin HEAD:refs/heads/token-check
   git -c credential.helper= push origin --delete token-check
   ```

6. **Cut v0.1.0.** Follow [Cutting a release](#cutting-a-release) with the
   version `0.1.0`. Then check the Homebrew formula,
   the apt and dnf repo at `dist.irsync.org`, and the signed `.rpm` files on
   the release.

## The packages repo grows

Every release adds a commit with new package files to `irsync-dev/packages`.
Pruning removes old files from the tree, but git keeps them in the history,
so the repository keeps growing. Pages serves only the current tree, so the
history is not needed. Once in a while, when no release is running, replace
it with a single commit:

```sh
git clone https://github.com/irsync-dev/packages
cd packages
git checkout --orphan fresh
git commit -m "Packages, history squashed"
git push --force origin fresh:main
```

If `main` is protected against force pushes, allow it for this push.

## Protecting the release tags

Release tags are created by GitHub when `sign-release.sh` publishes the
draft release. That is not a push. **Never push a `v*` tag yourself.** The
`Release tags` ruleset, set by `tools/github_settings.py`, makes every `v*`
tag permanent once it exists:

| Rule | What it stops |
| --- | --- |
| Restrict updates | Moving an existing tag to another commit |
| Restrict deletions | Deleting a tag |
| Block force pushes | Overwriting a tag with `git push --force` |

Nobody can bypass it, admins included. Creating tags is allowed, and
publishing a draft creates one, so the ruleset doesn't get in the way. A
stable release still publishes nothing beyond the GitHub Release until a
maintainer approves the `release` environment.

A tag pushed by hand has no GitHub Release. It is a stray tag. It can't be
deleted, and it uses up its number. The gate checks for stray tags and
stops every release until you have dealt with them: release a later
version. The two old alpha tags, `v0.1.0-alpha.1` and `v0.1.0-alpha.2`, were
pushed before releases came with the tag. They are the only ones the check
accepts without a release.

Two more rulesets protect the branches a release runs on:

- `Release branches` covers `release-*`. Like `main`, a branch there takes
  changes only through a pull request that passed `CI result`, and can't be
  deleted or force-pushed.
- `Pinned release commits` covers `pinned/*`. The gate creates and deletes
  these branches, but nobody can move one or force-push to it.

Check all of it, stray tags included, with:

```sh
tools/github_settings.py check
```

## Making the signing key

The key signs the apt and dnf repositories at `dist.irsync.org`. The steps
below make it, test it the way the workflow uses it, and leave you with the
files for the two GPG secrets. They take about ten minutes.

The key is RSA 4096, can only sign, has no subkeys and never expires:

- RSA works with every apt and rpm we care about, including older RHEL rpm
  versions that can't check newer key types.
- The workflow signs with the first secret key it finds, so one key with
  no subkeys is all it needs.
- apt users keep the public key in a file on their machine. If the key
  expired, `apt update` would fail for everyone until they downloaded it
  again. The revocation certificate covers a lost or leaked key instead.

The passphrase is long and random. Nobody types it; it lives only in the
secret and in your backup.

**1. A separate keyring.** Use a machine you trust, and keep the key out of
your everyday keyring. `GNUPGHOME` tells gpg which keyring to use, for this
terminal only, so run every later step in the same terminal:

```sh
umask 077
mkdir ~/irsync-signing-key
cd ~/irsync-signing-key
export GNUPGHOME="$PWD/gnupg"
mkdir -m 700 "$GNUPGHOME"
gpg --list-secret-keys    # prints nothing: the keyring is empty
```

**2. The passphrase.** 40 random characters, with no newline at the end.
The workflow uses the secret byte for byte, so a trailing newline would
become part of the passphrase and signing would fail:

```sh
gpg --gen-random --armor 1 30 | tr -d '\n' > passphrase.txt
wc -c passphrase.txt      # 40
```

**3. The key.** `rsa4096` is the key type, `sign` means it can only sign,
and `never` is the expiry date. `--pinentry-mode loopback` with
`--passphrase-file` takes the passphrase from the file instead of a
password prompt:

```sh
gpg --batch --pinentry-mode loopback --passphrase-file passphrase.txt \
    --quick-generate-key 'irsync releases <releases@irsync.org>' rsa4096 sign never
FPR=$(gpg --list-secret-keys --with-colons | awk -F: '$1 == "fpr" { print $10; exit }')
echo "$FPR"
gpg --list-keys
```

`FPR` is the fingerprint, the key's 40-character ID. `gpg --list-keys`
shows one `rsa4096` key with usage `[SC]`, no expiry, and the user ID
`irsync releases <releases@irsync.org>`.

**4. Export the files.**

```sh
gpg --batch --pinentry-mode loopback --passphrase-file passphrase.txt \
    --armor --export-secret-keys "$FPR" > private-key.asc
gpg --armor --export "$FPR" > public-key.asc
cp "$GNUPGHOME/openpgp-revocs.d/$FPR.rev" revocation.rev
head -1 private-key.asc   # -----BEGIN PGP PRIVATE KEY BLOCK-----
```

| File | What it is | Where it goes |
| --- | --- | --- |
| `private-key.asc` | The private key, still protected by the passphrase | Your backup only. GitHub gets a signing subkey, `private-subkey.asc`, made in [Supply chain security](supply-chain.md#maintainer-steps) step 2 |
| `passphrase.txt` | The passphrase | `PACKAGES_GPG_PASSPHRASE`, and your backup |
| `revocation.rev` | Declares the key dead, even without the private key | Your backup only |
| `public-key.asc` | The public key | Your records; every stable release publishes it as `irsync.asc` and `irsync.gpg` |

gpg made the revocation certificate together with the key. Anyone who has
it can kill the key, so it never goes into GitHub.

**5. Test the files the way the workflow uses them.** First make the
signing subkey ([Supply chain security](supply-chain.md#maintainer-steps)
step 2). The workflow imports `private-subkey.asc` into an empty keyring,
takes the first fingerprint it finds (the primary key's, whose secret part
isn't there) and signs with the passphrase, which uses the subkey. Do the
same in a second empty keyring:

```sh
export GNUPGHOME="$PWD/ci-test"
mkdir -m 700 "$GNUPGHOME"
gpg --batch --import private-subkey.asc
TEST_FPR=$(gpg --batch --list-secret-keys --with-colons | awk -F: '$1 == "fpr" { print $10; exit }')
[ "$TEST_FPR" = "$FPR" ] && echo "fingerprint matches"
echo test > test.txt
gpg --batch --yes --pinentry-mode loopback --passphrase-file passphrase.txt \
    --local-user "$TEST_FPR" --clearsign -o test.txt.asc test.txt
gpg --verify test.txt.asc
```

You should see `fingerprint matches` and `Good signature from "irsync
releases <releases@irsync.org>"`. A warning that the key is not certified
with a trusted signature is normal. Then remove the test keyring:

```sh
gpgconf --kill gpg-agent
rm -rf "$GNUPGHOME" test.txt test.txt.asc
export GNUPGHOME="$PWD/gnupg"
```

**6. The secrets.** Reading from a file keeps the passphrase out of your
shell history:

```sh
gh secret set PACKAGES_GPG_PRIVATE_KEY --env release --repo irsync-dev/irsync < private-subkey.asc
gh secret set PACKAGES_GPG_PASSPHRASE  --env release --repo irsync-dev/irsync < passphrase.txt
gh secret list --env release --repo irsync-dev/irsync
```

In the browser instead (Settings, Environments, `release`), paste the whole
of `private-subkey.asc`, including the
`BEGIN` and `END` lines, and the 40 passphrase characters with nothing
after them. GitHub never shows a secret again, so if you are unsure you
pasted it right, set it again.

**7. Back up, then clean up.** Keep `private-key.asc`, `passphrase.txt` and
`revocation.rev` somewhere safe outside GitHub, such as a password manager
that takes attachments or an encrypted USB stick. Write the fingerprint
(`echo "$FPR"`) next to them: it is how anyone checks that a downloaded
`irsync.asc` is the real key. Without the backup, losing the secret means a
new key, and every user has to import it again.

Once you have checked that you can open the backup, delete the working
folder:

```sh
gpgconf --kill gpg-agent
cd ~
rm -rf ~/irsync-signing-key
unset GNUPGHOME
```

**8. Check it on a real run.** An alpha doesn't sign anything, so the
first real use of the key is the dry run in
[The first release](#the-first-release), step 4. A wrong passphrase
secret fails there. After the first stable release, check that the
published key is yours:

```sh
curl -fsSL https://dist.irsync.org/irsync.asc | gpg --show-keys
```

The fingerprint it shows must match the one you wrote down.

**If the key leaks,** mark it revoked where users fetch it, then rotate to
a new key (below). In a throwaway keyring, import the public key and the
revocation certificate, and export the result:

```sh
gpg --import public-key.asc revocation.rev
gpg --armor --export "$FPR" > irsync.asc
gpg --export "$FPR" > irsync.gpg
```

Commit these two files to the `packages` repository by hand. Anyone who
fetches the key from then on gets it marked revoked, and the next stable
release replaces both files with the new key. Users who already have the
old key keep trusting it until they import the new one, so tell them
(rotation step 4). **If the key is lost** with no backup, make a new key
and rotate.

## Rotating the signing key

1. Make the new key, with the same user ID, as in
   [Making the signing key](#making-the-signing-key).
2. Put the new key's signing subkey and passphrase into the
   `PACKAGES_GPG_PRIVATE_KEY` and `PACKAGES_GPG_PASSPHRASE` secrets of the
   `release` environment
   ([Supply chain security](supply-chain.md#maintainer-steps), steps 2 and 4).
   If only the subkey leaked, replace the subkey instead and keep the key.
3. Cut a stable release. It signs the metadata with the new key and
   publishes the new public key as `irsync.asc` and `irsync.gpg`.
4. Tell users to import the new key again. For apt, that means downloading
   `irsync.gpg` again to `/etc/apt/keyrings/`. For dnf, the `.repo` file
   fetches the key from `irsync.asc`; dnf asks to trust the new one.

Old packages stay signed with the old key until they are pruned from the
repo (after 3 newer versions). Until then, users who have not re-imported the
key may see signature errors on those files.

## The user-facing install steps

The README, the book's introduction and the fileset introduction list the
ways to install, and what the install script does and how to uninstall.
Keep the three in step. The apt and dnf commands are in `packaging/repo/index.html`
(the page at `dist.irsync.org`). If you change them there, change them in
the docs too.
