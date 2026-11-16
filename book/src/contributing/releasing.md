# Releasing

You start a release by hand, with one command. The rest is done by GitHub
Actions. The release gate tests the commit, and the release workflow creates
the tag together with the GitHub Release, as its last step. You never push a
release tag. This page says how to cut a release, what the workflows do, what
to do when one fails, and what to set up once.

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
  `release-X.Y`. The gate creates that branch when it releases `X.Y.0`.

`main` and every `release-X.Y` branch only take changes through a pull
request that passed the `CI result` check. The gate refuses to run on the
wrong branch.

## Cutting a release

The three cases differ only in the branch. Steps 1 to 4 are the same.

1. Switch to the branch the release is cut from, up to date and with no
   uncommitted changes: `main` for a pre-release or `X.Y.0`, `release-X.Y`
   for a patch. Run the bump script with the new version, without the `v`:

   ```sh
   git switch main && git pull --ff-only     # or release-1.2
   .github/scripts/bump-version.sh 1.2.3
   ```

   It stops if you are on the wrong branch. It sets the version in
   `Cargo.toml` (`[workspace.package]`) and `packaging/dist/dist.toml`, and
   updates `Cargo.lock` to match. It commits the three files as
   `Release v1.2.3` on a new branch, `bump-v1.2.3`. It doesn't push or
   tag. It prints the commands for the next steps.
2. Open a pull request against the branch the release is cut from. Use
   `--base`:

   ```sh
   git push -u origin bump-v1.2.3
   gh pr create --fill --base main           # or release-1.2
   ```

3. Wait for its `CI result` check to pass. That includes the `release build`
   jobs, which run the Linux release build the way the release will. Then
   merge it: `gh pr merge --merge`.
4. Start the release gate on that branch, with the version:

   ```sh
   gh workflow run release-gate.yml --ref main -f version=1.2.3
   ```

   For a patch, use `--ref release-1.2`. In the browser: Actions, Release
   gate, Run workflow, pick the branch, enter the version.

For a pre-release, use a version such as `1.2.3-beta.1` or `1.2.3-alpha.1`.
For `1.3.0`, the gate also creates the branch `release-1.3` at the released
commit.

Don't start the gate twice for the same version. A second run can cancel the
resume run the first one started, which waits for the first to end.

If you change the version by hand instead, keep all three files in step.
These checks catch a mismatch:

- A test (`crates/irsync/tests/release_version.rs`) fails if `Cargo.toml` and
  `dist.toml` differ.
