"""Tests for packaging/installer: the generator and the installer it writes.

Each test runs the installer against a fake release of three small tarballs,
with a fake uname and a PATH of only the tools the installer needs. No network.

INSTALLER_TEST_SHELL=dash runs the installer under dash instead of sh.
"""
import hashlib
import io
import os
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BUILD = os.path.join(ROOT, "packaging", "installer", "build-installer.sh")
TEMPLATE = os.path.join(ROOT, "packaging", "installer", "irsync-installer.sh.in")
SHELL = shutil.which(os.environ.get("INSTALLER_TEST_SHELL", "sh"))

with open(os.path.join(ROOT, "packaging", "dist", "dist.toml")) as f:
    VERSION = re.search(r'^version = "([^"]+)"$', f.read(), re.M).group(1)
BASE_URL = f"https://github.com/irsync-dev/irsync/releases/download/v{VERSION}"

TARGETS = {  # target -> (uname -s, uname -m)
    "aarch64-apple-darwin": ("Darwin", "arm64"),
    "aarch64-unknown-linux-musl": ("Linux", "aarch64"),
    "x86_64-unknown-linux-musl": ("Linux", "x86_64"),
}

FILES = [".local/bin/irsync", ".local/bin/fileset",
         ".local/share/man/man1/irsync.1", ".local/share/man/man1/fileset.1",
         ".local/share/man/man5/irsync.toml.5"]
RECEIPT = ".config/irsync/irsync-receipt.json"
ENV_SH = ".config/irsync/env.sh"

# What the installer runs, besides uname, a downloader and a checksum tool.
# GNU tar runs xz to read .tar.xz; bsdtar reads it on its own.
TOOLS = ["sh", "mktemp", "rm", "mkdir", "cp", "mv", "chmod", "cat", "tar", "xz",
         "grep", "printf"]

FAKE_UNAME = """#!/bin/sh
case $1 in
  -s) echo "$FAKE_UNAME_S" ;;
  -m) echo "$FAKE_UNAME_M" ;;
  *) exit 1 ;;
esac
"""

# A checksum tool that logs its name. Hashes stdin, or the file named last.
FAKE_SUM = f"""#!{sys.executable}
import hashlib, os, sys
with open(os.environ["FAKE_LOG"], "a") as log:
    log.write(os.path.basename(sys.argv[0]) + " " + " ".join(sys.argv[1:]) + "\\n")
args = [a for a in sys.argv[1:] if a not in ("-a", "256")]
data = open(args[-1], "rb").read() if args else sys.stdin.buffer.read()
print(hashlib.sha256(data).hexdigest() + "  " + (args[-1] if args else "-"))
"""

# A downloader that logs its arguments and prints the tarball named by the
# URL from FAKE_RELEASE. `--help` lists --https-only unless FAKE_NO_HTTPS_ONLY
# is set (BusyBox wget has no such flag). FAKE_FAIL makes every download fail.
FAKE_FETCHER = """#!/bin/sh
if [ "$1" = --help ]; then
  [ -n "${FAKE_NO_HTTPS_ONLY-}" ] || echo "  --https-only   only follow secure HTTPS links"
  exit 0
fi
printf '%s\\n' "$*" >> "$FAKE_LOG"
[ -z "${FAKE_FAIL-}" ] || exit 22
for a in "$@"; do
  case $a in
    https://*) exec cat "$FAKE_RELEASE/${a##*/}" ;;
  esac
done
exit 22
"""


# sysctl for the Rosetta check: prints FAKE_SYSCTL for
# `-n sysctl.proc_translated`, and fails when it is unset, as on an Intel Mac
# where the key does not exist.
FAKE_SYSCTL = """#!/bin/sh
[ "$*" = "-n sysctl.proc_translated" ] || exit 1
[ -n "${FAKE_SYSCTL-}" ] || { echo "sysctl: unknown oid" >&2; exit 1; }
echo "$FAKE_SYSCTL"
"""

# Sends TERM to the shell that ran it, as a Ctrl-C or a kill would.
KILL_PARENT = """#!/bin/sh
kill -TERM $PPID
"""


def write_exe(path, body):
    with open(path, "w") as f:
        f.write(body)
    os.chmod(path, 0o755)


def make_tarball(path, target):
    """A release tarball whose irsync and fileset print their target."""
    top = f"irsync-{target}"
    members = {
        "irsync": f"#!/bin/sh\necho irsync {target}\n",
        "fileset": f"#!/bin/sh\necho fileset {target}\n",
        "man/man1/irsync.1": f".TH IRSYNC 1\n.SH NAME\nirsync \\- {target}\n",
        "man/man1/fileset.1": ".TH FILESET 1\n.SH NAME\nfileset \\- test\n",
        "man/man5/irsync.toml.5": ".TH IRSYNC.TOML 5\n.SH NAME\nirsync.toml \\- test\n",
        "README.md": "readme\n",
        "LICENSE": "license\n",
    }
    with tarfile.open(path, "w:xz") as tar:
        dirs = {top} | {f"{top}/{os.path.dirname(n)}" for n in members if "/" in n}
        dirs |= {f"{top}/man"}
        for d in sorted(dirs):
            info = tarfile.TarInfo(d)
            info.type = tarfile.DIRTYPE
            info.mode = 0o755
            tar.addfile(info)
        for name, body in members.items():
            data = body.encode()
            info = tarfile.TarInfo(f"{top}/{name}")
            info.size = len(data)
            info.mode = 0o755 if name in ("irsync", "fileset") else 0o644
            tar.addfile(info, io.BytesIO(data))


