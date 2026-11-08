import tempfile
import unittest
from pathlib import Path

from strip_font_link import strip_font_link


class StripFontLinkTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.d = Path(self._tmp.name)

    def test_removes_the_hashed_and_the_plain_link_at_any_depth(self):
        top = self.d / "index.html"
        top.write_text(
            '<head>\n        <link rel="stylesheet" href="fonts/fonts-9ac3b33f.css">\n'
            '        <link rel="stylesheet" href="css/general.css">\n</head>\n'
        )
        deep = self.d / "a" / "b.html"
        deep.parent.mkdir()
        deep.write_text('<head>\n  <link rel="stylesheet" href="../fonts/fonts.css">\n</head>\n')
        self.assertEqual(strip_font_link(self.d), 2)
        self.assertEqual(
            top.read_text(),
            '<head>\n        <link rel="stylesheet" href="css/general.css">\n</head>\n',
        )
        self.assertEqual(deep.read_text(), "<head>\n</head>\n")

    def test_leaves_other_pages_alone(self):
        page = self.d / "x.html"
        page.write_text('<a href="fonts/fonts.css">a link, not a stylesheet</a>\n')
        self.assertEqual(strip_font_link(self.d), 0)


if __name__ == "__main__":
    unittest.main()
