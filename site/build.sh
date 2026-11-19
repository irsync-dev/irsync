#!/bin/sh
# Build the site: the landing page at /, and the book once per release
# channel: /book/ (the main version), /stable/, /beta/, /alpha/ and /dev/
# (this working tree). See "The books" in book/src/contributing/website.md.
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

# mdbook, at exactly the version CI builds the site with, so the same
# sources give the same pages everywhere. No other version builds.
mdbook_pin=$(sed -n 's/^ *MDBOOK_VERSION: *\([0-9.]*\).*/\1/p' "$root/.github/workflows/site.yml" | sed -n 1p)
mdbook_v=$({ mdbook --version 2>/dev/null || true; } | sed -n '1s/[^0-9]*\([0-9.]*\).*/\1/p')
if [ -z "$mdbook_v" ]; then
  echo "mdbook not found; tools/dev-setup.sh installs it" >&2
  exit 1
elif [ "$mdbook_v" != "$mdbook_pin" ]; then
  echo "the site is built with mdbook $mdbook_pin, found $mdbook_v; tools/dev-setup.sh installs $mdbook_pin" >&2
  exit 1
fi

tokens() { python3 "$root/site/tools/gen_tokens.py" "$root/site/tokens.json" "$@"; }

# The landing page takes its tokens from the system's light or dark setting;
# tailwind.css imports site/tokens.css. The book takes them from the theme
# class mdbook puts on <html>.
tokens -o "$root/site/tokens.css"
tokens --light "html.light, html.rust" --dark "html.coal, html.navy, html.ayu" \
  -o "$root/book/theme/tokens.css"

# Start from an empty output, so files removed from site/ do not linger.
rm -rf "$out"
mkdir -p "$out"

# The books. versions.py lists them, one per line: PATH VERSION COMMIT STATE.
# The list ships as VERSIONS, so a promote can tell what a build holds. Each
# book is built from a working copy: today's settings and theme, and the
# pages of its version's commit (of this working tree for VERSION -), which
# label_book.py labels with the version. Nothing under book/ is changed.
python3 "$root/site/tools/versions.py" books "$root" >"$out/VERSIONS"
books=$root/target/site-books
while read -r path version commit _; do
  work=$books/$path
  rm -rf "$work"
  mkdir -p "$work"
  cp "$root/book/book.toml" "$work/"
  cp -R "$root/book/theme" "$work/theme"
  if [ "$version" = - ]; then
    cp -R "$root/book/src" "$work/src"
  else
    # All of that commit's pages or none: versions.py says what went wrong.
    python3 "$root/site/tools/versions.py" pages "$root" "$commit" "$work/src" || {
      echo "cannot build the book of $version" >&2
      exit 1
    }
  fi
  python3 "$root/site/tools/label_book.py" "$work" "$path" "$version"
  mdbook build "$work" -d "$out/$path"
  # The book uses the system font stacks from the tokens. The empty
  # theme/fonts/fonts.css keeps mdbook's web fonts out; drop that file too,
  # and its link from every page.
  rm -rf "$out/$path/fonts"
  python3 "$root/site/tools/strip_font_link.py" "$out/$path"
done <"$out/VERSIONS"

# The version switcher in every book's top bar. It needs every book built:
# each link goes to the same page in the other book if that book has it.
python3 "$root/site/tools/switcher.py" "$out"

# The landing page files and install.sh. Everything else in site/ (the
# tools, the sources, the scripts, the deploy settings) stays behind.
for f in 404.html robots.txt _headers install.sh; do
  cp "$root/site/$f" "$out/"
done
# The landing page itself, with the release versions from git written in.
python3 "$root/site/tools/versions.py" render "$root" "$root/site/index.html" "$out/index.html"

# Its stylesheet: Tailwind CSS and daisyUI, at the versions pinned in
# site/package-lock.json, compile site/tailwind.css into style.css. npm ci
# runs only when site/node_modules is missing or older than the lock file.
site=$root/site
if [ ! -f "$site/node_modules/.package-lock.json" ] ||
  [ "$site/package-lock.json" -nt "$site/node_modules/.package-lock.json" ]; then
  (cd "$site" && npm ci --ignore-scripts --no-audit --no-fund)
fi
"$site/node_modules/.bin/tailwindcss" --cwd "$site" -i tailwind.css -o "$out/style.css" --minify

# The web fonts of the landing page and the book: Barlow for headings,
# labels and the release versions, Inter for running text, JetBrains Mono for code. Only the faces
# tailwind.css and book/theme/irsync.css name, Latin only, and each
# family's licence (SIL Open Font License), which must ship with it. The
# two Barlow packages carry the same Barlow Project licence.
fontsource=$site/node_modules/@fontsource
mkdir -p "$out/fonts"
for f in barlow/files/barlow-latin-400-normal.woff2 \
  barlow/files/barlow-latin-600-normal.woff2 \
  barlow-semi-condensed/files/barlow-semi-condensed-latin-400-normal.woff2 \
  barlow-semi-condensed/files/barlow-semi-condensed-latin-600-normal.woff2 \
  inter/files/inter-latin-400-normal.woff2 \
  inter/files/inter-latin-400-italic.woff2 \
  inter/files/inter-latin-600-normal.woff2 \
  jetbrains-mono/files/jetbrains-mono-latin-400-normal.woff2 \
  jetbrains-mono/files/jetbrains-mono-latin-400-italic.woff2 \
  jetbrains-mono/files/jetbrains-mono-latin-600-normal.woff2; do
  cp "$fontsource/$f" "$out/fonts/"
done
cp "$fontsource/barlow/LICENSE" "$out/fonts/barlow-OFL.txt"
cp "$fontsource/inter/LICENSE" "$out/fonts/inter-OFL.txt"
cp "$fontsource/jetbrains-mono/LICENSE" "$out/fonts/jetbrains-mono-OFL.txt"

# Last, once every page is in place: link to the pages without .html, the
# addresses Cloudflare serves them at (see pretty_urls.py).
python3 "$root/site/tools/pretty_urls.py" "$out"