def make_release(directory):
    """Tarballs and .sha256 files as dist writes them. Returns target -> sha."""
    shas = {}
    for target in TARGETS:
        name = f"irsync-{target}.tar.xz"
        make_tarball(os.path.join(directory, name), target)
        with open(os.path.join(directory, name), "rb") as f:
            shas[target] = hashlib.sha256(f.read()).hexdigest()
        with open(os.path.join(directory, name + ".sha256"), "w") as f:
            f.write(f"{shas[target]}  {name}\n")
    return shas


def build(distrib, out):
    return subprocess.run([BUILD, distrib, out], capture_output=True, text=True)


def read(path):
    with open(path, "rb") as f:
        return f.read()


class Base(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root = tempfile.mkdtemp(prefix="installer-test-")
        cls.release = os.path.join(cls.root, "release")
        os.mkdir(cls.release)
        cls.shas = make_release(cls.release)
        cls.installer = os.path.join(cls.root, "irsync-installer.sh")
        r = build(cls.release, cls.installer)
        if r.returncode:
            shutil.rmtree(cls.root, ignore_errors=True)
            raise AssertionError(f"build-installer.sh failed:\n{r.stdout}{r.stderr}")

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.root, ignore_errors=True)

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="installer-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.home = os.path.join(self.tmp, "home")
        os.mkdir(self.home)
        # Each test gets its own copy of the release, free to break.
        self.archives = os.path.join(self.tmp, "release")
        shutil.copytree(self.release, self.archives)
        self.log = os.path.join(self.tmp, "calls.log")
        self.bindir = self.make_bindir("bin")

    def make_bindir(self, name, sums=("sha256sum", "shasum"), shell=SHELL, without=()):
        """A directory of the installer's tools; `sums` are the checksum tools in it,
        `without` are tools of TOOLS to leave out."""
        bindir = os.path.join(self.tmp, name)
        os.mkdir(bindir)
        for tool in TOOLS:
            if tool in without:
                continue
            found = shell if tool == "sh" else shutil.which(tool)
            if found:
                os.symlink(found, os.path.join(bindir, tool))
        write_exe(os.path.join(bindir, "uname"), FAKE_UNAME)
        for tool in sums:
            write_exe(os.path.join(bindir, tool), FAKE_SUM)
        return bindir

    def install(self, *args, home=None, uname=("Linux", "x86_64"), env=None,
                bindir=None, archives=True):
        bindir = bindir or self.bindir
        full_env = {
            "PATH": bindir,
            "HOME": home or self.home,
            "TMPDIR": self.tmp,
            "FAKE_UNAME_S": uname[0],
            "FAKE_UNAME_M": uname[1],
            "FAKE_LOG": self.log,
            "FAKE_RELEASE": self.archives,
        }
        if archives:
            full_env["IRSYNC_INSTALLER_ARCHIVE_DIR"] = self.archives
        full_env.update(env or {})
        return subprocess.run([os.path.join(bindir, "sh"), self.installer, *args],
                              env=full_env, capture_output=True, text=True)

    def under_home(self, home=None):
        """Every file and symlink under HOME, relative to it."""
        home = home or self.home
        found = set()
        for d, _, names in os.walk(home):
            for n in names:
                found.add(os.path.relpath(os.path.join(d, n), home))
        return found

    def snapshot(self, home=None):
        home = home or self.home
        return {f: read(os.path.join(home, f)) for f in self.under_home(home)}

    def make_dir(self, names):
        """An unpacked release directory holding the named files. Returns its path."""
        d = os.path.join(self.tmp, "from")
        for n in names:
            path = os.path.join(d, n)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as f:
                f.write(f"#!/bin/sh\necho {n}\n")
        return d

    def put(self, rel, body="old\n", home=None):
        path = os.path.join(home or self.home, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(body)
        return path


class GeneratorTests(Base):
    def test_fills_version_url_and_checksums(self):
        with open(self.installer) as f:
            text = f.read()
        self.assertNotRegex(text, r"@[A-Z0-9_]+@")
        self.assertIn(f"releases/download/v{VERSION}", text)
        for sha in self.shas.values():
            self.assertIn(sha, text)

    def test_template_has_every_placeholder(self):
        with open(TEMPLATE) as f:
            found = set(re.findall(r"@[A-Z0-9_]+@", f.read()))
        self.assertEqual(found, {"@VERSION@", "@BASE_URL@", "@SHA256_AARCH64_APPLE_DARWIN@",
                                 "@SHA256_AARCH64_UNKNOWN_LINUX_MUSL@",
                                 "@SHA256_X86_64_UNKNOWN_LINUX_MUSL@"})

    def test_missing_checksum_leaves_no_output(self):
        os.remove(os.path.join(self.archives, "irsync-aarch64-apple-darwin.tar.xz.sha256"))
        out = os.path.join(self.tmp, "irsync-installer.sh")
        r = build(self.archives, out)
        self.assertNotEqual(r.returncode, 0)
        self.assertFalse(os.path.exists(out))
        self.assertEqual(os.listdir(self.tmp).count("irsync-installer.sh"), 0)

    def test_malformed_checksum_leaves_no_output(self):
        for bad in ["", "not-hex  irsync-x86_64-unknown-linux-musl.tar.xz\n",
                    "abc123  irsync-x86_64-unknown-linux-musl.tar.xz\n"]:
            with open(os.path.join(self.archives,
                                   "irsync-x86_64-unknown-linux-musl.tar.xz.sha256"), "w") as f:
                f.write(bad)
            out = os.path.join(self.tmp, "irsync-installer.sh")
            r = build(self.archives, out)
            self.assertNotEqual(r.returncode, 0, repr(bad))
            self.assertFalse(os.path.exists(out), repr(bad))

    def test_failure_removes_a_stale_output(self):
        out = os.path.join(self.tmp, "irsync-installer.sh")
        self.assertEqual(build(self.archives, out).returncode, 0)
        self.assertTrue(os.path.exists(out))
        os.remove(os.path.join(self.archives, "irsync-x86_64-unknown-linux-musl.tar.xz.sha256"))
        r = build(self.archives, out)
        self.assertNotEqual(r.returncode, 0)
        self.assertFalse(os.path.exists(out))

    def test_generator_ignores_cdpath(self):
        # With CDPATH set, `cd packaging/installer` would go to the decoy and
        # print its path, and the script would read the wrong template.
        decoy = os.path.join(self.tmp, "decoy")
        os.makedirs(os.path.join(decoy, "packaging", "installer"))
        out = os.path.join(self.tmp, "irsync-installer.sh")
        r = subprocess.run(["packaging/installer/build-installer.sh", self.archives, out],
                           cwd=ROOT, env=dict(os.environ, CDPATH=decoy),
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(read(out), read(self.installer))

    def test_leaves_no_temporary_files(self):
        outdir = os.path.join(self.tmp, "out")
        os.mkdir(outdir)
        out = os.path.join(outdir, "irsync-installer.sh")
        self.assertEqual(build(self.archives, out).returncode, 0)
        os.remove(os.path.join(self.archives, "irsync-x86_64-unknown-linux-musl.tar.xz.sha256"))
        self.assertNotEqual(build(self.archives, out).returncode, 0)
        self.assertEqual(os.listdir(outdir), [])


class InstallTests(Base):
    def test_installs_five_files(self):
        r = self.install(home=self.home)
        self.assertEqual(r.returncode, 0, r.stderr)
        for f in FILES:
            self.assertTrue(os.path.isfile(os.path.join(self.home, f)), f)
        self.assertTrue(os.access(os.path.join(self.home, FILES[0]), os.X_OK))
        self.assertEqual(stat.S_IMODE(os.stat(os.path.join(self.home, FILES[0])).st_mode), 0o755)
        self.assertEqual(stat.S_IMODE(os.stat(os.path.join(self.home, FILES[1])).st_mode), 0o755)
        for f in FILES[2:]:
            self.assertEqual(stat.S_IMODE(os.stat(os.path.join(self.home, f)).st_mode), 0o644, f)
        self.assertFalse(os.path.exists(os.path.join(self.home, RECEIPT)))
        self.assertEqual(self.under_home(), set(FILES))
        self.assertIn(f"installed irsync {VERSION}", r.stderr)
        self.assertIn("man irsync", r.stderr)
        self.assertEqual(r.stdout, "")

    def test_each_platform_picks_its_tarball(self):
        for target, uname in TARGETS.items():
            with self.subTest(uname=uname):
                home = os.path.join(self.tmp, target)
                os.mkdir(home)
                r = self.install(home=home, uname=uname)
                self.assertEqual(r.returncode, 0, r.stderr)
                out = subprocess.run([os.path.join(home, ".local/bin/irsync")],
                                     capture_output=True, text=True).stdout
                self.assertEqual(out, f"irsync {target}\n")
        for uname in [("Linux", "arm64"), ("Linux", "amd64")]:
            with self.subTest(uname=uname):
                home = os.path.join(self.tmp, "-".join(uname))
                os.mkdir(home)
                r = self.install(home=home, uname=uname)
                self.assertEqual(r.returncode, 0, r.stderr)
                out = subprocess.run([os.path.join(home, ".local/bin/irsync")],
                                     capture_output=True, text=True).stdout
                arch = "aarch64" if uname[1] == "arm64" else "x86_64"
                self.assertEqual(out, f"irsync {arch}-unknown-linux-musl\n")

    def test_unsupported_platform(self):
        for uname in [("Linux", "riscv64"), ("Darwin", "x86_64"), ("FreeBSD", "amd64")]:
            with self.subTest(uname=uname):
                r = self.install(uname=uname)
                self.assertNotEqual(r.returncode, 0)
                self.assertIn("https://github.com/irsync-dev/irsync/releases", r.stderr)
                for target in TARGETS:
                    self.assertIn(target, r.stderr)
                self.assertEqual(self.under_home(), set())

    def test_bad_checksum_changes_nothing(self):
        self.assertEqual(self.install().returncode, 0)
        before = self.snapshot()
        path = os.path.join(self.archives, "irsync-x86_64-unknown-linux-musl.tar.xz")
        make_tarball(path, "aarch64-unknown-linux-musl")  # a valid tarball, wrong bytes
        r = self.install()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("checksum", r.stderr)
        self.assertEqual(self.snapshot(), before)

    def test_missing_archive_changes_nothing(self):
        self.assertEqual(self.install().returncode, 0)
        before = self.snapshot()
        os.remove(os.path.join(self.archives, "irsync-x86_64-unknown-linux-musl.tar.xz"))
        r = self.install()
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual(self.snapshot(), before)

    def test_fresh_home_untouched_on_failure(self):
        os.remove(os.path.join(self.archives, "irsync-x86_64-unknown-linux-musl.tar.xz"))
        self.assertNotEqual(self.install().returncode, 0)
        self.assertEqual(os.listdir(self.home), [])

    def test_home_with_a_space(self):
        home = os.path.join(self.tmp, "x y")
        os.mkdir(home)
        r = self.install(home=home)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.under_home(home), set(FILES))
        r = self.install("--uninstall", home=home)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.under_home(home), set())

    def test_upgrade_over_dist_install(self):
        self.put(".local/bin/irsync", "#!/bin/sh\necho old\n")
        self.put(".local/bin/fileset", "#!/bin/sh\necho old\n")
        self.put(RECEIPT, '{"version": "0.1.0-beta.1"}\n')
        self.put(ENV_SH, 'export PATH="$HOME/.local/bin:$PATH"\n')
        before = self.snapshot()
        r = self.install()
        self.assertEqual(r.returncode, 0, r.stderr)
        after = self.snapshot()
        out = subprocess.run([os.path.join(self.home, ".local/bin/irsync")],
                             capture_output=True, text=True).stdout
        self.assertEqual(out, "irsync x86_64-unknown-linux-musl\n")
        self.assertNotEqual(after[".local/bin/fileset"], before[".local/bin/fileset"])
        self.assertEqual(after[RECEIPT], before[RECEIPT])
        self.assertEqual(after[ENV_SH], before[ENV_SH])
        self.assertEqual(set(after), set(FILES) | {RECEIPT, ENV_SH})

    def test_path_hint_only_when_needed(self):
        r = self.install()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn(".local/bin", r.stderr)
        self.assertEqual(self.under_home(), set(FILES))

        home = os.path.join(self.tmp, "home2")
        os.mkdir(home)
        path = os.path.join(home, ".local", "bin") + os.pathsep + self.bindir
        r = self.install(home=home, env={"PATH": path})
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertNotIn(".local/bin", r.stderr)
        self.assertEqual(self.under_home(home), set(FILES))

    def path_hint(self, name, **kwargs):
        home = os.path.join(self.tmp, name)
        os.mkdir(home)
        r = self.install(home=home, **kwargs)
        self.assertEqual(r.returncode, 0, r.stderr)
        return r.stderr

    def test_path_hint_says_to_log_in_again_on_linux(self):
        # Most Linux systems add ~/.local/bin at login once it exists; macOS
        # does not.
        err = self.path_hint("linux", env={"SHELL": "/bin/zsh"})
        self.assertIn("Log out and in again first", err)
        self.assertIn("If it is still missing then, put this line in ~/.zshrc:", err)
        err = self.path_hint("mac", uname=("Darwin", "arm64"), env={"SHELL": "/bin/zsh"})
        self.assertNotIn("Log out", err)
        self.assertIn("is not on your PATH. To add it, put this line in ~/.zshrc:", err)

    def test_path_hint_names_the_shells_startup_file(self):
        export = '\n  export PATH="$HOME/.local/bin:$PATH"\n'
        for shell, uname, want in [
            ("/bin/zsh", "Linux", "put this line in ~/.zshrc:" + export),
            ("/usr/bin/bash", "Linux", "put this line in ~/.bashrc:" + export),
            # A terminal on macOS starts a login shell.
            ("/bin/bash", "Darwin", "put this line in ~/.bash_profile:" + export),
            ("/bin/sh", "Linux", "put this line in ~/.profile:" + export),
            ("/usr/bin/dash", "Linux", "put this line in ~/.profile:" + export),
            ("/bin/ksh", "Linux", "put this line in ~/.profile:" + export),
            ("/usr/bin/fish", "Linux", "run: fish_add_path ~/.local/bin\n"),
            ("/bin/tcsh", "Darwin", "put this line in ~/.cshrc:\n  set path = (~/.local/bin $path)\n"),
        ]:
            with self.subTest(shell=shell, uname=uname):
                arch = "arm64" if uname == "Darwin" else "x86_64"
                err = self.path_hint(f"{uname}-{os.path.basename(shell)}",
                                     uname=(uname, arch), env={"SHELL": shell})
                self.assertIn(want, err)
                if "fish" in shell or "csh" in shell:
                    self.assertNotIn("export PATH", err)

    def test_path_hint_for_an_unknown_shell(self):
        err = self.path_hint("nu", env={"SHELL": "/usr/bin/nu"})
        self.assertIn("add it in your shell's startup file", err)
        self.assertIn("does not know your shell, nu)", err)
        self.assertIn('In sh, bash or zsh the line is:\n  export PATH="$HOME/.local/bin:$PATH"\n', err)
        # Empty, not unset: bash as sh sets a missing SHELL itself.
        err = self.path_hint("empty", env={"SHELL": ""})
        self.assertIn("SHELL is not set", err)
        self.assertIn("In sh, bash or zsh the line is:", err)

    def test_no_modify_path_is_accepted(self):
        r = self.install(env={"IRSYNC_NO_MODIFY_PATH": "1"})
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.under_home(), set(FILES))

    def test_no_modify_path_flag_is_accepted(self):
        startup = [".profile", ".zshrc", ".bashrc"]
        for f in startup:
            self.put(f, f"# {f}\n")
        before = self.snapshot()
        r = self.install("--no-modify-path")
        self.assertEqual(r.returncode, 0, r.stderr)
        after = self.snapshot()
        self.assertEqual(set(after), set(FILES) | set(startup))
        for f in startup:
            self.assertEqual(after[f], before[f], f)

    def test_help_lists_no_modify_path(self):
        r = self.install("--help")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("--no-modify-path", r.stdout)
        r = self.install("--no-modify-path", "--help")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("--uninstall", r.stdout)
        self.assertEqual(self.under_home(), set())

    def test_rosetta_shell_gets_the_arm_build(self):
        write_exe(os.path.join(self.bindir, "sysctl"), FAKE_SYSCTL)
        r = self.install(uname=("Darwin", "x86_64"), env={"FAKE_SYSCTL": "1"})
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("Rosetta", r.stderr)
        out = subprocess.run([os.path.join(self.home, ".local/bin/irsync")],
                             capture_output=True, text=True).stdout
        self.assertEqual(out, "irsync aarch64-apple-darwin\n")

    def test_intel_mac_is_still_refused(self):
        write_exe(os.path.join(self.bindir, "sysctl"), FAKE_SYSCTL)
        for env in [{"FAKE_SYSCTL": "0"}, {}]:
            with self.subTest(env=env):
                r = self.install(uname=("Darwin", "x86_64"), env=env)
                self.assertNotEqual(r.returncode, 0)
                self.assertIn("https://github.com/irsync-dev/irsync/releases", r.stderr)
                self.assertNotIn("Rosetta", r.stderr)
                self.assertEqual(self.under_home(), set())

    def test_rosetta_check_only_on_darwin_x86_64(self):
        # A Linux x86_64 host with a sysctl that would say "translated".
        write_exe(os.path.join(self.bindir, "sysctl"), FAKE_SYSCTL)
        r = self.install(env={"FAKE_SYSCTL": "1"})
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertNotIn("Rosetta", r.stderr)
        out = subprocess.run([os.path.join(self.home, ".local/bin/irsync")],
                             capture_output=True, text=True).stdout
        self.assertEqual(out, "irsync x86_64-unknown-linux-musl\n")

    def test_man_line_mentions_path_when_hint_shown(self):
        r = self.install()
        self.assertEqual(r.returncode, 0, r.stderr)
        last = r.stderr.rstrip("\n").split("\n")[-1]
        self.assertIn("man irsync", last)
        self.assertIn("PATH", last)

        home = os.path.join(self.tmp, "home2")
        os.mkdir(home)
        path = os.path.join(home, ".local", "bin") + os.pathsep + self.bindir
        r = self.install(home=home, env={"PATH": path})
        self.assertEqual(r.returncode, 0, r.stderr)
        last = r.stderr.rstrip("\n").split("\n")[-1]
        self.assertIn("man irsync", last)
        self.assertNotIn("PATH", last)

    def test_home_with_trailing_slash_no_false_hint(self):
        path = os.path.join(self.home, ".local", "bin") + os.pathsep + self.bindir
        r = self.install(home=self.home + "/", env={"PATH": path})
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertNotIn("not on your PATH", r.stderr)
        self.assertEqual(self.under_home(), set(FILES))

    def test_does_not_edit_startup_files(self):
        startup = [".profile", ".zshrc", ".bashrc", ".bash_profile", ".zshenv"]
        for f in startup:
            self.put(f, f"# {f}\n")
        before = self.snapshot()
        for shell in ["/bin/sh", "/bin/zsh", "/bin/bash", "/usr/bin/fish"]:
            r = self.install(env={"SHELL": shell})
            self.assertEqual(r.returncode, 0, r.stderr)
        after = self.snapshot()
        for f in startup:
            self.assertEqual(after[f], before[f], f)
        self.assertEqual(set(after), set(FILES) | set(startup))

    def test_unknown_argument(self):
        for args in [("--frobnicate",), ("--uninstall", "--help"),
                     ("--no-modify-path", "--frobnicate"), ("--uninstall", "--uninstall")]:
            with self.subTest(args=args):
                r = self.install(*args)
                self.assertEqual(r.returncode, 2)
                self.assertIn("usage", r.stderr)
                self.assertEqual(self.under_home(), set())

    def test_help(self):
        r = self.install("--help")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("--uninstall", r.stdout)
        self.assertEqual(self.under_home(), set())

    def test_checksum_with_sha256sum(self):
        bindir = self.make_bindir("bin-sha256sum", sums=["sha256sum"])
        r = self.install(bindir=bindir)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(read(self.log).startswith(b"sha256sum"))

    def test_checksum_with_shasum(self):
        bindir = self.make_bindir("bin-shasum", sums=["shasum"])
        r = self.install(bindir=bindir)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(read(self.log).startswith(b"shasum -a 256"))

    def test_no_checksum_tool(self):
        bindir = self.make_bindir("bin-nosum", sums=[])
        r = self.install(bindir=bindir)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("sha256sum", r.stderr)
        self.assertEqual(self.under_home(), set())

    def test_tar_without_xz(self):
        bindir = self.make_bindir("bin-oldtar")
        os.remove(os.path.join(bindir, "tar"))
        write_exe(os.path.join(bindir, "tar"),
                  "#!/bin/sh\necho 'tar: invalid option -- J' >&2\nexit 1\n")
        r = self.install(bindir=bindir)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("xz", r.stderr)
        self.assertEqual(self.under_home(), set())


