"""Checks of the book's theme sources (book/theme/, book/book.toml) against
the design tokens. They read the source files; no build is needed."""

import json
import os
import re
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from gen_tokens import tokens_css  # noqa: E402

ROOT = os.path.join(HERE, "..", "..")


def _read(*parts):
    with open(os.path.join(ROOT, *parts), encoding="utf-8") as f:
        return f.read()


with open(os.path.join(ROOT, "site", "tokens.json"), encoding="utf-8") as f:
    TOKENS = json.load(f)
CSS = _read("book", "theme", "irsync.css")
# Comments may name things the rules don't use.
RULES = re.sub(r"/\*.*?\*/", "", CSS, flags=re.S)


class BookThemeTest(unittest.TestCase):
    def test_no_old_token_names(self):
        old = r"--ir-(font-serif|surface|accent|line|space-|focus-ring|font-sans|shadow|ink-muted|code-|body-|display-|heading-|(?:link|warn|danger)(?![\w-]))"
        self.assertEqual(re.findall(old, RULES), [])

    def test_every_ir_var_is_defined(self):
        defined = set(re.findall(r"(--ir-[\w-]+):", tokens_css(TOKENS, "a", "b")))
        defined |= set(re.findall(r"(--ir-[\w-]+)\s*:", RULES))
        used = set(re.findall(r"var\((--ir-[\w-]+)\)", RULES))
        self.assertEqual(used - defined, set())

    def test_book_scales_rem_sizes(self):
        # mdbook's root font size is 62.5%, so a rem from the tokens is
        # scaled by --ir-book-rem wherever it is used. Sizes in em are
        # relative to the text around them and are used as they are.
        self.assertRegex(RULES, r"--ir-book-rem:\s*1\.6;")
        sizes = {f"--ir-text-{s['name']}": s["size"] for s in TOKENS["type"]["scale"]}
        for m in re.finditer(r"var\((--ir-text-[a-z-]+)\)", RULES):
            if m.group(1).endswith(("-line", "-tracking")) or "rem" not in sizes[m.group(1)]:
                continue
            line = RULES[RULES.rfind("\n", 0, m.start()) + 1:RULES.find("\n", m.end())]
            self.assertRegex(line, r"calc\(var\(--ir-text-[a-z]+\) \* var\(--ir-book-rem\)\)", line)

    def test_web_fonts_from_site_fonts(self):
        urls = re.findall(r'url\("([^"]+)"\)', RULES)
        self.assertEqual(
            sorted(urls),
            [
                "../../fonts/barlow-latin-400-normal.woff2",
                "../../fonts/barlow-latin-600-normal.woff2",
                "../../fonts/barlow-semi-condensed-latin-600-normal.woff2",
                "../../fonts/inter-latin-400-italic.woff2",
                "../../fonts/inter-latin-400-normal.woff2",
                "../../fonts/inter-latin-600-normal.woff2",
                "../../fonts/jetbrains-mono-latin-400-italic.woff2",
                "../../fonts/jetbrains-mono-latin-400-normal.woff2",
                "../../fonts/jetbrains-mono-latin-600-normal.woff2",
            ],
        )

    def test_bold_is_the_shipped_semibold(self):
        # Inter and JetBrains Mono ship 600, not 700, so bold text uses 600
        # rather than a weight the browser would have to fake.
        self.assertRegex(RULES, r"strong,\s*b\s*\{[^}]*font-weight:\s*600")

    def test_inline_code_takes_the_code_token(self):
        self.assertRegex(RULES, r":not\(pre\) > code\s*\{[^}]*font-size:\s*var\(--ir-text-code\)")

    def test_highlight_classes_of_the_dark_theme_are_covered(self):
        # tomorrow-night.css, mdbook's highlight sheet for Coal and Navy,
        # colours these classes too; without an override they keep its
        # hues (bash function names came out blue).
        covered = set(re.findall(r"html \.(hljs-[\w-]+)", RULES))
        dark_only = {
            "hljs-cdata", "hljs-class", "hljs-constant", "hljs-decorator",
            "hljs-doctype", "hljs-function", "hljs-header", "hljs-hexcolor",
            "hljs-id", "hljs-inheritance", "hljs-pi", "hljs-pragma",
            "hljs-preprocessor", "hljs-pseudo", "hljs-rule", "hljs-sub",
            "hljs-value",
        }
        self.assertEqual(dark_only - covered, set())

    def test_warning_icon_is_readable(self):
        # mdbook colours the .warning icon with --warning-border, which is
        # the signal-yellow bar: too faint as an icon on the light ground.
        self.assertRegex(RULES, r"\.warning::?before\s*\{[^}]*color:\s*var\(--ir-warn-ink\)")

    def test_copy_button_takes_token_colours(self):
        # mdbook tints the copy icon with per-theme CSS filters (blue on
        # hover in Light); the book draws it as a mask in muted, aqua-ink
        # on hover, so every theme looks the same.
        rule = re.search(r"button\.clip-button::before\s*\{([^}]*)\}", RULES)
        self.assertIsNotNone(rule)
        self.assertIn("filter: none", rule.group(1))
        self.assertIn("background-color: var(--ir-muted)", rule.group(1))
        self.assertRegex(rule.group(1), r"mask(-image)?:\s*url\(")
        hover = re.search(r"button\.clip-button:hover::before\s*\{([^}]*)\}", RULES)
        self.assertIsNotNone(hover)
        self.assertIn("background-color: var(--ir-aqua-ink)", hover.group(1))

    def test_home_link_script_is_loaded(self):
        toml = _read("book", "book.toml")
        self.assertRegex(toml, r'additional-js\s*=\s*\[[^\]]*"theme/home-link\.js"')
        js = _read("book", "theme", "home-link.js")
        self.assertIn("menu-title", js)
        self.assertIn('"/"', js)

    def test_home_link_looks_like_the_site_wordmark(self):
        # The landing page's "irsync": Barlow semibold in ink, no underline.
        rule = re.search(r"\.menu-title a(?:,\s*\.menu-title a:visited)?\s*\{([^}]*)\}", RULES)
        self.assertIsNotNone(rule)
        self.assertIn("color: var(--ir-ink)", rule.group(1))
        self.assertIn("text-decoration: none", rule.group(1))
        self.assertRegex(RULES, r"\.menu-title a:hover\s*\{[^}]*text-decoration: underline")

    def test_picker_script_is_loaded(self):
        toml = _read("book", "book.toml")
        self.assertRegex(toml, r'additional-js\s*=\s*\[[^\]]*"theme/picker\.js"')
        js = _read("book", "theme", "picker.js")
        for name in ("mdbook-theme-rust", "mdbook-theme-coal", "mdbook-theme-ayu", "mdbook-theme-navy", '"Dark"'):
            self.assertIn(name, js)

    def test_switcher_script_is_loaded(self):
        # site/tools/switcher.py writes the markup; the script opens it.
        toml = _read("book", "book.toml")
        self.assertRegex(toml, r'additional-js\s*=\s*\[[^\]]*"theme/switcher\.js"')
        js = _read("book", "theme", "switcher.js")
        for name in ("irsync-book-toggle", "irsync-book-list", "aria-expanded", "data-same-page",
                     "Escape"):
            self.assertIn(name, js)

    def test_switcher_click_reaches_mdbook(self):
        # mdbook closes its theme list on a click elsewhere on the page; the
        # switcher's button must not swallow that click.
        self.assertNotIn("stopPropagation", _read("book", "theme", "switcher.js").split("keydown")[0])

    def test_switcher_takes_the_popup_colours(self):
        rule = re.search(r"#irsync-book-list\s*\{([^}]*)\}", RULES)
        self.assertIsNotNone(rule)
        self.assertIn("var(--theme-popup-bg)", rule.group(1))
        self.assertIn("var(--theme-popup-border)", rule.group(1))


if __name__ == "__main__":
    unittest.main()
