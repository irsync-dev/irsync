#!/bin/bash
# Usage: code-changed.sh <base-sha> <head-sha>
#
# Prints "code=true" if the change from <base> to <head> touches anything CI
# has to build and test, "code=false" if it only touches Markdown files,
# book/ or site/ (the site workflow checks those). The exception is what the
# man pages are built from: the book chapters and packaging/man/ that
# tools/build-man.sh reads and CI checks. Ready for $GITHUB_OUTPUT.
#
# An all-zero or unknown <base> (a new branch, a shallow clone) prints
# "code=true": when in doubt, test.
set -euo pipefail

[ $# -eq 2 ] || { echo "usage: $0 <base-sha> <head-sha>" >&2; exit 2; }
base=$1
head=$2

if [[ $base =~ ^0+$ ]] || ! git cat-file -e "$base^{commit}" 2>/dev/null; then
    echo "code=true"
    exit 0
fi

while IFS= read -r path; do
    case $path in
        # The inputs of tools/build-man.sh. Keep this list and the one there
        # together.
        book/src/reference/command-line.md | book/src/reference/management.md | \
            book/src/reference/remote-addresses.md | \
            book/src/reference/files-and-state.md | \
            book/src/reference/limitations.md | \
            book/src/reference/configuration.md | book/src/fileset/reference.md | \
            packaging/man/*)
            echo "code=true"; exit 0 ;;
        *.md | book/* | site/*) ;;
        *) echo "code=true"; exit 0 ;;
    esac
# --no-renames: a file moved from crates/ to docs/ is a change to both paths.
# base...head: only what the change itself did, not newer commits on main.
done < <(git diff --no-renames --name-only "$base...$head")
echo "code=false"
