#!/bin/bash
# Usage: bump-version.sh <version>
#
# Prepares a release commit: sets <version> (for example 1.2.3 or
# 1.2.3-beta.1, without a "v") in Cargo.toml ([workspace.package]) and in
# packaging/dist/dist.toml, updates Cargo.lock and fuzz/Cargo.lock to match,
# and commits the four files as "Release v<version>" on a new branch,
# bump-v<version>. The release notes are not its work: when CHANGELOG.md has
# no section for the version yet, it says so at the end.
#
# Run it on the branch the release is cut from: release-X.Y for a patch
# (X.Y.Z with Z > 0), main for everything else (release-branch.sh has the
# rule). That branch only takes changes through a pull request, so the script
# does not push and does not tag: open the pull request, merge it once "CI
# result" passes, then start the release gate on the base branch. The gate
# ends at a draft release; sign-release.sh signs it, and with --publish
# publishes it, which creates the tag. The script prints those commands at the end.
#
# Run it anywhere in the repo, with no uncommitted changes to tracked files.
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 <version>   (for example 1.2.3 or 1.2.3-beta.1)" >&2; exit 2; }
version=$1
tag=v$version
here=$(cd "$(dirname "$0")" && pwd)
check=$here/check-release-tag.sh
base_of=$here/release-branch.sh

cd "$(git rev-parse --show-toplevel)"

# Same rules as the release gate, so a version that passes here passes there.
"$check" "$tag" >/dev/null

if ! git diff --quiet HEAD --; then
    echo "error: there are uncommitted changes. Commit or stash them first." >&2
    exit 1
fi

if "$check" --versions "$tag" >/dev/null 2>&1; then
    echo "error: the version is already $version." >&2
    exit 1
fi

# The branch this release is cut from; the version is already valid here.
base=$("$base_of" "$version")
current=$(git branch --show-current)
if [ "$current" != "$base" ]; then
    echo "error: $version is released from $base, but you are on ${current:-a detached HEAD}." >&2
    echo "Run: git switch $base && git pull --ff-only" >&2
    exit 1
fi

branch="bump-$tag"
if git rev-parse --verify --quiet "refs/heads/$branch" >/dev/null; then
    echo "error: the branch $branch already exists." >&2
    exit 1
fi

# Replace the `version = "..."` line inside the given table of each file,
# leaving everything else (comments, layout, other tables) as it is.
python3 - "$version" Cargo.toml workspace.package packaging/dist/dist.toml package <<'EOF'
import re, sys
version, args = sys.argv[1], sys.argv[2:]
for path, table in zip(args[::2], args[1::2]):
    with open(path) as f:
        text = f.read()
    # The table header, then its lines up to the next header.
    pattern = (r'(^\[' + re.escape(table) + r'\][ \t]*\n(?:(?!\[).*\n)*?)'
               r'version = "[^"]*"[ \t]*$')
    text, n = re.subn(pattern, lambda m: m.group(1) + f'version = "{version}"',
                      text, count=1, flags=re.M)
    if n != 1:
        sys.exit(f"error: no version line in [{table}] of {path}")
    with open(path, "w") as f:
        f.write(text)
EOF

# Make sure the right keys changed: this is the gate's own check.
"$check" --versions "$tag" >/dev/null

# Write the new version into Cargo.lock without touching any other package.
cargo update --workspace --quiet
cargo metadata --locked --format-version 1 >/dev/null
# The fuzz targets are a project of their own, outside the workspace, with
# a lock file of their own. It records the workspace crates' versions too,
# and CI builds it with --locked. --workspace here takes in the crates it
# uses by path, and nothing from crates.io.
cargo update --manifest-path fuzz/Cargo.toml --workspace --quiet
cargo metadata --manifest-path fuzz/Cargo.toml --locked --format-version 1 >/dev/null

git switch --quiet -c "$branch"
git commit --quiet -m "Release $tag" -- Cargo.toml Cargo.lock fuzz/Cargo.lock packaging/dist/dist.toml
git log --oneline -1

# The gate refuses a release without its notes, and so does a test in CI.
notes=
if ! "$here/changelog-section.sh" "$version" >/dev/null 2>&1; then
    notes="  # write the release notes in CHANGELOG.md, under the heading '## $tag'
  # (AGENTS.md, Release notes, says how), then:
  git commit -m 'Release notes for $tag' -- CHANGELOG.md
"
fi

cat <<EOF

Next:
$notes  git push -u origin $branch
  gh pr create --fill --base $base
  # once "CI result" passes:
  gh pr merge --merge
  gh workflow run release-gate.yml --ref $base -f version=$version
  # once the gate has made the draft: sign it, check it on GitHub, then publish:
  .github/scripts/sign-release.sh $version
  .github/scripts/sign-release.sh --publish $version
EOF