@unittest.skipIf(os.geteuid() == 0, "root can write to a 0555 directory")
class UnwritableTests(Base):
    """A target directory the installer cannot write to."""

    def lock(self, rel):
        """Create HOME/rel and make it read-only for the rest of the test."""
        path = os.path.join(self.home, rel)
        os.makedirs(path, exist_ok=True)
        os.chmod(path, 0o555)
        # Cleanups run last first, so this runs before setUp's rmtree.
        self.addCleanup(os.chmod, path, 0o755)
        return path

    def test_unwritable_share_installs_the_programs_only(self):
        self.lock(".local/share")
        man1 = os.path.join(self.home, ".local/share/man/man1")
        on_path = os.path.join(self.home, ".local", "bin") + os.pathsep + self.bindir
        for env in [None, {"PATH": on_path}]:
            with self.subTest(env=env):
                r = self.install(env=env)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual(self.under_home(), set(FILES[:2]))
                self.assertIn(man1, r.stderr)
                self.assertIn("manual pages were not installed", r.stderr)
                self.assertNotIn("man irsync", r.stderr)
                self.assertIn(f"installed irsync {VERSION}", r.stderr)

    def test_one_unwritable_man_dir_skips_every_page(self):
        man5 = self.lock(".local/share/man/man5")
        r = self.install()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.under_home(), set(FILES[:2]))
        self.assertIn(man5, r.stderr)
        self.assertIn("manual pages were not installed", r.stderr)
        self.assertNotIn("man irsync", r.stderr)

    def test_unwritable_bin_changes_nothing(self):
        for f in FILES:
            self.put(f)
        bindir = self.lock(".local/bin")
        before = self.snapshot()
        r = self.install()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn(bindir, r.stderr)
        self.assertNotIn("installed irsync", r.stderr)
        self.assertEqual(self.snapshot(), before)
        self.assertEqual([n for n in os.listdir(self.tmp) if n.startswith("irsync-installer.")], [])

    def test_unwritable_bin_with_from_changes_nothing(self):
        for f in FILES:
            self.put(f)
        self.lock(".local/bin")
        d = os.path.join(self.tmp, "from")
        os.makedirs(d)
        with open(os.path.join(d, "irsync"), "w") as f:
            f.write("#!/bin/sh\n")
        before = self.snapshot()
        r = self.install("--from", d, archives=False)
        self.assertEqual(r.returncode, 1)
        self.assertEqual(self.snapshot(), before)

    def test_unwritable_bin_in_a_fresh_home_installs_no_pages(self):
        self.lock(".local/bin")
        r = self.install()
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual(self.under_home(), set())
        self.assertFalse(os.path.exists(os.path.join(self.home, ".local/share")))


