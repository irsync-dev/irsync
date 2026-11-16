"""Tests for tools/build-man.sh, which builds the man pages from the book.

Needs pandoc and mandoc. A missing tool skips the tests, unless
MAN_TESTS_REQUIRED=1, which makes it a failure.
"""
import os
import re
import shutil
import subprocess
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SCRIPT = os.path.join(ROOT, "tools", "build-man.sh")
DATE = "2026-01-02"
TOOLS = ["pandoc", "mandoc"]
# Everything the script reads, relative to the repository root.
INPUTS = [
    "tools/build-man.sh",
    "packaging/man",
    "packaging/dist/dist.toml",
    "book/src/reference/command-line.md",
    "book/src/reference/management.md",
    "book/src/reference/remote-addresses.md",
    "book/src/reference/files-and-state.md",
    "book/src/reference/limitations.md",
    "book/src/reference/configuration.md",
    "book/src/fileset/reference.md",
]


def read_version():
    with open(os.path.join(ROOT, "packaging", "dist", "dist.toml")) as f:
        return re.search(r'^version = "([^"]+)"', f.read(), re.M).group(1)


VERSION = read_version()


def setUpModule():
    for tool in TOOLS:
        if shutil.which(tool) is None:
            if os.environ.get("MAN_TESTS_REQUIRED") == "1":
                raise AssertionError(f"required tool missing: {tool}")
            raise unittest.SkipTest(f"tool not installed: {tool}")


def build(script, out):
    env = dict(os.environ, IRSYNC_MAN_DATE=DATE)
    return subprocess.run([script, out], capture_output=True, text=True, env=env)


def render(page, width=80):
    man = subprocess.run(["mandoc", "-T", "utf8", "-O", f"width={width}", page],
                         capture_output=True, text=True, check=True).stdout
    return subprocess.run(["col", "-bx"], input=man,
                          capture_output=True, text=True, check=True).stdout


def read(path):
    with open(path) as f:
        return f.read()


def section(text, name, subsections=True):
    """The rendered text of one section, without its heading. Without
    subsections, it stops at the first subsection heading."""
    lines = text.splitlines()
    start = lines.index(name)
    end = start + 1
    while end < len(lines):
        line = lines[end]
        if line and not line[0].isspace():
            break
        if not subsections and re.match(r"^ {3}\S", line):
            break
        end += 1
    return "\n".join(lines[start + 1:end])


def roff_section(roff, name):
    """The roff source of one section, without its .SH line."""
    m = re.search(r"^\.SH " + re.escape(name) + r"\n(.*?)(?=^\.SH |\Z)", roff, re.M | re.S)
    return m.group(1)


def plain(roff):
    """Roff text with font changes and escapes taken out."""
    roff = re.sub(r"\\f(\[\w*\]|\w)", "", roff)
    return roff.replace("\\-", "-").replace("\\&", "").replace("\\e", "\\")


def tags(roff):
    """The tags of the .TP items in roff source, as plain text."""
    return [plain(t) for t in re.findall(r"^\.TP\n(.*)$", roff, re.M)]


def copy_inputs(tree):
    """Copies everything tools/build-man.sh reads to tree."""
    for rel in INPUTS:
        src, dst = os.path.join(ROOT, rel), os.path.join(tree, rel)
        if os.path.isdir(src):
            shutil.copytree(src, dst)
        else:
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            shutil.copy2(src, dst)


class BuildManTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.mkdtemp(prefix="build-man-")
        cls.out = os.path.join(cls.tmp, "out")
        r = build(SCRIPT, cls.out)
        if r.returncode:
            raise AssertionError(f"build-man.sh failed ({r.returncode}):\n{r.stderr}")
        cls.irsync = os.path.join(cls.out, "man1", "irsync.1")
        cls.fileset = os.path.join(cls.out, "man1", "fileset.1")
        cls.toml = os.path.join(cls.out, "man5", "irsync.toml.5")
        cls.pages = [cls.irsync, cls.fileset, cls.toml]

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tmp, ignore_errors=True)

    def test_builds_three_pages(self):
        for rel in ("man1/irsync.1", "man1/fileset.1", "man5/irsync.toml.5"):
            self.assertTrue(os.path.isfile(os.path.join(self.out, rel)), rel)

    def test_lint_is_clean(self):
        for page in self.pages:
            r = subprocess.run(["mandoc", "-T", "lint", "-W", "warning", page],
                               capture_output=True, text=True)
            self.assertEqual((r.returncode, r.stdout), (0, ""), page)

    def test_title_block(self):
        # version from packaging/dist/dist.toml, date from IRSYNC_MAN_DATE
        text = render(self.irsync)
        self.assertIn("IRSYNC(1)", text)
        self.assertIn(f"irsync {VERSION}", text)
        self.assertIn(DATE, text)
        self.assertIn("IRSYNC.TOML(5)", render(self.toml))

    def test_flags_keep_their_dashes(self):
        text = render(self.irsync)
        for flag in ("--dry-run", "--archive", "--no-recursive", "--dial-retries"):
            self.assertIn(flag, text)
        for page in self.pages:
            self.assertNotIn("–", render(page))  # en dash
            self.assertNotIn("—", render(page))  # em dash

    def test_irsync_section_order(self):
        heads = re.findall(r"^\.SH (.+)$", read(self.irsync), re.M)
        self.assertEqual(heads[:5], ["NAME", "SYNOPSIS", "DESCRIPTION", "EXAMPLES", "OPERANDS"])
        self.assertEqual(heads[-6:], ["REMOTE ADDRESSES", "ENVIRONMENT", "FILES",
                                      "EXIT STATUS", "LIMITATIONS", "SEE ALSO"])
        self.assertIn("MANAGEMENT COMMANDS", heads)
        self.assertLess(heads.index("SUBCOMMANDS"), heads.index("MANAGEMENT COMMANDS"))
        for sub in ("HOST", "EXPORT", "GRANT"):  # subsections, not sections
            self.assertNotIn(sub, heads)

    def test_every_page_has_a_synopsis(self):
        for page in self.pages:
            self.assertIn("\n.SH SYNOPSIS\n", read(page), page)
        fileset = section(render(self.fileset), "SYNOPSIS")
        for command in ("init", "update", "check", "list"):
            self.assertRegex(fileset, r"fileset " + command + r"\b")
        self.assertIn("~/.config/irsync/irsync.toml", section(render(self.toml), "SYNOPSIS"))
        # The normal synopsis layout: at the section's indent, not indented
        # further as a code example is. That indent is 7 columns in most
        # mandoc versions and 5 in FreeBSD's, so take it from the NAME line.
        for page in self.pages:
            text = render(page)
            name = next(l for l in section(text, "NAME").splitlines() if l.strip())
            indent = len(name) - len(name.lstrip())
            self.assertIn(indent, (5, 7), page)
            for line in section(text, "SYNOPSIS").splitlines():
                if line.strip():
                    self.assertRegex(line, rf"^ {{{indent}}}\S", page)

    def test_description_introduces_the_terms(self):
        text = section(render(self.irsync), "DESCRIPTION")
        for term in ("iroh", "EndpointId", "relay", "export", "grant",
                     "immediate grant", "daemon"):
            self.assertIn(term, text)

    def test_examples(self):
        lines = [l.strip() for l in section(render(self.irsync), "EXAMPLES").splitlines()]
        commands = [l for l in lines if l.startswith("irsync ")]
        self.assertTrue([c for c in commands if re.search(r" myserver:\S*$", c)
                         and len(c.split()) > 2], "a push")
        self.assertTrue([c for c in commands if re.search(r" myserver:\S* \S+$", c)], "a pull")
        self.assertIn("irsync myserver:", commands)
        self.assertIn("ssh", "\n".join(lines))

    def test_options_are_tagged_lists(self):
        for page in self.pages:
            for table in re.findall(r"^\.TS\n(.*?)^\.TE$", read(page), re.M | re.S):
                self.assertNotIn("--", plain(table), page)
        terms = tags(read(self.irsync))
        self.assertIn("--paranoid", terms)
        self.assertTrue([t for t in terms if "--dry-run" in t], terms)
        self.assertIn("--paranoid", tags(read(self.fileset)))

    def test_tagged_lists_reflow(self):
        def entry(text):
            lines = text.splitlines()
            start = [i for i, l in enumerate(lines) if l.strip() == "--paranoid"][0]
            end = lines.index("", start)
            return lines[start:end]
        narrow, wide = entry(render(self.irsync, 80)), entry(render(self.irsync, 140))
        self.assertLess(len(wide), len(narrow))

    def test_no_book_phrasing(self):
        for page in self.pages:
            text = render(page)
            for phrase in ("next table", "next section", "linked below", "See below",
                           "see below", "(below)", "page linked"):
                self.assertNotIn(phrase, text, page)

    def test_same_page_links_name_the_section(self):
        # "Setup over ssh" is a subsection of OPERANDS; "Copying one file"
        # one of REMOTE ADDRESSES, which comes from another chapter.
        operands = section(render(self.irsync), "OPERANDS", subsections=False)
        self.assertIn("Setup over ssh", operands)
        self.assertIn("Copying one file", operands)
        self.assertIn("COMPARISON TIERS", section(render(self.fileset), "OPTIONS"))
        # A link to a chapter of another page names that page.
        self.assertIn("REMOTE ADDRESSES in irsync(1)", render(self.toml))

    def test_line_length(self):
        # Literal examples that cannot be shortened are exempt: a 64-digit
        # hash (a checksum, an EndpointId), a line of fileset's output with
        # its nanosecond timestamp, and a URL alone on its line.
        for page in self.pages:
            for line in render(page).splitlines():
                if len(line) <= 80 or re.search(r"[0-9a-f]{64}|=\d{4}-\d\d-\d\dT[\d:.]+Z", line):
                    continue
                if re.fullmatch(r"\s*([^:]+: )?https://\S+", line):
                    continue
                self.fail(f"{os.path.basename(page)}: {len(line)} columns: {line!r}")

    def test_help_is_not_called_rsync_compatible(self):
        text = render(self.irsync)
        self.assertNotIn("-h, --help", section(text, "RSYNC-COMPATIBLE FLAGS", subsections=False))
        flags = " ".join(section(text, "RSYNC-COMPATIBLE FLAGS").split())
        self.assertIn("-h is --human-readable, except on its own, where it prints the help", flags)

    def test_rsync_options_irsync_lacks(self):
        text = section(render(self.irsync), "LIMITATIONS")
        for option in ("--xattrs", "--files-from", "--link-dest"):
            self.assertIn(option, text)

    def test_files_section(self):
        text = section(render(self.irsync), "FILES")
        for name in ("irsync.toml", "id_ed25519", "~/.local/state/irsync"):
            self.assertIn(name, text)
        text = section(render(self.toml), "FILES")
        self.assertIn("~/.config/irsync/irsync.toml", text)
        self.assertIn("irsync(1)", text)

    def test_remote_addresses_is_in_the_page(self):
        text = render(self.irsync)
        self.assertNotIn("remote-addresses.html", text)
        self.assertIn("Resolution order", section(text, "REMOTE ADDRESSES"))

    def test_fileset_terms_and_placeholders(self):
        text = render(self.fileset)
        description = " ".join(section(text, "DESCRIPTION").split())
        self.assertRegex(description, r"fjall[^.]*backend|backend[^.]*fjall")
        self.assertIn("checksum", description)  # what a manifest is for
        for page in self.pages:
            for placeholder in ("<PATH>", "<MPATH>", "<N>", "<FIELD>"):
                self.assertNotIn(placeholder, render(page), page)
        media = section(text, "READ-ONLY MEDIA")
        words = " ".join(w for w in media.split() if "https://" not in w)
        self.assertGreaterEqual(len(re.findall(r"[.!?](\s|$)", words)), 2, media)

    def test_daemon_keys_in_sample_order(self):
        sample = section(render(self.toml), "THE WHOLE FILE").splitlines()
        start = [i for i, l in enumerate(sample) if l.strip() == "[daemon]"][0]
        keys = []
        for line in sample[start + 1:]:
            if not line.strip():
                break
            m = re.match(r"\s*(\w+) =", line)
            if m:  # not a comment
                keys.append(m.group(1))
        self.assertIn("allow_root", keys)
        documented = tags(roff_section(read(self.toml), "DAEMON"))
        self.assertEqual(documented[:len(keys)], keys)

    def test_url_budget(self):
        budget = {self.irsync: 6, self.fileset: 4, self.toml: 4}
        for page, most in budget.items():
            urls = re.findall(r"https://irsync\.org/book/\S*", render(page))
            self.assertLessEqual(len(urls), most, f"{page}: {urls}")

    @unittest.skipUnless(shutil.which("groff"), "groff not installed")
    def test_groff_renders_without_warnings(self):
        # Linux man-db renders with groff. -k runs preconv for UTF-8 input,
        # as man-db does; -ww turns on every warning, -z drops the output,
        # -t runs tbl for the tables. "cannot adjust line" only means a
        # ragged line at a word too long to break, such as a URL. Not run on
        # a Mac without groff; Linux CI runs it.
        for page in self.pages:
            r = subprocess.run(["groff", "-k", "-t", "-man", "-Tutf8", "-ww", "-z", page],
                               capture_output=True, text=True)
            warnings = [l for l in r.stderr.splitlines() if "cannot adjust line" not in l]
            self.assertEqual((r.returncode, warnings), (0, []), f"{page}:\n{r.stderr}")

    def test_cross_references(self):
        self.assertIn("irsync.toml(5)", render(self.irsync))
        self.assertIn("irsync(1)", render(self.toml))
        # A how-to guide keeps its URL; an explanation is named as a page
        # of the book.
        text = render(self.irsync)
        self.assertIn("https://irsync.org/book/how-to/seed-the-index.html", text)
        self.assertNotIn("https://irsync.org/book/explanation/", text)
        self.assertIn("in the book", text)

    def test_repeated_book_links_say_in_the_book(self):
        # A book page's URL is given once; later links to it still say
        # where the page is.
        text = " ".join(render(self.irsync).split())
        self.assertEqual(text.count("https://irsync.org/book/how-to/clean-up.html"), 1)
        self.assertIn("Clean up state: https://irsync.org/book/how-to/clean-up.html", text)
        self.assertIn("is in \u201cClean up state\u201d in the book.", text)
        self.assertIn("See \u201cSeed the index\u201d in the book.", text)
        files = " ".join(section(render(self.irsync), "FILES").split())
        self.assertEqual(files.count("Clean up state"),
                         files.count("\u201cClean up state\u201d in the book"))
        # The title is quoted, so "in the book" never reads as part of it, and
        # no sentence makes the book do what the linked page does.
        for page in self.pages:
            text = " ".join(render(page).split())
            self.assertNotRegex(text, r"[^\u201d] in the book", page)
        files = " ".join(section(render(self.irsync), "FILES").split())
        self.assertNotIn("in the book removes", files)
        self.assertNotIn("setup in the book", files)
        for page in self.pages:  # no raw Markdown link targets
            self.assertNotRegex(read(page), r"\w\.md[#)]")

    def test_help_sentence_stands_alone(self):
        text = " ".join(section(render(self.irsync), "DESCRIPTION").split())
        self.assertNotIn("the same flags", text)
        self.assertIn("irsync --help lists irsync's flags, one line each", text)

    def test_fileset_commands_do_not_repeat_the_synopsis(self):
        text = render(self.fileset)
        self.assertIn("fileset init [OPTIONS] PATH", " ".join(section(text, "SYNOPSIS").split()))
        commands = section(text, "COMMANDS", subsections=False)
        self.assertNotIn("fileset init", commands)
        self.assertTrue(commands.strip().startswith("PATH is the root of the tree"), commands)

    def test_required_keys_say_required(self):
        text = " ".join(render(self.toml).split())
        self.assertNotIn("Default: required", text)
        self.assertIn("endpoint_id Required. The remote's EndpointId.", text)
        self.assertRegex(text, r"\bpath Required\. The directory")

    def test_fileset_page_has_no_install_block(self):
        self.assertNotIn("cargo install", render(self.fileset))

    def test_mdbook_directive_fails(self):
        tree = os.path.join(self.tmp, "tree")
        copy_inputs(tree)
        chapter = os.path.join(tree, "book/src/reference/management.md")
        with open(chapter, "a") as f:
            f.write("\n{{#include x.md}}\n")
        r = build(os.path.join(tree, "tools", "build-man.sh"),
                  os.path.join(self.tmp, "tree-out"))
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("{{#", r.stderr)
        self.assertIn("management.md", r.stderr)

    def test_retitled_chapter_fails(self):
        # The filter knows each chapter by its title, to rewrite links to it.
        tree = os.path.join(self.tmp, "retitled")
        copy_inputs(tree)
        chapter = os.path.join(tree, "book/src/reference/remote-addresses.md")
        text = read(chapter)
        with open(chapter, "w") as f:
            f.write(text.replace("# Remote addresses\n", "# Remote operands\n", 1))
        r = build(os.path.join(tree, "tools", "build-man.sh"),
                  os.path.join(self.tmp, "retitled-out"))
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("remote-addresses.md", r.stderr)
        self.assertIn("Remote operands", r.stderr)

    def test_same_inputs_same_output(self):
        out2 = os.path.join(self.tmp, "out2")
        r = build(SCRIPT, out2)
        self.assertEqual(r.returncode, 0, r.stderr)
        for page in self.pages:
            rel = os.path.relpath(page, self.out)
            with open(page, "rb") as a, open(os.path.join(out2, rel), "rb") as b:
                self.assertEqual(a.read(), b.read(), rel)


