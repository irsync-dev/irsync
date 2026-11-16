#!/bin/sh
# Builds the man pages irsync(1), fileset(1) and irsync.toml(5) from the
# book's Markdown with pandoc.
#
# Usage: tools/build-man.sh OUTDIR
#
# Writes OUTDIR/man1/irsync.1, OUTDIR/man1/fileset.1 and
# OUTDIR/man5/irsync.toml.5, and fails on any pandoc warning. Each page is a
# header from packaging/man (NAME, SEE ALSO and what else the book does not
# say) around one or more book chapters; packaging/man/filter.lua turns the
# chapters into man sections, and packaging/man/normalize.awk makes the
# result the same from an old pandoc as from a new one.
#
# The footer names the version in packaging/dist/dist.toml. The date is that
# of the last commit, or IRSYNC_MAN_DATE=YYYY-MM-DD when set (for tests and
# for builds outside a git checkout), so two builds of one commit match.
set -eu

if [ $# -ne 1 ]; then
    echo "usage: $0 OUTDIR" >&2
    exit 2
fi
out=$1
root=$(cd "$(dirname "$0")/.." && pwd -P)
man=$root/packaging/man
src=book/src

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/packaging/dist/dist.toml")
if [ -z "$version" ]; then
    echo "build-man.sh: no version found in packaging/dist/dist.toml" >&2
    exit 1
fi
if [ -n "${IRSYNC_MAN_DATE:-}" ]; then
    date=$IRSYNC_MAN_DATE
elif ! date=$(git -C "$root" log -1 --format=%cs 2>/dev/null) || [ -z "$date" ]; then
    echo "build-man.sh: not a git checkout; set IRSYNC_MAN_DATE=YYYY-MM-DD" >&2
    exit 1
fi
case $date in
    [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]) ;;
    *) echo "build-man.sh: bad date '$date', want YYYY-MM-DD" >&2; exit 1 ;;
esac

# One line per page: file, section, title, header, chapters under book/src.
# packaging/man/filter.lua knows each chapter too.
pages="irsync.1 1 IRSYNC irsync.md reference/command-line.md reference/management.md reference/remote-addresses.md reference/files-and-state.md reference/limitations.md
fileset.1 1 FILESET fileset.md fileset/reference.md
irsync.toml.5 5 IRSYNC.TOML irsync.toml.md reference/configuration.md"
# A change to one of these chapters or to packaging/man/ must run CI even
# when it is only Markdown: keep .github/scripts/code-changed.sh in step.

# mdbook expands {{#include}} and friends; pandoc would print them as they
# are. Check every input before writing anything.
failed=false
for f in "$man"/*.md $(echo "$pages" | cut -d' ' -f5-); do
    case $f in /*) ;; *) f=$root/$src/$f ;; esac
    if grep -n '{{#' "$f" >/dev/null; then
        grep -n '{{#' "$f" | sed "s|^|build-man.sh: ${f#"$root"/}:|" >&2
        failed=true
    fi
done
if $failed; then
    echo "build-man.sh: mdbook directives ({{#...}}) are not expanded in man pages;" \
        "teach packaging/man/filter.lua to expand them" >&2
    exit 1
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "$pages" | while read -r file section title header chapters; do
    marker='<!-- chapters -->'
    if [ "$(grep -c -x "$marker" "$man/$header")" != 1 ]; then
        echo "build-man.sh: packaging/man/$header needs one line '$marker'" >&2
        exit 1
    fi
    {
        sed "/^$marker\$/,\$d" "$man/$header"
        for c in $chapters; do
            printf '\n<!-- chapter: %s -->\n\n' "$src/$c"
            cat "$root/$src/$c"
        done
        printf '\n<!-- end of chapters -->\n\n'
        sed "1,/^$marker\$/d" "$man/$header"
    } >"$tmp/$file.md"
    pandoc --from gfm --to man --standalone --fail-if-warnings \
        --lua-filter "$man/filter.lua" \
        -V title="$title" -V section="$section" -V footer="irsync $version" \
        -V date="$date" -V header="irsync manual" \
        -o "$tmp/$file" "$tmp/$file.md"
    # Old and new pandoc write fonts and code blocks differently; this makes
    # the page the same from both.
    mkdir -p "$out/man$section"
    awk -f "$man/normalize.awk" "$tmp/$file" >"$out/man$section/$file"
done