class FromDirTests(Base):
    """--from DIR: install from an unpacked release directory."""

    ALL = ["irsync", "fileset", "man/man1/irsync.1", "man/man1/fileset.1",
           "man/man5/irsync.toml.5"]

    def test_from_installs_five_files(self):
        r = self.install("--from", self.make_dir(self.ALL), archives=False)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.under_home(), set(FILES))
        self.assertTrue(os.access(os.path.join(self.home, ".local/bin/irsync"), os.X_OK))

    def test_from_irsync_only(self):
        r = self.install("--from", self.make_dir(["irsync"]), archives=False)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.under_home(), {".local/bin/irsync"})
        self.assertIn("fileset", r.stderr)
        self.assertIn("manual pages", r.stderr)

    def test_from_some_pages_installs_no_pages(self):
        r = self.install("--from", self.make_dir(["irsync", "man/man1/irsync.1"]),
                         archives=False)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.under_home(), {".local/bin/irsync"})

    def test_from_without_irsync_changes_nothing(self):
        r = self.install("--from", self.make_dir(["fileset"]), archives=False)
        self.assertEqual(r.returncode, 1)
        self.assertEqual(self.under_home(), set())

    def test_from_needs_no_tools_and_no_platform(self):
        # No tar, xz or checksum tool on PATH, and an unsupported platform.
        r = self.install("--from", self.make_dir(self.ALL), archives=False,
                         uname=("SunOS", "sparc"),
                         bindir=self.make_bindir("bare", sums=(), without=("tar", "xz")))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.under_home(), set(FILES))

    def test_from_usage_errors(self):
        d = self.make_dir(self.ALL)
        for args, code in [(["--from"], 2), (["--from", "/nonexistent"], 1),
                           (["--from", d, "--uninstall"], 2), (["--from", d, "--help"], 2)]:
            with self.subTest(args=args):
                self.assertEqual(self.install(*args, archives=False).returncode, code)
        self.assertEqual(self.under_home(), set())


