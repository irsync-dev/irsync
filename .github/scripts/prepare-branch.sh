#!/bin/bash
# Usage: prepare-branch.sh <line> <main-sha> [<fixed-branch>]
#
# Creates the branch release-<line> (for example release-1.3) at main, or
# rebases it onto main, on GitHub. release-prepare.yml runs it after
# prepare-check.sh, in a full clone whose origin it may push to (the deploy
# key); the "Release branches" rule lets only that key past. <main-sha> is
# the commit of main that prepare-check.sh checked: if main has moved since
# (the approval in between can take hours), nothing is pushed.
#
# - No release-X.Y yet: it is created at main.
# - Otherwise its own commits (those not on main) are rebased onto main, and
#   the result is pushed with a lease on the old tip: if anyone moved the
#   branch in the meantime, nothing is pushed. Commits main already has are
#   dropped by the rebase.
# - On a conflict nothing is pushed. The error says how to rebase by hand into
#   a branch of one's own and to hand that in as <fixed-branch>.
# - With <fixed-branch>: it must start at main, have no merges, and hold the
#   same commits as release-X.Y's own (by author and subject, in order; some
#   may be left out, none added). Then it becomes release-X.Y, with the same
#   lease, and the fixed branch is deleted.
#
# Never pushes without a lease.
set -euo pipefail

[ $# -eq 2 ] || [ $# -eq 3 ] || { echo "usage: $0 <line> <main-sha> [<fixed-branch>]" >&2; exit 2; }
line=$1
checked=$2
fixed=${3:-}

fail() {
    echo "::error::$1" >&2
    exit 1
}

[[ $line =~ ^[0-9]+\.[0-9]+$ ]] || fail "'$line' is not a release line such as 1.3."
[[ $checked =~ ^[0-9a-f]{40}$ ]] || fail "'$checked' is not a full commit SHA."
if [ -n "$fixed" ]; then
    case $fixed in
        main | release-* | pinned/* | -*) fail "The fixed branch can't be $fixed." ;;
    esac
    [[ $fixed =~ ^[A-Za-z0-9][A-Za-z0-9._/-]*$ ]] || fail "The fixed branch name '$fixed' has odd characters."
fi
branch=release-$line

git fetch --quiet --prune origin '+refs/heads/*:refs/remotes/origin/*'
main=$(git rev-parse --verify origin/main)
if [ "$main" != "$checked" ]; then
    fail "main moved since the checks ran (they saw $checked, main is now $main). Nothing was pushed. Start the workflow again."
fi

if ! old=$(git rev-parse --verify --quiet "refs/remotes/origin/$branch"); then
    if [ -n "$fixed" ]; then
        fail "$branch does not exist, so there is nothing to fix. Start the workflow again without fixed-branch."
    fi
    git push --quiet origin "$main:refs/heads/$branch"
    echo "created $branch at $main"
    exit 0
fi

lease_push() {
    git push --quiet "--force-with-lease=refs/heads/$branch:$old" origin "$1:refs/heads/$branch" ||
        fail "Could not push $branch. If someone moved it since this run read it ($old), start the workflow again."
}

if [ -n "$fixed" ]; then
    new=$(git rev-parse --verify --quiet "refs/remotes/origin/$fixed") ||
        fail "The fixed branch $fixed does not exist on GitHub."
    git merge-base --is-ancestor "$main" "$new" ||
        fail "$fixed does not start at the current main ($main). Rebase it onto origin/main again and push it."
    merges=$(git rev-list --merges "$main..$new")
    [ -z "$merges" ] || fail "$fixed has merge commits ($merges). Rebase it without merges."
    # Every commit of the fixed branch must be one of release-X.Y's own, in
    # the same order; a commit main already has may be left out.
    lists=$(mktemp -d)
    trap 'rm -rf "$lists"' EXIT
    git log --reverse --format='%an%x09%s' "$main..$new" >"$lists/fixed"
    git log --reverse --format='%an%x09%s' "$main..$old" >"$lists/own"
    python3 - "$lists/fixed" "$lists/own" <<'EOF' ||
import sys
fixed, own = (open(p).read().splitlines() for p in sys.argv[1:3])
it = iter(own)
sys.exit(0 if all(c in it for c in fixed) else 1)
EOF
        fail "$fixed has commits that are not $branch's own (by author and subject, in order). It may only hold $branch's commits, rebased onto main."
    lease_push "$new"
    git push --quiet origin --delete "$fixed"
    echo "replaced $branch with $fixed: $old -> $new"
    exit 0
fi

work=$(mktemp -d)
trap 'git worktree remove --force "$work" >/dev/null 2>&1 || true; rm -rf "$work"' EXIT
git worktree add --quiet --detach "$work" "$old"
if ! GIT_COMMITTER_NAME="irsync release bot" GIT_COMMITTER_EMAIL="release-bot@irsync.org" \
        git -C "$work" -c core.hooksPath=/dev/null rebase --quiet "$main" >/dev/null 2>&1; then
    files=$(git -C "$work" diff --name-only --diff-filter=U | tr '\n' ' ')
    git -C "$work" rebase --abort
    fail "Rebasing $branch onto main ($main) conflicts in: ${files% }. Nothing was pushed. Rebase it by hand into a branch of your own, then hand that in:
  git fetch origin
  git switch -c fix-$line origin/$branch
  git rebase origin/main      # resolve each conflict, git add, git rebase --continue
  git push origin fix-$line
  gh workflow run release-prepare.yml --ref main -f version=VERSION -f fixed-branch=fix-$line"
fi
new=$(git -C "$work" rev-parse HEAD)
lease_push "$new"
echo "rebased $branch onto $main: $old -> $new"
