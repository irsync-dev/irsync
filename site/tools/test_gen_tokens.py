import json
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from gen_tokens import tokens_css  # noqa: E402

TOKENS_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "tokens.json")
with open(TOKENS_PATH, encoding="utf-8") as f:
    TOKENS = json.load(f)


class TokensCssTest(unittest.TestCase):
    def test_dark_values_are_the_root_default(self):
        css = tokens_css(TOKENS, "[data-theme=light]", "[data-theme=dark]")
        self.assertRegex(css, r":root[^{]*\{[^}]*--ir-surface:\s*#121412;")

    def test_light_values_under_light_selector(self):
        css = tokens_css(TOKENS, "html.light, html.rust", "html.coal")
        block = css[css.index("html.light, html.rust"):]
        self.assertIn("--ir-surface: #f7f6f2;", block.split("}")[0])
        self.assertIn("--ir-accent: #0f766e;", block.split("}")[0])

    def test_light_values_follow_prefers_color_scheme(self):
        css = tokens_css(TOKENS, "[data-theme=light]", "[data-theme=dark]")
        self.assertIn("@media (prefers-color-scheme: light)", css)
        self.assertIn(":root:not([data-theme=dark])", css)

    def test_alias_becomes_var(self):
        css = tokens_css(TOKENS, "a", "b")
        self.assertIn("--ir-link: var(--ir-accent);", css)

    def test_themed_shadow_and_plain_lengths(self):
        css = tokens_css(TOKENS, "a", "b")
        self.assertIn("--ir-space-4: 16px;", css)
        self.assertIn("--ir-radius-lg: 8px;", css)
        self.assertIn("--ir-focus-ring: 0 0 0 2px #121412, 0 0 0 4px #2dd4bf;", css)

    def test_type_styles(self):
        css = tokens_css(TOKENS, "a", "b")
        self.assertIn("--ir-display-size: 32px;", css)
        self.assertIn("--ir-display-tracking: -0.01em;", css)
        self.assertIn("--ir-font-mono: ui-monospace,", css)


if __name__ == "__main__":
    unittest.main()