- CI builds with `--locked`, so it fails if `Cargo.lock` is out of date. The
  release build uses `--locked` too, and would fail later.
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
   one you gave. It checks that the tag doesn't exist yet and that there are
   no stray release tags (see
   [Protecting the release tags](#protecting-the-release-tags)). For `X.Y.0`
   it checks that `release-X.Y` doesn't exist yet. For a patch it checks
   that `release-X.Y` does. This takes seconds.
2. **Test.** It runs `ci.yml` on the branch's commit, with the musl release
   targets added. If anything fails, it stops. Nothing is published, and no
   tag exists.
3. **Pin.** It makes a branch `pinned/vX.Y.Z` at the tested commit, so the
   release builds exactly that commit even if the branch moves on. It
   deletes a `pinned/vX.Y.Z` left over from an earlier failed attempt. It
   refuses to go on if a `release.yml` run from an earlier attempt is still
   going.
4. **Release.** It launches `release.yml` on the pinned branch and waits for
   it. `release.yml` builds `irsync` and `fileset` for Linux x86_64, Linux
   arm64 and macOS on Apple Silicon. Each tarball also holds the man pages
   `irsync(1)`, `fileset(1)` and `irsync.toml(5)`, built from the book with
   pandoc. It publishes the GitHub Release with the tarballs, checksums,
   our installer `irsync-installer.sh` and the Homebrew formula, each with
   an attestation. As its last step it creates the tag `vX.Y.Z`,
   together with the release. That is the point of no return: the release
   is immutable from then on, and no file on it can be replaced or added.
   The gate then deletes the `pinned/` branch.
5. **Branch** (`X.Y.0` only). It creates `release-X.Y` at the released
   commit. This step and the next one start together after the release;
   neither waits for the other.
6. **Resume.** It starts `.github/workflows/release-resume.yml` on the new
   tag. This run waits for the gate run to end. The rest happens there,
   because the secrets of the `release` environment open only for runs on a
   `v*` tag:
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
   - **Notes.** It adds GitHub's list of changes to the release notes, under
     the heading `## What's changed`. For a stable release the list starts
     at the previous stable release, so it covers the betas in between too.

The release tag is a lightweight tag made by GitHub, not an annotated one.

The install script `site/install.sh` (served as `irsync.org/install.sh`)
picks the newest release for its channel: stable by default, `--beta` for
betas and stable, `--alpha` for all. Other arguments go to the installer
`irsync-installer.sh` that `release.yml` attached to the release. It puts
the programs in `~/.local/bin` and the three man pages in
`~/.local/share/man/man1` and `man5`, and has no updater. It writes no
receipt and never edits the shell's startup files. If `~/.local/bin` is not
on `PATH`, it prints the line to add. `--uninstall` removes the two
programs, the three pages and an old receipt file. Before it places
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

`.github/workflows/release.yml` is made by `dist generate` (cargo dist
0.34.0). Never edit it by hand. The settings are in `dist-workspace.toml`,
and the package is described in `packaging/dist/dist.toml`. After you change
`dist-workspace.toml`, run:

```sh
dist generate
```

and commit the result.

The installer is not dist's. `dist-workspace.toml` lists
`irsync-installer.sh` under `extra-artifacts`, and dist builds it with
`packaging/installer/build-installer.sh`, which fills in the version and the
checksums of the three tarballs. The man pages get into the tarballs through
`packaging/dist/build.sh`, which runs `tools/build-man.sh`. The build
runners need pandoc, which `dist-workspace.toml` lists as a dependency.
`dist generate` needed no change to `release.yml` for this: the workflow
reads the runners' packages and the extra artifact from `dist plan` when it
runs.

`release.yml` only starts when the gate launches it. **Never launch it by
hand from the Actions tab.** That skips the tests.

## If a release fails

The tag is created last, so a failure before the GitHub Release exists
leaves no tag behind. Fix the cause and start the gate again with the same
version.

**The check failed.** Nothing ran. Read the error. Usually the version in
the two files does not match, or you are on the wrong branch, or the branch
`release-X.Y` is missing or already there. Fix it and start the gate again.
A stray tag is different: see
[Protecting the release tags](#protecting-the-release-tags).

**The test step failed.** Nothing was published, and no tag exists. If it
was a flaky test, use "Re-run failed jobs" on the gate run. If the failure
is real, fix it on the branch through a pull request, then start the gate
again with the same version.

**The release step failed.** Look at the `release.yml` run it launched. The
link is in the gate's log. If the gate could not find the run, look in the
Actions tab under the release workflow: it may still be running.

- If the GitHub Release was not created, nothing was published and there is
  no tag. Fix the cause, then start the gate again with the same version.
  A leftover `pinned/vX.Y.Z` branch is deleted by the next run. If a
  `release.yml` run is still going, the gate refuses to start: cancel that
  run first (`gh run cancel <id>`), or wait for it to end.
- If the GitHub Release was created, the release is public (and the tag
  exists), but the gate stopped before the branch, the packages, Homebrew
  and the notes. This happens when the gate lost track of the `release.yml`
  run. Finish the release with the resume workflow (see below). For
  `X.Y.0`, also create the branch `release-X.Y` at the released commit by
  hand, if it is missing (see
  [Creating `release-X.Y` by hand](#creating-release-xy-by-hand)).

**The packages or Homebrew step failed.** Fix the cause (often a token in
the `release` environment), then re-run that job from the resume run, or
start a new resume run. Both are safe to run again. If the packages repo
already has this version, the packages job builds nothing and changes
nothing there. If the tap already has the formula, the Homebrew job pushes
nothing.

**The notes step failed.** Re-run that job, or start a new resume run. It
does nothing if the heading is already there.

**The release is out, but packages, Homebrew or the notes did not happen.**
This is what you see if the gate or the resume run was cancelled. Start the
resume run by hand:

```sh
gh workflow run release-resume.yml --ref v1.2.3 -f tag=v1.2.3
```

Two traps:

- **Cancelling the gate run does not stop `release.yml`** once it has been
  launched. Cancel that run too, from the Actions tab.
- **Starting the gate twice for the same version** can cancel the first
  run's waiting resume run. Start the resume run by hand if that happens.

### Finishing a release with the resume workflow

`.github/workflows/release-resume.yml` is the second half of every release:
the gate starts it on the new tag. You start it yourself to finish a release
that already exists but whose resume run failed or never ran. It runs the
same packages, Homebrew and notes steps. It never builds or publishes a
release. Start it **on the tag**, so the `release` environment opens. In the
browser, go to Actions, Release resume, Run workflow, and pick the tag under
"Use workflow from". Or run:

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

The `branch` job creates `release-X.Y` after the release is public. If it
fails, or the branch is missing, create it at the released commit:

```sh
git push origin vX.Y.Z:refs/heads/release-X.Y
```

The `Release branches` ruleset may refuse to create the ref. If it does, an
admin disables that ruleset for a moment, pushes the branch, and turns the
ruleset on again with `tools/github_settings.py apply`.

**Watch the first stable release.** The first `X.Y.0` is the first time the
`branch` job creates `release-X.Y`, and it runs after the release is
public. The `Release branches` ruleset may block it. If it does, create the
branch by hand as above, and fix the ruleset or the job.

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
   [Cutting a release](#cutting-a-release) with that version. The gate
   creates the tag.

   Check that:
   - the gate passes and creates the tag `v0.1.0-beta.4`;
   - the GitHub Release is marked as a pre-release and has the tarballs, the
     checksums and the installer;
   - its notes end with `## What's changed`;
   - the install script with `--alpha` installs it, and without `--alpha`
     says there is no stable release yet;
   - after that install, `man irsync` shows the released version in its
     footer, and `man fileset` and `man irsync.toml` work;
   - `--uninstall` (with `--alpha`) removes the programs and the three
     pages;
   - nothing went to Homebrew, apt or dnf.
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

Release tags are created by `release.yml`, as the last step of a release,
together with the GitHub Release. **Never push a `v*` tag yourself.** The
`Release tags` ruleset, set by `tools/github_settings.py`, makes every `v*`
tag permanent once it exists:

| Rule | What it stops |
| --- | --- |
| Restrict updates | Moving an existing tag to another commit |
| Restrict deletions | Deleting a tag |
| Block force pushes | Overwriting a tag with `git push --force` |

Nobody can bypass it, admins included. Creating tags is allowed (the
release workflow needs that). A stable release still publishes nothing
beyond the GitHub Release until a maintainer approves the `release`
environment.

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
