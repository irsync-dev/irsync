import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import check
import switcher
from switcher import Entry

TOOL = Path(__file__).resolve().parent / "switcher.py"
SHA = "b" * 40
TODAY = (f"book 0.1.0-beta.5 {SHA} published\nbeta 0.1.0-beta.5 {SHA} published\n"
         f"alpha 0.1.0-alpha.4 {SHA} published\ndev - {SHA} main\n")
NO_RELEASE = f"book - {SHA} main\ndev - {SHA} main\n"
PAGE = "tutorial/first-sync.html"


def links(menu):
    """{href: the whole <a> tag} of a menu."""
    return {m.group(1): m.group(0) for m in re.finditer(r'<a [^>]*href="([^"]*)"[^>]*>', menu)}


class EntriesTests(unittest.TestCase):
    def test_entries(self):
        self.assertEqual(switcher.entries(TODAY), [Entry("beta", "Beta", "0.1.0-beta.5"),
                                                   Entry("alpha", "Alpha", "0.1.0-alpha.4"),
                                                   Entry("dev", "Dev", None)])
        with_stable = TODAY.replace("\nbeta", f"\nstable 1.0.0 {SHA} published\nbeta", 1)
        self.assertEqual(switcher.entries(with_stable)[0], Entry("stable", "Stable", "1.0.0"))
        self.assertEqual(switcher.entries(NO_RELEASE), [Entry("dev", "Dev", None)])

    def test_ticked(self):
        self.assertEqual(switcher.ticked(TODAY, "book"), "beta")
        self.assertEqual(switcher.ticked(TODAY, "alpha"), "alpha")
        self.assertEqual(switcher.ticked(TODAY, "dev"), "dev")
        self.assertEqual(switcher.ticked(NO_RELEASE, "book"), "dev")

    def test_bad_versions_file(self):
        with self.assertRaises(switcher.SwitcherError):
            switcher.entries("nonsense\n")


class MenuTests(unittest.TestCase):
    def all_pages(self):
        return {p: {"index.html", PAGE} for p in ("beta", "alpha", "dev")}

    def test_page_in_a_subdirectory(self):
        menu = switcher.menu(switcher.entries(TODAY), "beta", PAGE, self.all_pages())
        got = links(menu)
        self.assertEqual(set(got), {f"../../{p}/{PAGE}" for p in ("beta", "alpha", "dev")})
        self.assertEqual(menu.count("data-same-page"), 3)
        self.assertIn('aria-current="true"', got[f"../../beta/{PAGE}"])
        self.assertNotIn("aria-current", got[f"../../alpha/{PAGE}"])
        self.assertRegex(menu, r'<button [^>]*id="irsync-book-toggle"[^>]*>\s*Beta')
        self.assertIn('aria-haspopup="true"', menu)
        self.assertIn('aria-expanded="false"', menu)
        self.assertIn('aria-controls="irsync-book-list"', menu)
        self.assertIn('id="irsync-book-list"', menu)
        self.assertIn("unreleased", menu)
        self.assertIn("0.1.0-alpha.4", menu)

    def test_missing_page_goes_to_the_title_page(self):
        pages = self.all_pages()
        pages["alpha"] = {"index.html"}
        got = links(switcher.menu(switcher.entries(TODAY), "beta", PAGE, pages))
        self.assertIn("../../alpha/index.html", got)
        self.assertNotIn("data-same-page", got["../../alpha/index.html"])
        self.assertIn("data-same-page", got[f"../../dev/{PAGE}"])

    def test_not_found_page_goes_to_title_pages(self):
        pages = {p: {"index.html", "404.html"} for p in ("beta", "alpha", "dev")}
        menu = switcher.menu(switcher.entries(TODAY), "beta", "404.html", pages)
        self.assertEqual(set(links(menu)), {f"../{p}/index.html" for p in ("beta", "alpha", "dev")})
        self.assertNotIn("data-same-page", menu)

    def test_insert(self):
        page = '<div class="right-buttons">\n<a href="print.html">'
        self.assertEqual(switcher.insert(page, "<M>"),
                         '<div class="right-buttons"><M>\n<a href="print.html">')
        self.assertEqual(switcher.insert("<div>no bar</div>", "<M>"), "<div>no bar</div>")
        with self.assertRaises(switcher.SwitcherError):
            switcher.insert(page + page, "<M>")


class ApplyTests(unittest.TestCase):
    def setUp(self):
        self.site = Path(tempfile.mkdtemp(prefix="site-switcher-"))
        self.addCleanup(shutil.rmtree, self.site, ignore_errors=True)

    def test_apply_over_a_small_site(self):
        (self.site / "VERSIONS").write_text(TODAY, encoding="utf-8")
        bar = '<html><body><div class="right-buttons"></div></body></html>\n'
        for book in ("book", "beta", "alpha", "dev"):
            d = self.site / book
            (d / "tutorial").mkdir(parents=True)
            for name in ("index.html", "404.html"):
                (d / name).write_text(bar, encoding="utf-8")
            (d / "toc.html").write_text("<ol></ol>\n", encoding="utf-8")
            if book != "alpha":
                (d / PAGE).write_text(bar, encoding="utf-8")
        switcher.apply(self.site)
        page = (self.site / "book" / PAGE).read_text(encoding="utf-8")
        got = links(page)
        self.assertIn("../../alpha/index.html", got)
        self.assertIn(f"../../beta/{PAGE}", got)
        self.assertIn('aria-current="true"', got[f"../../beta/{PAGE}"])
        for book in ("book", "beta", "alpha", "dev"):
            self.assertEqual((self.site / book / "toc.html").read_text(encoding="utf-8"),
                             "<ol></ol>\n")
        self.assertEqual(check.check_links(self.site), [])

    def test_cli_error(self):
        r = subprocess.run([sys.executable, str(TOOL), str(self.site)],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 1)
        self.assertTrue(r.stderr.startswith("switcher.py: "), r.stderr)


if __name__ == "__main__":
    unittest.main()
