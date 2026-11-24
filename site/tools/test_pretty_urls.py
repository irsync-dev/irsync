import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import check
import pretty_urls as pu

TOOL = Path(__file__).resolve().parent / "pretty_urls.py"
SHA = "b" * 40
VERSIONS = f"book 0.1.0-beta.5 {SHA} published\nbeta 0.1.0-beta.5 {SHA} published\ndev - {SHA} main\n"
TOC = ("var x = 1; this.innerHTML = '<ol><li><a href=\"introduction.html\">Introduction</a></li>"
       "<li><a href=\"tutorial/first-sync.html\">Your first sync</a></li></ol>';\n")
SEARCH = ("window.search = Object.assign(window.search, JSON.parse('{\"doc_urls\":"
          "[\"introduction.html#install\",\"tutorial/first-sync.html#the-quick-way\"],"
          "\"index\":{\"x\":\"a.html\"}}'));\n")


class PrettyTests(unittest.TestCase):
    def test_pages_lose_html(self):
        for href, want in [
            ("introduction.html", "introduction"),
            ("../tutorial/first-sync.html", "../tutorial/first-sync"),
            ("tutorial/first-sync.html#the-quick-way", "tutorial/first-sync#the-quick-way"),
            ("print.html?x=1#y", "print?x=1#y"),
            ("/book/how-to/clean-up.html", "/book/how-to/clean-up"),
        ]:
            self.assertEqual(pu.pretty(href), want, href)

    def test_index_becomes_its_folder(self):
        self.assertEqual(pu.pretty("index.html"), "./")
        self.assertEqual(pu.pretty("../alpha/index.html"), "../alpha/")
        self.assertEqual(pu.pretty("../../dev/index.html#top"), "../../dev/#top")

    def test_everything_else_stays(self):
        for href in ["https://irsync.org/book/x.html", "mailto:a@b.c", "#install", "book/",
                     "install.sh", "css/chrome.css", "", "?q=1", "//other.org/x.html",
                     "data:text/html,x.html"]:
            self.assertEqual(pu.pretty(href), href, href)

    def test_hrefs_in_a_page(self):
        page = ('<a href="introduction.html">I</a> <a href="https://x.org/a.html">X</a>'
                '<link rel="next" href="tutorial/first-sync.html"><a href="#here">h</a>')
        self.assertEqual(pu.rewrite_hrefs(page),
                         '<a href="introduction">I</a> <a href="https://x.org/a.html">X</a>'
                         '<link rel="next" href="tutorial/first-sync"><a href="#here">h</a>')

    def test_the_sidebar_frame(self):
        # Without JavaScript, mdbook shows the sidebar in a frame.
        page = '<iframe class="sidebar-iframe-outer" src="../toc.html"></iframe><img src="a.png">'
        self.assertEqual(pu.rewrite_hrefs(page),
                         '<iframe class="sidebar-iframe-outer" src="../toc"></iframe><img src="a.png">')

    def test_search_index(self):
        got = pu.rewrite_search_index(SEARCH)
        self.assertIn('"doc_urls":["introduction#install","tutorial/first-sync#the-quick-way"]', got)
        # Only the addresses; the words of the index stay as they are.
        self.assertIn('"x":"a.html"', got)

    def test_search_index_without_addresses_stops(self):
        with self.assertRaises(pu.PrettyError):
            pu.rewrite_search_index("window.search = {};\n")


class ApplyTests(unittest.TestCase):
    def setUp(self):
        self.site = Path(tempfile.mkdtemp(prefix="site-pretty-"))
        self.addCleanup(shutil.rmtree, self.site, ignore_errors=True)
        (self.site / "VERSIONS").write_text(VERSIONS, encoding="utf-8")
        (self.site / "index.html").write_text('<a href="book/introduction.html">Docs</a>\n',
                                              encoding="utf-8")
        for book in ("book", "beta", "dev"):
            d = self.site / book
            (d / "tutorial").mkdir(parents=True)
            (d / "index.html").write_text(
                '<a href="tutorial/first-sync.html#here">t</a><a href="print.html">p</a>'
                '<a href="../beta/index.html">beta</a>\n', encoding="utf-8")
            (d / "introduction.html").write_text('<h1 id="install">I</h1>\n', encoding="utf-8")
            (d / "print.html").write_text("<p>all</p>\n", encoding="utf-8")
            (d / "tutorial" / "first-sync.html").write_text(
                '<h1 id="here">T</h1><a href="../introduction.html#install">i</a>\n', encoding="utf-8")
            (d / "toc-abc123.js").write_text(TOC, encoding="utf-8")
            (d / "searchindex-def456.js").write_text(SEARCH, encoding="utf-8")

    def test_apply(self):
        pu.apply(self.site)
        self.assertEqual((self.site / "index.html").read_text(encoding="utf-8"),
                         '<a href="book/introduction">Docs</a>\n')
        self.assertEqual((self.site / "beta" / "index.html").read_text(encoding="utf-8"),
                         '<a href="tutorial/first-sync#here">t</a><a href="print">p</a>'
                         '<a href="../beta/">beta</a>\n')
        self.assertIn('href=\\"tutorial/first-sync\\"', (self.site / "dev" / "toc-abc123.js")
                      .read_text(encoding="utf-8").replace('"', '\\"'))
        self.assertIn('"introduction#install"',
                      (self.site / "book" / "searchindex-def456.js").read_text(encoding="utf-8"))
        # The files keep their names: Cloudflare serves x.html at x.
        self.assertTrue((self.site / "beta" / "tutorial" / "first-sync.html").is_file())
        self.assertEqual(check.check_links(self.site), [])
        self.assertEqual(check.check_html_links(self.site), [])

    def test_a_book_without_its_sidebar_script_stops(self):
        (self.site / "beta" / "toc-abc123.js").unlink()
        with self.assertRaisesRegex(pu.PrettyError, "beta: no toc"):
            pu.apply(self.site)

    def test_a_sidebar_script_without_links_stops(self):
        (self.site / "dev" / "toc-abc123.js").write_text("var x = 1;\n", encoding="utf-8")
        with self.assertRaisesRegex(pu.PrettyError, "dev/toc-abc123.js"):
            pu.apply(self.site)

    def test_cli_error(self):
        (self.site / "VERSIONS").unlink()
        r = subprocess.run([sys.executable, str(TOOL), str(self.site)],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 1)
        self.assertTrue(r.stderr.startswith("pretty_urls.py: "), r.stderr)


if __name__ == "__main__":
    unittest.main()
