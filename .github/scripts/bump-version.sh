#!/bin/bash
# Usage: bump-version.sh <version>
#
# Prepares a release commit: sets <version> (for example 1.2.3 or
# 1.2.3-beta.1, without a "v") in Cargo.toml ([workspace.package]) and in
# packaging/dist/dist.toml, updates Cargo.lock to match, and commits the three
# files as "Release v<version>". It does not push or tag: push to main, wait
# for CI to go green, then tag. The script prints those commands at the end.
#
# Run it anywhere in the repo, with no uncommitted changes to tracked files.
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 <version>   (for example 1.2.3 or 1.2.3-beta.1)" >&2; exit 2; }
version=$1
tag=v$version
here=$(cd "$(dirname "$0")" && pwd)
check=$here/check-release-tag.sh

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

git commit --quiet -m "Release $tag" -- Cargo.toml Cargo.lock packaging/dist/dist.toml
git log --oneline -1

cat <<EOF

Next:
  git push origin main
  # wait for CI on main to go green, then:
  git tag -a $tag -m "irsync $version"
  git push origin $tag
EOF