class CompatibilityTests(Base):
    """What dist's installer took: its flags, and its install directory variables."""

    INSTALL_DIR_VARS = ["IRSYNC_INSTALL_DIR", "CARGO_DIST_FORCE_INSTALL_DIR",
                        "IRSYNC_UNMANAGED_INSTALL"]

    def test_quiet_and_verbose_are_accepted(self):
        for args in [("-q",), ("--quiet",), ("-v",), ("--verbose",), ("-qv",),
                     ("--quiet", "--no-modify-path", "-v")]:
            with self.subTest(args=args):
                home = os.path.join(self.tmp, "home-" + "".join(args))
                os.mkdir(home)
                r = self.install(*args, home=home)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual(self.under_home(home), set(FILES))

    def test_short_help(self):
        for args in [("-h",), ("-qh",), ("--quiet", "-h")]:
            with self.subTest(args=args):
                r = self.install(*args)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertIn("--uninstall", r.stdout)
                self.assertEqual(self.under_home(), set())

    def test_help_lists_the_ignored_flags(self):
        r = self.install("--help")
        self.assertEqual(r.returncode, 0, r.stderr)
        for flag in ["--quiet", "--verbose", "--no-modify-path", "-h"]:
            self.assertIn(flag, r.stdout)

    def test_bad_short_flags(self):
        for args in [("-x",), ("-qx",), ("-",), ("--quiet=1",), ("-h", "--uninstall"),
                     ("--help", "-h")]:
            with self.subTest(args=args):
                r = self.install(*args)
                self.assertEqual(r.returncode, 2)
                self.assertIn("usage", r.stderr)
                self.assertEqual(self.under_home(), set())

    def test_install_dir_variables_are_refused(self):
        write_exe(os.path.join(self.bindir, "curl"), FAKE_FETCHER)
        for var in self.INSTALL_DIR_VARS:
            with self.subTest(var=var):
                elsewhere = os.path.join(self.tmp, "elsewhere")
                r = self.install(archives=False, env={var: elsewhere})
                self.assertEqual(r.returncode, 1)
                self.assertIn(var, r.stderr)
                self.assertIn("~/.local", r.stderr)
                self.assertEqual(os.listdir(self.home), [])
                self.assertFalse(os.path.exists(elsewhere))
                self.assertFalse(os.path.exists(self.log), "nothing may be downloaded")

    def test_empty_install_dir_variables_are_fine(self):
        r = self.install(env={var: "" for var in self.INSTALL_DIR_VARS})
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.under_home(), set(FILES))

    def test_install_dir_variables_do_not_block_help_or_uninstall(self):
        self.assertEqual(self.install().returncode, 0)
        for var in self.INSTALL_DIR_VARS:
            with self.subTest(var=var):
                r = self.install("--help", env={var: "/elsewhere"})
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertIn("--uninstall", r.stdout)
        r = self.install("--uninstall", env={var: "/elsewhere" for var in self.INSTALL_DIR_VARS})
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.under_home(), set())