# What pandoc 3.1.3 writes (Ubuntu 24.04's): a block that defines the V
# fonts, .hy, V for inline code, and code blocks as .nf with the C font.
OLD_PANDOC = r"""'\" t
.\" Automatically generated by Pandoc 3.1.3
.\"
.\" Define V font for inline verbatim, using C font in formats
.\" that render this, and otherwise B font.
.ie "\f[CB]x\f[]"x" \{\
. ftr V B
. ftr VI BI
. ftr VB B
. ftr VBI BI
.\}
.el \{\
. ftr V CR
. ftr VI CI
. ftr VB CB
. ftr VBI CBI
.\}
.TH "X" "1" "2026-01-02" "x 1" "x manual"
.hy
.SH NAME
.PP
x \- an example
.SH SYNOPSIS
.nf
x [OPTIONS]
.fi
.SH DESCRIPTION
.PP
Run \f[V]x \-\-all\f[R], or \f[B]\f[VB]x\f[B]\f[R], \f[VI]y\f[R]
and \f[VBI]z\f[R].
.IP
.nf
\f[C]
x \-\-all
\f[R]
.fi
.PP
Done.
"""

# The same page from pandoc 3.1.12 or newer, less its version comment.
NEW_PANDOC = r"""'\" t
.TH "X" "1" "2026-01-02" "x 1" "x manual"
.SH NAME
x \- an example
.SH SYNOPSIS
.nf
x [OPTIONS]
.fi
.SH DESCRIPTION
Run \f[CR]x \-\-all\f[R], or \f[B]\f[CB]x\f[B]\f[R], \f[CI]y\f[R]
and \f[CBI]z\f[R].
.IP
.EX
x \-\-all
.EE
.PP
Done.
"""


