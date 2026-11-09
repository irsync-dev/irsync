import re
import unittest
from pathlib import Path

HEADERS = Path(__file__).resolve().parent.parent / "_headers"


def parse_headers(text):
    """Cloudflare `_headers`: {url pattern: {header name: value}}.

    A rule is an unindented URL pattern followed by indented `Name: value`
    lines; `#` starts a comment line.
    """
    rules = {}
    current = None
    for line in text.splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if not line[0].isspace():
            current = rules.setdefault(line.strip(), {})
            continue
        if current is None:
            raise ValueError(f"header outside a rule: {line!r}")
        name, sep, value = line.strip().partition(":")
        if not sep:
            raise ValueError(f"not a header: {line!r}")
        current[name.strip()] = value.strip()
    return rules


def pattern_matches(pattern, url):
    """Whether a `_headers` URL pattern matches an absolute URL.

    A pattern starting with `/` matches that path on every host; `*` is a
    splat and `:name` a placeholder for one host label or path segment.
    """
    if pattern.startswith("/"):
        pattern = "https://:host" + pattern
    regex = ""
    for token in re.split(r"(\*|:[A-Za-z]\w*)", pattern):
        if token == "*":
            regex += ".*"
        elif token.startswith(":") and len(token) > 1:
            regex += r"[^./]+"
        else:
            regex += re.escape(token)
    # Placeholders may stand for a whole host, dots and all.
    regex = regex.replace(r"https://[^./]+/", r"https://[^/]+/", 1)
    return re.fullmatch(regex, url) is not None


class PatternTest(unittest.TestCase):
    def test_pattern_matching(self):
        self.assertTrue(pattern_matches("/*", "https://irsync.org/"))
        self.assertTrue(pattern_matches("https://dev.irsync.org/*", "https://dev.irsync.org/book/"))
        self.assertFalse(pattern_matches("https://dev.irsync.org/*", "https://irsync.org/"))
        workers_dev = "https://:worker.:account.workers.dev/*"
        self.assertTrue(pattern_matches(workers_dev, "https://irsync-pub.example.workers.dev/"))
        self.assertTrue(pattern_matches(workers_dev, "https://0a1b2c3d-irsync-pub.example.workers.dev/book/"))
        self.assertFalse(pattern_matches(workers_dev, "https://irsync.org/"))


class HeadersTest(unittest.TestCase):
    def setUp(self):
        self.rules = parse_headers(HEADERS.read_text())

    def test_noindex_only_for_staging_hosts(self):
        noindex = {
            pattern
            for pattern, headers in self.rules.items()
            if headers.get("X-Robots-Tag") == "noindex"
        }
        self.assertEqual(
            noindex,
            {
                "https://dev.irsync.org/*",
                "https://:worker.:account.workers.dev/*",
            },
        )
        for pattern, headers in self.rules.items():
            if pattern_matches(pattern, "https://irsync.org/"):
                self.assertNotIn("X-Robots-Tag", headers, pattern)

    def test_every_host_gets_the_safety_headers(self):
        everywhere = {}
        for pattern, headers in self.rules.items():
            if pattern_matches(pattern, "https://irsync.org/"):
                everywhere.update(headers)
        self.assertEqual(everywhere.get("X-Content-Type-Options"), "nosniff")
        self.assertEqual(everywhere.get("Referrer-Policy"), "strict-origin-when-cross-origin")


if __name__ == "__main__":
    unittest.main()
