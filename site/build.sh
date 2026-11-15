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

tokens() { python3 "$root/site/tools/gen_tokens.py" "$root/site/tokens.json" "$@"; }

# The book takes its tokens from the theme class mdbook puts on <html>.
tokens --light "html.light, html.rust" --dark "html.coal, html.navy, html.ayu" \
  -o "$root/book/theme/tokens.css"

# Start from an empty output, so files removed from site/ do not linger.
rm -rf "$out"
mkdir -p "$out"
mdbook build "$root/book" -d "$out/book"
# The book uses the system font stacks from the tokens. The empty
# book/theme/fonts/fonts.css keeps mdbook's web fonts out; drop that file too,
# and its link from every page.
rm -rf "$out/book/fonts"
python3 "$root/site/tools/strip_font_link.py" "$out/book"

# The landing page files and install.sh. Everything else in site/ (the
# tools, the sources, the scripts, the deploy settings) stays behind.
for f in index.html 404.html robots.txt _headers install.sh; do
  cp "$root/site/$f" "$out/"
done

# The landing page takes its tokens from a data-theme attribute.
tokens --light "[data-theme=light]" --dark "[data-theme=dark]" -o "$out/tokens.css"

# Its stylesheet: Tailwind CSS and daisyUI, at the versions pinned in
# site/package-lock.json, compile site/tailwind.css into style.css. npm ci
# runs only when site/node_modules is missing or older than the lock file.
site=$root/site
if [ ! -f "$site/node_modules/.package-lock.json" ] ||
  [ "$site/package-lock.json" -nt "$site/node_modules/.package-lock.json" ]; then
  (cd "$site" && npm ci --ignore-scripts --no-audit --no-fund)
fi
"$site/node_modules/.bin/tailwindcss" --cwd "$site" -i tailwind.css -o "$out/style.css" --minify
