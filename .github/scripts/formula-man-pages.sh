#!/bin/bash
# Usage: formula-man-pages.sh FORMULA.rb
#
# Adds two lines to the Homebrew formula that dist builds, so brew puts the
# man pages where `man` looks. dist's `install` moves the binaries out, then
# puts every leftover file in pkgshare, where `man` doesn't look. These lines
# go just before that step, with the same indentation:
#
#     man1.install Dir["man/man1/*.1"]
#     man5.install Dir["man/man5/*.5"]
#
# The file is edited in place. Running it again changes nothing. It fails
# (exit 1, file untouched) if only one of the two lines is there, or if the
# line it anchors on is missing or appears more than once: dist changed its template, so look at the formula.
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 FORMULA.rb" >&2; exit 2; }
f=$1
anchor='leftover_contents = Dir["*"] - doc_files'
man1='man1.install Dir["man/man1/*.1"]'
man5='man5.install Dir["man/man5/*.5"]'

has1=0
has5=0
grep -qE "^[[:space:]]*man1\.install Dir\[\"man/man1/\*\.1\"\][[:space:]]*$" "$f" && has1=1
grep -qE "^[[:space:]]*man5\.install Dir\[\"man/man5/\*\.5\"\][[:space:]]*$" "$f" && has5=1
if [ $((has1 + has5)) -eq 2 ]; then
    echo "$f already installs the man pages."
    exit 0
elif [ $((has1 + has5)) -eq 1 ]; then
    echo "::error::$f has only one of the two man page lines. Fix it by hand." >&2
    exit 1
fi

n=$(grep -cF "$anchor" "$f" || true)
if [ "$n" -ne 1 ]; then
    echo "::error::$f has $n lines with '$anchor', expected 1. Did dist change its formula template?" >&2
    exit 1
fi

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
awk -v anchor="$anchor" -v m1="$man1" -v m5="$man5" '
    index($0, anchor) {
        match($0, /^[ \t]*/)
        indent = substr($0, 1, RLENGTH)
        print indent m1
        print indent m5
    }
    { print }
' "$f" > "$tmp"
cat "$tmp" > "$f"
echo "Added the man page lines to $f."
