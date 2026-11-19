#!/bin/bash
# Usage: release-yml.sh write|check
#
# Keeps .github/workflows/release.yml. dist writes it, with one change: the
# release is made as a draft (`gh release create --draft`), so a maintainer
# can sign the files before anyone sees them.
#
# dist-workspace.toml has allow-dirty = ["ci"], so dist no longer manages
# release.yml, and `dist generate --check` ignores it. This script does that
# job instead. It takes the dist config without the allow-dirty line, runs
# `dist generate --mode=ci`, and adds --draft.
#
#   write  regenerate release.yml
#   check  fail, with a diff, if release.yml is not what write would make
#
# Never edit release.yml by hand. Run this script from anywhere in the repo.
set -euo pipefail

# shellcheck disable=SC2015 # usage only runs when the test is false
[ $# -eq 1 ] && { [ "$1" = write ] || [ "$1" = check ]; } ||
    { echo "usage: $0 write|check" >&2; exit 2; }
mode=$1

cd "$(git rev-parse --show-toplevel)"
config=dist-workspace.toml
workflow=.github/workflows/release.yml

tmp=$(mktemp -d)
# dist cannot be pointed at another config, so it works on the real files.
# Put them back however this script ends.
cp "$config" "$tmp/config"
cp "$workflow" "$tmp/workflow"
restore() {
    cp "$tmp/config" "$config"
    cp "$tmp/workflow" "$workflow"
}
trap 'restore; rm -rf "$tmp"' EXIT

grep -v '^allow-dirty = ' "$tmp/config" >"$config"
dist generate --mode=ci >/dev/null

found=$(grep -c 'gh release create' "$workflow" || true)
if [ "$found" -ne 1 ]; then
    echo "release-yml.sh: expected one 'gh release create' line in what dist wrote, found $found; did dist change its template?" >&2
    exit 1
fi
# Not sed -i: BSD and GNU sed take its argument differently.
sed 's/\(gh release create .*\) artifacts\/\*/\1 --draft artifacts\/*/' "$workflow" >"$tmp/draft"
cp "$tmp/draft" "$workflow"
if ! grep 'gh release create' "$workflow" | grep -q -- ' --draft artifacts/\*'; then
    echo "release-yml.sh: could not add --draft before artifacts/*; did dist change its template?" >&2
    exit 1
fi

cp "$workflow" "$tmp/generated"
restore
if [ "$mode" = write ]; then
    cp "$tmp/generated" "$workflow"
    # The trap restores from these copies; make the new file the one it keeps.
    cp "$tmp/generated" "$tmp/workflow"
    echo "release-yml.sh: wrote $workflow"
elif ! diff -u "$workflow" "$tmp/generated"; then
    echo "release-yml.sh: $workflow is not what dist makes; run $0 write" >&2
    exit 1
fi
