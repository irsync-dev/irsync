#!/bin/sh
# Build the site: landing page at / and the book at /book/.
# Usage: site/build.sh [OUT]   (default: target/site)
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-"$root/target/site"}

if [ "$(mdbook --version)" != "mdbook v0.5.2" ]; then
  echo "warning: built and tested with mdbook v0.5.2, found $(mdbook --version)" >&2
fi

tokens() { python3 "$root/site/tools/gen_tokens.py" "$root/site/tokens.json" "$@"; }

# The book takes its tokens from the theme class mdbook puts on <html>.
tokens --light "html.light, html.rust" --dark "html.coal, html.navy, html.ayu" \
  -o "$root/book/theme/tokens.css"

mkdir -p "$out"
mdbook build "$root/book" -d "$out/book"

# Landing page files; the tools and the token source stay behind.
for f in "$root"/site/*; do
  case $f in
    "$root"/site/tools | "$root"/site/tokens.json) ;;
    *) cp -R "$f" "$out/" ;;
  esac
done

# The landing page takes its tokens from a data-theme attribute.
tokens --light "[data-theme=light]" --dark "[data-theme=dark]" -o "$out/tokens.css"
