#!/bin/sh
# Build the site: landing page at / and the book at /book/.
# Usage: site/build.sh [OUT]   (default: target/site)
set -eu

root=$(cd "$(dirname "$0")/.." && pwd -P)
out=${1:-"$root/target/site"}

# OUT is emptied below, so it must be a directory under target/.
mkdir -p "$root/target"
base=$(basename "$out")
parent=$(cd "$(dirname "$out")" 2>/dev/null && pwd -P) || {
  echo "refusing to build into '$out': its parent directory does not exist" >&2
  exit 1
}
case $parent/$base in
  "$root"/target/*) ;;
  *) echo "refusing to build into '$out': not under $root/target/" >&2; exit 1 ;;
esac
case $base in
  . | ..) echo "refusing to build into '$out': not under $root/target/" >&2; exit 1 ;;
esac
out=$parent/$base

if [ "$(mdbook --version)" != "mdbook v0.5.2" ]; then
  echo "warning: built and tested with mdbook v0.5.2, found $(mdbook --version)" >&2
fi

tokens() { python3 "$root/site/tools/gen_tokens.py" "$root/site/tokens.json" "$@"; }

# The book takes its tokens from the theme class mdbook puts on <html>.
tokens --light "html.light, html.rust" --dark "html.coal, html.navy, html.ayu" \
  -o "$root/book/theme/tokens.css"

# Start from an empty output, so files removed from site/ do not linger.
rm -rf "$out"
mkdir -p "$out"
mdbook build "$root/book" -d "$out/book"

# Landing page files; the tools, the token source and the scripts stay behind.
for f in "$root"/site/*; do
  case $f in
    "$root"/site/tools | "$root"/site/tokens.json | "$root"/site/*.sh) ;;
    *) cp -R "$f" "$out/" ;;
  esac
done

# The landing page takes its tokens from a data-theme attribute.
tokens --light "[data-theme=light]" --dark "[data-theme=dark]" -o "$out/tokens.css"
