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
PRETTY = "tutorial/first-sync"


def url(book, page=PRETTY):
    """Where the switcher links `page` of `book`: Dev on dev.irsync.org,
    the others on irsync.org."""
    host = "https://dev.irsync.org" if book == "dev" else "https://irsync.org"
    return f"{host}/{book}/{page}"


def links(menu):
    """{href: the whole <a> tag} of a menu."""
    return {m.group(1): m.group(0) for m in re.finditer(r'<a [^>]*href="([^"]*)"[^>]*>', menu)}


class EntriesTests(unittest.TestCase):
    def test_entries(self):
        self.assertEqual(switcher.entries(TODAY), [Entry("beta", "Beta", "0.1.0-beta.5", SHA),
                                                   Entry("alpha", "Alpha", "0.1.0-alpha.4", SHA),
                                                   Entry("dev", "Dev", None, SHA)])
        with_stable = TODAY.replace("\nbeta", f"\nstable 1.0.0 {SHA} published\nbeta", 1)
        self.assertEqual(switcher.entries(with_stable)[0], Entry("stable", "Stable", "1.0.0", SHA))
        self.assertEqual(switcher.entries(NO_RELEASE), [Entry("dev", "Dev", None, SHA)])

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
        self.assertEqual(set(got), {url(p) for p in ("beta", "alpha", "dev")})
        self.assertIn("https://dev.irsync.org/dev/tutorial/first-sync", got)
        self.assertIn("https://irsync.org/beta/tutorial/first-sync", got)
        self.assertEqual(menu.count("data-same-page"), 3)
        self.assertIn('aria-current="true"', got[url("beta")])
        self.assertNotIn("aria-current", got[url("alpha")])
        self.assertRegex(menu, r'<button [^>]*id="irsync-book-toggle"[^>]*>\s*Beta')
        self.assertIn('aria-haspopup="true"', menu)
        self.assertIn('aria-expanded="false"', menu)
        self.assertIn('aria-controls="irsync-book-list"', menu)
        self.assertIn('id="irsync-book-list"', menu)
        self.assertIn('>Dev <span class="irsync-book-version">irsync@bbbbbbb</span>', menu)
        self.assertNotIn("unreleased", menu)
        self.assertIn("0.1.0-alpha.4", menu)

    def test_release_entries_show_their_version_not_a_commit(self):
        menu = switcher.menu(switcher.entries(TODAY), "beta", PAGE, self.all_pages())
        self.assertIn('>Beta <span class="irsync-book-version">0.1.0-beta.5</span>', menu)
        self.assertEqual(menu.count("irsync@"), 1)

    def test_dev_without_a_commit_is_unreleased(self):
        # A VERSIONS file from a build before the line carried the commit.
        old = TODAY.replace(f"dev - {SHA} main", "dev - - dev")
        menu = switcher.menu(switcher.entries(old), "beta", PAGE, self.all_pages())
        self.assertIn('>Dev <span class="irsync-book-version">unreleased</span>', menu)
        self.assertNotIn("irsync@", menu)

    def test_a_list_of_links_not_a_menu(self):
        # Menu roles promise arrow keys; these are plain links behind a button.
        menu = switcher.menu(switcher.entries(TODAY), "beta", PAGE, self.all_pages())
        self.assertNotIn("role=", menu)

    def test_missing_page_goes_to_the_title_page(self):
        pages = self.all_pages()
        pages["alpha"] = {"index.html"}
        got = links(switcher.menu(switcher.entries(TODAY), "beta", PAGE, pages))
        self.assertIn(url("alpha", ""), got)
        self.assertNotIn("data-same-page", got[url("alpha", "")])
        self.assertIn("data-same-page", got[url("dev")])

    def test_not_found_page_goes_to_title_pages(self):
        pages = {p: {"index.html", "404.html"} for p in ("beta", "alpha", "dev")}
        menu = switcher.menu(switcher.entries(TODAY), "beta", "404.html", pages)
        self.assertEqual(set(links(menu)), {url(p, "") for p in ("beta", "alpha", "dev")})
        self.assertNotIn("data-same-page", menu)

    def test_local_links_stay_in_the_build(self):
        # site/serve.sh: the books on this machine, not on the two sites.
        got = links(switcher.menu(switcher.entries(TODAY), "beta", PAGE, self.all_pages(),
                                  local=True))
        self.assertEqual(set(got), {f"../../{p}/{PAGE}" for p in ("beta", "alpha", "dev")})
        got = links(switcher.menu(switcher.entries(TODAY), "beta", "index.html", {}, up="",
                                  local=True))
        self.assertEqual(set(got), {f"{p}/index.html" for p in ("beta", "alpha", "dev")})

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
        self.assertIn(url("alpha", ""), got)
        self.assertIn(url("beta"), got)
        self.assertIn('aria-current="true"', got[url("beta")])
        for book in ("book", "beta", "alpha", "dev"):
            self.assertEqual((self.site / book / "toc.html").read_text(encoding="utf-8"),
                             "<ol></ol>\n")
        self.assertIn("irsync@bbbbbbb", (self.site / "dev" / PAGE).read_text(encoding="utf-8"))
        self.assertEqual(check.check_links(self.site), [])

    def test_landing_page(self):
        (self.site / "VERSIONS").write_text(TODAY, encoding="utf-8")
        bar = '<html><body><div class="right-buttons"></div></body></html>\n'
        for book in ("book", "beta", "alpha", "dev"):
            (self.site / book).mkdir()
            (self.site / book / "index.html").write_text(bar, encoding="utf-8")
        (self.site / "index.html").write_text(
            "<ul><li><!-- switcher -->x<!-- /switcher --></li></ul>\n", encoding="utf-8")
        switcher.apply(self.site)
        page = (self.site / "index.html").read_text(encoding="utf-8")
        got = links(page)
        self.assertEqual(set(got), {url(p, "") for p in ("beta", "alpha", "dev")})
        # The main version's channel is ticked, as in /book/.
        self.assertIn('aria-current="true"', got[url("beta", "")])
        self.assertNotIn("data-same-page", page)
        # The landing page remembers the book picked by its data-book.
        self.assertIn('data-book="alpha"', got[url("alpha", "")])
        self.assertNotIn("switcher -->", page)
        self.assertIn("irsync@bbbbbbb", page)
        self.assertEqual(check.check_links(self.site), [])

    def test_landing_page_without_markers_stops(self):
        with self.assertRaisesRegex(switcher.SwitcherError, "index.html"):
            switcher.landing("<p>no markers</p>", "<M>")
        with self.assertRaises(switcher.SwitcherError):
            switcher.landing("<!-- switcher --><!-- /switcher -->" * 2, "<M>")

    def test_a_book_without_a_top_bar_stops(self):
        # A future mdbook that names the bar differently must not drop the
        # switcher in silence.
        (self.site / "VERSIONS").write_text(NO_RELEASE, encoding="utf-8")
        for book in ("book", "dev"):
            (self.site / book).mkdir()
            (self.site / book / "index.html").write_text("<div class=\"bar\"></div>\n",
                                                         encoding="utf-8")
        with self.assertRaisesRegex(switcher.SwitcherError, "book/index.html: no top bar"):
            switcher.apply(self.site)

    def test_cli_local(self):
        (self.site / "VERSIONS").write_text(NO_RELEASE, encoding="utf-8")
        bar = '<html><body><div class="right-buttons"></div></body></html>\n'
        for book in ("book", "dev"):
            (self.site / book).mkdir()
            (self.site / book / "index.html").write_text(bar, encoding="utf-8")
        r = subprocess.run([sys.executable, str(TOOL), "--local", str(self.site)],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        got = links((self.site / "book" / "index.html").read_text(encoding="utf-8"))
        self.assertEqual(set(got), {"../dev/index.html"})

    def test_cli_error(self):
        r = subprocess.run([sys.executable, str(TOOL), str(self.site)],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 1)
        self.assertTrue(r.stderr.startswith("switcher.py: "), r.stderr)


if __name__ == "__main__":
    unittest.main()
