# A release branch for every release line

## Why

Every release, stable, beta or alpha, should come from a branch
`release-X.Y` on GitHub, so that:

- the book of each release can link to the branch it came from (the next
  piece of work: the book's title shows where it was built from, with a
  link; see "Later" below);
- fixes can be copied from `main` into a release line (cherry-picked) when
  needed, also before its stable release. The machinery for that goes in
  now, though no such commits exist yet.

Today only `X.Y.0` creates `release-X.Y` (in `release-resume.yml`, after
the release is public), and only patches `X.Y.Z` (Z > 0) are cut from it.
Alphas, betas and `X.Y.0` are cut from `main`. The current releases
(`0.1.0-alpha.4`, `0.1.0-beta.6`) have no branch.

## The life cycle of one release line (decided)

One branch `release-X.Y` holds every release of `X.Y`:

```
release-1.3:  1.3.0-alpha.1 → alpha.2 → … → 1.3.0-beta.1 → beta.2 → … → 1.3.0 → 1.3.1 → …
```

- The first release of `X.Y` creates the branch.
- Order, checked by the release gate before anything is built:
  - an alpha follows a lower alpha (or starts the line);
  - a beta follows an alpha or a lower beta;
  - a stable follows an alpha, a beta or a lower stable.
  - Anything else (an alpha after a beta, any pre-release after a stable, a
    lower number than one already released) is refused.
- A line may start with a beta or a stable. That is allowed, with a
  warning (a GitHub `::warning::` on the gate's run) that it is not the
  usual order.
- Pre-releases are always of `X.Y.0` (`1.3.0-beta.2`). A pre-release of a
  patch (`1.3.1-beta.1`) is refused: after a stable only higher stables
  follow. (My call, from the order above.)

## How work reaches the branch (decided)

- **While `main` is on the line and its `X.Y.0` is not out** (alphas,
  betas, and `X.Y.0` itself), each release first **rebases `release-X.Y`
  onto `main`**: the branch's
  own commits, if any, are replayed on top of the current `main`. Today the
  branch has none, so the rebase just moves the branch to `main`. Commits
  added to the branch later (a fix not wanted on `main` yet, a cherry-pick)
  are carried along by the rebase. Including `X.Y.0` in the rebasing phase
  is my call: it is cut from `main` today.
- **After `X.Y.0`** the branch is no longer rebased. Patches land on it
  through pull requests (cherry-picks from `main`), as today.
- **Lines overlap (decided).** `main` may move on to the next line
  (`1.4.0-alpha.1`) before `1.3.0` is out. From the moment `main`'s version
  (`[workspace.package]` in `Cargo.toml`) is no longer `X.Y.*`, `release-X.Y`
  is no longer rebased: its remaining betas and `X.Y.0` are bumped with a
  pull request to `release-X.Y`, like a patch, and fixes come over by
  cherry-pick. So the branch a bump goes to is: `main` for a pre-release or
  `X.Y.0` while `main` is on `X.Y`; `release-X.Y` otherwise (and for every
  patch). `release-branch.sh` keeps that rule in one place; it now needs
  `main`'s version, which it reads from the checkout or from GitHub.
- **When the rebasing stops (my call).** Besides the two conditions above
  (`main` left the line, or the tag `vX.Y.0` exists), the preparing step
  refuses while a draft release or a `pinned/vX.Y.*` branch of the line
  exists, so a release half-way out is never rebased under. It shares a
  concurrency group per line with the gate.
- **Every release is cut from `release-X.Y`.** The gate always runs on the
  release branch, tests its tip, pins it and releases it.

## Approaches

1. **Fast-forward only, refuse own commits for now.** Smallest change; the
   released commit is the tested one on `main`; no force-push. Rejected by
   the user: the machinery for commits on the branch should be in place now.
2. **A real rebase (chosen).** Handles commits on the branch from the start.
   Costs a force-push to a protected branch, a rebase that can stop on a
   conflict, and a released commit that may differ from `main`'s (the gate's
   full test run covers it).

## The flow, step by step

For an alpha, a beta or `X.Y.0`:

1. Bump the version on `main` with a pull request, as today
   (`bump-version.sh`, which prints the next commands).
2. **New step:** start the workflow that prepares the branch,
   `release-branch.yml` (name left to the builder), on `main` with the
   version. It checks the version's order on the line (as the gate does),
   then creates `release-X.Y` at `main`'s tip, or rebases it onto `main`
   and force-pushes it. On a rebase conflict it stops, changes nothing, and
   says how to resolve it (below).
