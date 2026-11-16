import json
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from gen_tokens import tokens_css  # noqa: E402

TOKENS_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "tokens.json")
with open(TOKENS_PATH, encoding="utf-8") as f:
    TOKENS = json.load(f)

BOOK_LIGHT = "html.light, html.rust"
BOOK_DARK = "html.coal, html.navy, html.ayu"

# The landing page's colours as site/tailwind.css had them before it read
# tokens.json. The landing page must not change, so a change here is a
# change to the site: make it on purpose.
SITE_COLOURS = {
    "ground": ("#f2f5f6", "#0e1920"),
    "raised": ("#ffffff", "#15242d"),
    "rule": ("#ccd6db", "#263943"),
    "ink": ("#13222c", "#e2ebee"),
    "muted": ("#4f616c", "#96a9b3"),
    "aqua": ("#00a6b4", "#2cc4d1"),
    "aqua-ink": ("#00707b", "#5ed6e0"),
    "on-aqua": ("#ffffff", "#0e1920"),
    "signal": ("#f6c700", "#f6c700"),
    "on-signal": ("#13222c", "#13222c"),
    "term": ("#13222c", "#081116"),
    "term-ink": ("#dde7eb", "#dde7eb"),
    "term-muted": ("#8da3ae", "#7f949f"),
    "error": ("#c8322b", "#c8322b"),
    "on-error": ("#ffffff", "#ffffff"),
}


def _block(css, opener):
    """The declarations of the rule that starts with `opener`."""
    return css[css.index(opener):].split("}")[0]


def _colours(mode):
    """{name: value} of every colour token in `mode` ("light" or "dark")."""
    out = {}
    for tok in TOKENS["color"]["tokens"]:
        v = tok["value"]
        out[tok["name"]] = v[mode] if isinstance(v, dict) else v
    return out


def _luminance(hex_colour):
    h = hex_colour.lstrip("#")
    chans = [int(h[i:i + 2], 16) / 255 for i in (0, 2, 4)]
    lin = [c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4 for c in chans]
    return 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2]


def _contrast(a, b):
    hi, lo = sorted((_luminance(a), _luminance(b)), reverse=True)
    return (hi + 0.05) / (lo + 0.05)


class SiteFormTest(unittest.TestCase):
    def test_site_form_light_on_root(self):
        css = tokens_css(TOKENS)
        root = _block(css, ":root {")
        self.assertIn("--ir-ground: #f2f5f6;", root)
        self.assertIn("--ir-signal: #f6c700;", root)
        self.assertIn("--ir-font-display: Barlow, Bahnschrift,", root)
        self.assertIn("--ir-text-body: 1.0625rem;", root)
        self.assertIn("--ir-text-body-line: 1.6;", root)
        self.assertIn("--ir-text-display: clamp(2rem, 4vw, 3rem);", root)
        self.assertIn("--ir-text-display-tracking: -0.01em;", root)
        self.assertIn("--ir-radius-sm: 2px;", root)
        self.assertIn("--ir-radius-md: 4px;", root)

    def test_site_form_dark_under_media_query(self):
        css = tokens_css(TOKENS)
        media = css[css.index("@media (prefers-color-scheme: dark)"):]
        self.assertIn(":root {", media)
        self.assertIn("--ir-ground: #0e1920;", media)
        self.assertNotIn("--ir-signal", media)
        self.assertNotIn("html.", css)

    def test_every_value_matches_the_site(self):
        light, dark = _colours("light"), _colours("dark")
        for name, (lv, dv) in SITE_COLOURS.items():
            self.assertEqual(light[name], lv, name)
            self.assertEqual(dark[name], dv, name)


class BookFormTest(unittest.TestCase):
    def test_book_form(self):
        css = tokens_css(TOKENS, BOOK_LIGHT, BOOK_DARK)
        self.assertIn("--ir-ground: #f2f5f6;", _block(css, f":root, {BOOK_LIGHT} {{"))
        self.assertIn("--ir-ground: #0e1920;", _block(css, f"{BOOK_DARK} {{"))
        self.assertIn(f":root:not({BOOK_LIGHT})", css)

    def test_one_selector_only_is_an_error(self):
        with self.assertRaises(ValueError):
            tokens_css(TOKENS, "html.light", None)
        with self.assertRaises(ValueError):
            tokens_css(TOKENS, None, "html.navy")


class ContrastTest(unittest.TestCase):
    PAIRS = [
        (fg, bg)
        for fg in ("ink", "muted", "aqua-ink", "warn-ink", "danger-ink")
        for bg in ("ground", "raised")
    ] + [
        ("aqua-ink", "aqua-soft"),
        ("warn-ink", "warn-soft"),
        ("danger-ink", "danger-soft"),
        ("ink", "aqua-soft"),
        ("ink", "warn-soft"),
        ("ink", "danger-soft"),
    ]

    def test_contrast(self):
        for mode in ("light", "dark"):
            c = _colours(mode)
            for fg, bg in self.PAIRS:
                with self.subTest(mode=mode, fg=fg, bg=bg):
                    self.assertGreaterEqual(_contrast(c[fg], c[bg]), 4.5)


if __name__ == "__main__":
    unittest.main()
