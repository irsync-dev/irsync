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
        old = r"--ir-(surface|accent|line|space-|focus-ring|font-sans|shadow|ink-muted|code-|body-|display-|heading-|(?:link|warn|danger)(?![\w-]))"
        self.assertEqual(re.findall(old, RULES), [])

    def test_every_ir_var_is_defined(self):
        defined = set(re.findall(r"(--ir-[\w-]+):", tokens_css(TOKENS, "a", "b")))
        defined |= set(re.findall(r"(--ir-[\w-]+)\s*:", RULES))
        used = set(re.findall(r"var\((--ir-[\w-]+)\)", RULES))
        self.assertEqual(used - defined, set())

    def test_book_scales_rem_sizes(self):
        # mdbook's root font size is 62.5%, so a rem from the tokens is
        # scaled by --ir-book-rem wherever it is used.
        self.assertRegex(RULES, r"--ir-book-rem:\s*1\.6;")
        for m in re.finditer(r"var\((--ir-text-[a-z]+)\)", RULES):
            line = RULES[RULES.rfind("\n", 0, m.start()) + 1:RULES.find("\n", m.end())]
            self.assertRegex(line, r"calc\(var\(--ir-text-[a-z]+\) \* var\(--ir-book-rem\)\)", line)

    def test_barlow_from_site_fonts(self):
        urls = re.findall(r'url\("([^"]+)"\)', RULES)
        self.assertEqual(
            sorted(urls),
            [
                "../../fonts/barlow-latin-400-normal.woff2",
                "../../fonts/barlow-latin-600-normal.woff2",
                "../../fonts/barlow-semi-condensed-latin-600-normal.woff2",
            ],
        )

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

    def test_picker_script_is_loaded(self):
        toml = _read("book", "book.toml")
        self.assertRegex(toml, r'additional-js\s*=\s*\[[^\]]*"theme/picker\.js"')
        js = _read("book", "theme", "picker.js")
        for name in ("mdbook-theme-rust", "mdbook-theme-coal", "mdbook-theme-ayu", "mdbook-theme-navy", '"Dark"'):
            self.assertIn(name, js)


if __name__ == "__main__":
    unittest.main()