3. Start the gate on `release-X.Y`, as for a patch today. It tests, pins
   and releases the branch tip, as today.

**A rebase that stops on a conflict (decided).** Only the preparing
workflow ever force-pushes `release-X.Y`; no person gets a bypass. The
maintainer rebases by hand into a branch of their own (any name outside
`release-*`), resolves the conflict, pushes that branch, and starts the
preparing workflow again with an extra input naming it. The workflow
checks that branch and, if it passes, force-pushes it as `release-X.Y`
and deletes the input branch. The checks: it sits directly on `main`'s
current tip, and its commits after `main` are the old `release-X.Y`'s own
commits, in order, by subject and author (not content: resolving the
conflict changes content). A commit that the rebase dropped because `main`
already has it may be missing; nothing may be added. The guide shows the
commands.

For a release on a line `main` has left: bump with a pull request to
`release-X.Y`, then start the gate on `release-X.Y` (no preparing step;
the gate's order check still applies).

For a patch `X.Y.Z` (Z > 0): unchanged. Bump with a pull request to
`release-X.Y`, start the gate on `release-X.Y`.

Why a separate step rather than a rebase inside the gate: the gate tests
`GITHUB_SHA`, the commit it was started on, and pins exactly that commit.
A rebase inside the gate would make a new commit the run never tested.
With the branch prepared first, the gate stays as it is: tested commit =
released commit.

## Constraints

- **The branch rule.** `Release branches` (`tools/github_settings.py`)
  protects `release-*`: changes only through a pull request with `CI
  result`, no deletion, no force-push. The preparing workflow must create,
  update and force-push `release-X.Y` anyway. That needs a narrow exception
  for that workflow alone, not a weaker rule for everyone. My proposal: a
  ruleset bypass for a dedicated identity (a deploy key or a GitHub App)
  whose secret lives in a new environment that only `main` can use and
  that needs the maintainer's approval, like `release`. The builder checks
  what GitHub allows and picks the narrowest working option; the token
  `GITHUB_TOKEN` cannot be given a bypass. `github_settings.py` describes
  and checks the result.
- **Only the preparing workflow rebases and force-pushes**, and only while
  rebasing is allowed (above). Otherwise it refuses.
- **No release without the gate's tests**, as today. The rebased tip is
  tested by the gate's full run (`ci.yml` via `workflow_call`), and
  `ci.yml` also runs on the push to `release-*`.
- **The gate refuses a release whose order is wrong**, also when the
  preparing step was skipped: the order check lives in one script, used by
  both.
- **`release-resume.yml` no longer creates branches**: the branch exists
  before the gate runs. Its `new-branch` output and job go.
- **`site/tools/versions.py`** reads the version at the tip of every
  `release-X.Y` as a release being built. A branch rebased onto a bumped
  `main` shows that version until it is tagged; that is correct. Its tests
  stay green.
- The existing tests (`.github/scripts/tests/test_release_scripts.py`,
  `test_workflow_policy.py`, `tools/tests/test_github_settings.py`) are
  updated for the new rules; the fake `gh` in `fake_gh.py` grows what the
  new checks call.

## Moving to this

- Create `release-0.1` at `v0.1.0-beta.6` once, by hand (the guide's
  "Creating `release-X.Y` by hand" section, updated). The next 0.1 release
  then follows the rules above (a higher beta or `0.1.0`).
- Apply the new branch rule settings with `tools/github_settings.py apply`.

## Documentation

`book/src/contributing/releasing.md`: "Where a release is cut from",
"Cutting a release" (the new step), the order rules and the warning,
copying a fix into a release line (before and after `X.Y.0`, and once
`main` has left the line), resolving a rebase conflict, and the setup of
the new identity and environment. The rule "Only the newest `release-X.Y`
branch gets fixes" becomes: lines still before their `X.Y.0`, and the
newest stable line (my call).
`bump-version.sh` prints the new sequence of commands.

## Non-goals

- Deleting old release branches (alpha-only lines and the like): later.
  The `Release branches` rule forbids deletion, so that work will need its
  own exception.
- The book's title linking to the branch: the next piece of work, after
  this one.
- Rebasing after `X.Y.0`; releasing from anything but `release-X.Y`.

## Left to the builder

- The preparing workflow's name and the split of its logic into scripts.
- The exact bypass identity, within the constraint above.
- How the order check reads earlier releases (tags `vX.Y.*` on GitHub, as
  the other checks do).

## Later

The book's title: in the Dev book, `irsync Documentation (irsync@SHORTSHA)`
linking to that commit; in the release books, a link to `release-X.Y`.