class KillTests(Base):
    def shells(self):
        found = [("sh", shutil.which("sh"))]
        if shutil.which("dash"):
            found.append(("dash", shutil.which("dash")))
        return found

    def temp_dirs(self):
        return [n for n in os.listdir(self.tmp) if n.startswith("irsync-installer.")]

    def test_killed_install_leaves_no_temp_files(self):
        for name, shell in self.shells():
            with self.subTest(shell=name):
                bindir = self.make_bindir(f"bin-kill-{name}", shell=shell)
                write_exe(os.path.join(bindir, "curl"), KILL_PARENT)
                r = self.install(bindir=bindir, archives=False)
                self.assertNotEqual(r.returncode, 0)
                self.assertEqual(self.temp_dirs(), [])
                self.assertEqual(os.listdir(self.home), [])

    def test_killed_while_placing_leaves_no_part_file(self):
        # A chmod that kills the shell: the first file has been copied to its
        # temporary name in ~/.local/bin but not yet renamed.
        for name, shell in self.shells():
            with self.subTest(shell=name):
                home = os.path.join(self.tmp, f"home-{name}")
                os.mkdir(home)
                bindir = self.make_bindir(f"bin-chmod-{name}", shell=shell)
                os.remove(os.path.join(bindir, "chmod"))
                write_exe(os.path.join(bindir, "chmod"),
                          KILL_PARENT + f'exec {shutil.which("chmod")} "$@"\n')
                r = self.install(bindir=bindir, home=home)
                self.assertNotEqual(r.returncode, 0)
                self.assertEqual(self.temp_dirs(), [])
                self.assertEqual(self.under_home(home), set())