class NormalizeTests(unittest.TestCase):
    """packaging/man/normalize.awk, on what an old pandoc writes: no runner
    with a new pandoc would notice it breaking."""

    def normalize(self, roff):
        return subprocess.run(["awk", "-f", os.path.join(ROOT, "packaging/man/normalize.awk")],
                              input=roff, capture_output=True, text=True)

    def test_old_pandoc_output_becomes_the_new_form(self):
        r = self.normalize(OLD_PANDOC)
        self.assertEqual((r.returncode, r.stderr), (0, ""))
        self.assertEqual(r.stdout, NEW_PANDOC)

    def test_new_form_is_left_alone(self):
        self.assertEqual(self.normalize(NEW_PANDOC).stdout, NEW_PANDOC)

    def test_result_is_clean_for_mandoc(self):
        page = os.path.join(tempfile.mkdtemp(prefix="normalize-"), "x.1")
        self.addCleanup(shutil.rmtree, os.path.dirname(page), ignore_errors=True)
        # mandoc has no bold italic code font, from any pandoc; the pages
        # do not use one.
        with open(page, "w") as f:
            f.write(self.normalize(OLD_PANDOC.replace("VBI", "VB")).stdout)
        r = subprocess.run(["mandoc", "-T", "lint", "-W", "warning", page],
                           capture_output=True, text=True)
        self.assertEqual((r.returncode, r.stdout), (0, ""))

    def test_unfinished_font_block_fails(self):
        r = self.normalize(OLD_PANDOC.replace(".\\}\n.TH", ".TH", 1))
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("does not end", r.stderr)

    def test_pages_do_not_name_the_pandoc_version(self):
        out = os.path.join(tempfile.mkdtemp(prefix="normalize-"), "out")
        self.addCleanup(shutil.rmtree, os.path.dirname(out), ignore_errors=True)
        self.assertEqual(build(SCRIPT, out).returncode, 0)
        self.assertNotIn("Pandoc", read(os.path.join(out, "man1", "irsync.1")))


if __name__ == "__main__":
    unittest.main()