class UninstallTests(Base):
    def test_removes_what_install_placed(self):
        self.assertEqual(self.install().returncode, 0)
        self.put(RECEIPT, "{}\n")
        self.put(".config/irsync/irsync.toml", "# config\n")
        r = self.install("--uninstall")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.under_home(), {".config/irsync/irsync.toml"})
        self.assertIn("https://irsync.org/book/how-to/clean-up.html", r.stderr)
        self.assertIn("~/.config/irsync", r.stderr)
        self.assertIn("~/.local/state/irsync", r.stderr)
        # Directories stay.
        self.assertTrue(os.path.isdir(os.path.join(self.home, ".local/share/man/man1")))

    def test_safe_to_run_twice_and_on_a_clean_home(self):
        r = self.install("--uninstall")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(os.listdir(self.home), [])
        self.assertEqual(self.install().returncode, 0)
        for _ in range(2):
            r = self.install("--uninstall")
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(self.under_home(), set())

    def test_leaves_other_files_alone(self):
        self.assertEqual(self.install().returncode, 0)
        self.put(".local/bin/other", "x\n")
        self.put(".local/share/man/man1/other.1", "x\n")
        self.assertEqual(self.install("--uninstall").returncode, 0)
        self.assertEqual(self.under_home(),
                         {".local/bin/other", ".local/share/man/man1/other.1"})

    def test_needs_no_archive_and_no_supported_platform(self):
        self.assertEqual(self.install().returncode, 0)
        os.remove(self.log)
        for fetcher in ["curl", "wget"]:
            write_exe(os.path.join(self.bindir, fetcher), FAKE_FETCHER)
        r = self.install("--uninstall", uname=("Linux", "riscv64"), archives=False,
                         env={"FAKE_FAIL": "1"})
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.under_home(), set())
        self.assertFalse(os.path.exists(self.log), "nothing may run a download or a checksum")

    def test_no_modify_path_with_uninstall(self):
        for args in [("--no-modify-path", "--uninstall"), ("--uninstall", "--no-modify-path")]:
            with self.subTest(args=args):
                self.assertEqual(self.install().returncode, 0)
                r = self.install(*args)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertIn("removed", r.stderr)
                self.assertEqual(self.under_home(), set())

    def test_notes_old_env_file(self):
        self.put(ENV_SH, "# env\n")
        r = self.install("--uninstall")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("env.sh", r.stderr)
        self.assertTrue(os.path.exists(os.path.join(self.home, ENV_SH)))

    def test_no_env_note_without_env_file(self):
        r = self.install("--uninstall")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertNotIn("env.sh", r.stderr)


class ShellTests(Base):
    def test_truncated_script_does_nothing(self):
        with open(self.installer) as f:
            lines = f.read().rstrip("\n").split("\n")
        self.assertEqual(lines[-1], 'main "$@"')
        env = {
            "PATH": self.bindir,
            "TMPDIR": self.tmp,
            "FAKE_UNAME_S": "Linux",
            "FAKE_UNAME_M": "x86_64",
            "FAKE_LOG": self.log,
            "IRSYNC_INSTALLER_ARCHIVE_DIR": self.archives,
        }
        sh = os.path.join(self.bindir, "sh")
        for n in range(1, len(lines) + 1):
            home = os.path.join(self.tmp, f"home-{n}")
            os.mkdir(home)
            part = "\n".join(lines[:n]) + "\n"
            r = subprocess.run([sh, "-s"], input=part, env=dict(env, HOME=home),
                               capture_output=True, text=True)
            if n < len(lines):
                # A syntax error on stderr is fine; installing anything is not.
                self.assertEqual(os.listdir(home), [], f"cut after line {n}")
                self.assertNotIn("installed", r.stderr, f"cut after line {n}")
            else:
                # The whole script does install, so the check above means something.
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual(self.under_home(home), set(FILES))

    def test_posix_sh(self):
        for script in [self.installer, BUILD]:
            subprocess.run(["sh", "-n", script], check=True)
            dash = shutil.which("dash")
            if dash:
                subprocess.run([dash, "-n", script], check=True)

    def fetch_with(self, fetcher, extra_env=None):
        write_exe(os.path.join(self.bindir, fetcher), FAKE_FETCHER)
        return self.install(archives=False, env=extra_env)

    def test_downloads_are_https_only(self):
        url = f"{BASE_URL}/irsync-x86_64-unknown-linux-musl.tar.xz"
        r = self.fetch_with("curl")
        self.assertEqual(r.returncode, 0, r.stderr)
        calls = read(self.log).decode().splitlines()
        curl = [c for c in calls if url in c]
        self.assertEqual(len(curl), 1, calls)
        self.assertIn("--proto =https", curl[0])
        self.assertIn("--tlsv1.2", curl[0])
        self.assertEqual(self.under_home(), set(FILES))

    def test_wget_downloads_are_https_only(self):
        url = f"{BASE_URL}/irsync-x86_64-unknown-linux-musl.tar.xz"
        r = self.fetch_with("wget")
        self.assertEqual(r.returncode, 0, r.stderr)
        calls = [c for c in read(self.log).decode().splitlines() if url in c]
        self.assertEqual(len(calls), 1)
        self.assertIn("--https-only", calls[0])
        self.assertEqual(self.under_home(), set(FILES))

    def test_wget_without_https_only_refuses(self):
        r = self.fetch_with("wget", {"FAKE_NO_HTTPS_ONLY": "1"})
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("install curl", r.stderr)
        self.assertFalse(os.path.exists(self.log), "nothing may be downloaded")
        self.assertEqual(self.under_home(), set())

    def test_failed_download_changes_nothing(self):
        r = self.fetch_with("curl", {"FAKE_FAIL": "1"})
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("could not download", r.stderr)
        self.assertEqual(self.under_home(), set())


if __name__ == "__main__":
    unittest.main()
