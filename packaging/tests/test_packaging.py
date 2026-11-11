"""Tests for build-packages.sh and update-repo.sh, using a throwaway GPG key.

Needs nfpm, apt-ftparchive, createrepo_c, rpm (and rpmsign) and gpg. A missing
tool skips the tests, unless PACKAGING_TESTS_REQUIRED=1, which makes it a failure.
"""
import glob
import gzip
import os
import re
import shutil
import subprocess
import tarfile
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
PKG = os.path.dirname(HERE)
BUILD = os.path.join(PKG, "build-packages.sh")
UPDATE = os.path.join(PKG, "update-repo.sh")
UID = "irsync releases <releases@irsync.org>"
TOOLS = ["nfpm", "apt-ftparchive", "createrepo_c", "rpm", "rpmsign", "gpg", "dpkg-deb"]
ARCHES = {  # rust triple arch -> (deb arch, rpm arch)
    "x86_64": ("amd64", "x86_64"),
    "aarch64": ("arm64", "aarch64"),
}


def missing_tool():
    for tool in TOOLS:
        if shutil.which(tool) is None:
            return tool
    return None


def setUpModule():
    tool = missing_tool()
    if tool and os.environ.get("PACKAGING_TESTS_REQUIRED") == "1":
        raise AssertionError(f"required tool missing: {tool}")
    if tool:
        raise unittest.SkipTest(f"tool not installed: {tool}")


def run(*args, **kw):
    kw.setdefault("check", True)
    kw.setdefault("capture_output", True)
    kw.setdefault("text", True)
    r = subprocess.run(list(args), **{**kw, "check": False})
    if kw["check"] and r.returncode:
        raise AssertionError(f"{args} failed ({r.returncode}):\n{r.stdout}\n{r.stderr}")
    return r


class Base(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root = tempfile.mkdtemp(prefix="pkgtest-")
        cls.gnupg = os.path.join(cls.root, "gnupg")
        os.mkdir(cls.gnupg, 0o700)
        cls.env = dict(os.environ, GNUPGHOME=cls.gnupg)
        cls.env.pop("GPG_PASSPHRASE", None)
        run("gpg", "--batch", "--passphrase", "", "--quick-gen-key", UID,
            "rsa2048", "sign", "never", env=cls.env)
        out = run("gpg", "--batch", "--with-colons", "--list-keys", UID, env=cls.env).stdout
        cls.fpr = next(l.split(":")[9] for l in out.splitlines() if l.startswith("fpr:"))
        cls.env["GPG_KEY_ID"] = cls.fpr
        cls.archives = os.path.join(cls.root, "archives")
        os.mkdir(cls.archives)
        for arch in ARCHES:
            triple = f"irsync-{arch}-unknown-linux-musl"
            d = os.path.join(cls.root, "stage", triple)
            os.makedirs(d)
            for name in ("irsync", "fileset"):
                with open(os.path.join(d, name), "w") as f:
                    f.write(f"#!/bin/sh\necho {name}\n")
                os.chmod(os.path.join(d, name), 0o755)
            for name in ("LICENSE", "README.md"):
                with open(os.path.join(d, name), "w") as f:
                    f.write(name + "\n")
            with tarfile.open(os.path.join(cls.archives, triple + ".tar.xz"), "w:xz") as t:
                t.add(d, arcname=triple)
        cls.built = {}

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.root, ignore_errors=True)

    def build(self, version):
        """Build packages for a version once; return the directory."""
        if version not in self.built:
            out = os.path.join(self.root, "out-" + version)
            run(BUILD, self.archives, version, out, env=self.env)
            self.built[version] = out
        return self.built[version]

    def update(self, repo, versions, keep=3):
        for v in versions:
            run(UPDATE, repo, self.build(v), str(keep), env=self.env)

    def new_repo(self):
        return tempfile.mkdtemp(prefix="repo-", dir=self.root)

    def debs(self, repo, arch):
        return sorted(glob.glob(f"{repo}/apt/pool/main/i/irsync/irsync_*_{arch}.deb"))

    def rpms(self, repo, arch):
        return sorted(glob.glob(f"{repo}/rpm/{arch}/*.rpm"))

    def packages_versions(self, repo, deb_arch):
        p = f"{repo}/apt/dists/stable/main/binary-{deb_arch}/Packages"
        with open(p) as f:
            text = f.read()
        return re.findall(r"^Version: (\S+)$", text, re.M)


class Tests(Base):
    def test_build_packages_contents(self):
        out = self.build("0.2.0")
        for arch, (deb_arch, rpm_arch) in ARCHES.items():
            deb = f"{out}/irsync_0.2.0_{deb_arch}.deb"
            rpm = f"{out}/irsync-0.2.0-1.{rpm_arch}.rpm"
            self.assertTrue(os.path.isfile(deb), deb)
            self.assertTrue(os.path.isfile(rpm), rpm)
            files = [l.split()[-1] for l in run("dpkg-deb", "-c", deb).stdout.splitlines()
                     if not l.startswith("d")]
            self.assertEqual(sorted(files), ["./usr/bin/fileset", "./usr/bin/irsync"])
            self.assertEqual(run("dpkg-deb", "-f", deb, "Version").stdout.strip(), "0.2.0")
            self.assertEqual(run("dpkg-deb", "-f", deb, "Architecture").stdout.strip(), deb_arch)
            self.assertEqual(run("dpkg-deb", "-f", deb, "Maintainer").stdout.strip(), UID)
            listed = run("rpm", "-qpl", rpm).stdout.split()
            self.assertEqual(sorted(listed), ["/usr/bin/fileset", "/usr/bin/irsync"])
            self.assertEqual(run("rpm", "-qp", "--qf", "%{ARCH}", rpm).stdout, rpm_arch)

    def test_apt_metadata_signed(self):
        repo = self.new_repo()
        self.update(repo, ["0.2.0"])
        d = f"{repo}/apt/dists/stable"
        run("gpg", "--verify", f"{d}/InRelease", env=self.env)
        run("gpg", "--verify", f"{d}/Release.gpg", f"{d}/Release", env=self.env)
        for deb_arch in ("amd64", "arm64"):
            self.assertEqual(self.packages_versions(repo, deb_arch), ["0.2.0"])
            p = f"{d}/main/binary-{deb_arch}/Packages"
            with open(p) as f:
                text = f.read()
            self.assertIn("Package: irsync\n", text)
            fn = re.search(r"^Filename: (\S+)$", text, re.M).group(1)
            self.assertTrue(os.path.isfile(f"{repo}/apt/{fn}"), fn)
            with gzip.open(p + ".gz", "rt") as f:
                self.assertEqual(f.read(), text)

    def test_rpm_metadata_signed(self):
        repo = self.new_repo()
        self.update(repo, ["0.2.0"])
        for rpm_arch in ("x86_64", "aarch64"):
            run("gpg", "--verify", f"{repo}/rpm/{rpm_arch}/repodata/repomd.xml.asc",
                f"{repo}/rpm/{rpm_arch}/repodata/repomd.xml", env=self.env)
        db = os.path.join(self.root, "rpmdb-" + os.path.basename(repo))
        os.mkdir(db)
        run("rpm", "--dbpath", db, "--import", f"{repo}/irsync.asc")
        pkg = self.rpms(repo, "x86_64")[0]
        out = run("rpm", "--dbpath", db, "-K", pkg).stdout
        self.assertRegex(out, r"\bOK\b")
        self.assertNotIn("NOT OK", out)
        self.assertNotIn("NOKEY", out)

    def test_prunes_by_version(self):
        repo = self.new_repo()
        self.update(repo, ["0.8.0", "0.9.0", "0.10.0", "0.11.0"], keep=3)
        want = ["0.10.0", "0.11.0", "0.9.0"]
        for deb_arch, rpm_arch in ARCHES.values():
            debs = [re.search(r"irsync_(.*)_", os.path.basename(p)).group(1)
                    for p in self.debs(repo, deb_arch)]
            rpms = [re.search(r"irsync-(.*)-1\.", os.path.basename(p)).group(1)
                    for p in self.rpms(repo, rpm_arch)]
            self.assertEqual(sorted(debs), want)
            self.assertEqual(sorted(rpms), want)
            self.assertEqual(sorted(self.packages_versions(repo, deb_arch)), want)
            repodata = glob.glob(f"{repo}/rpm/{rpm_arch}/repodata/*primary.xml*")[0]
            raw = run("sh", "-c", f"(zstd -dc {repodata} 2>/dev/null || gzip -dc {repodata})",
                      capture_output=True, text=True, check=False).stdout
            self.assertTrue(raw, "could not read " + repodata)
            found = sorted(re.findall(r'<version epoch="0" ver="([^"]+)"', raw))
            self.assertEqual(found, want)

    def test_rerun_same_version_is_idempotent(self):
        repo = self.new_repo()
        self.update(repo, ["0.2.0", "0.2.0"])
        for deb_arch, rpm_arch in ARCHES.values():
            self.assertEqual(len(self.debs(repo, deb_arch)), 1)
            self.assertEqual(len(self.rpms(repo, rpm_arch)), 1)
            self.assertEqual(self.packages_versions(repo, deb_arch), ["0.2.0"])
        d = f"{repo}/apt/dists/stable"
        run("gpg", "--verify", f"{d}/InRelease", env=self.env)
        run("gpg", "--verify", f"{d}/Release.gpg", f"{d}/Release", env=self.env)
        run("gpg", "--verify", f"{repo}/rpm/x86_64/repodata/repomd.xml.asc",
            f"{repo}/rpm/x86_64/repodata/repomd.xml", env=self.env)
        db = os.path.join(self.root, "rpmdb2-" + os.path.basename(repo))
        os.mkdir(db)
        run("rpm", "--dbpath", db, "--import", f"{repo}/irsync.asc")
        out = run("rpm", "--dbpath", db, "-K", self.rpms(repo, "x86_64")[0]).stdout
        self.assertNotIn("NOT OK", out)

    def test_static_files(self):
        repo = self.new_repo()
        self.update(repo, ["0.2.0"])
        with open(f"{repo}/CNAME") as f:
            self.assertEqual(f.read().strip(), "dist.irsync.org")
        asc = run("gpg", "--show-keys", "--with-colons", f"{repo}/irsync.asc").stdout
        gpgk = run("gpg", "--show-keys", "--with-colons", f"{repo}/irsync.gpg").stdout
        self.assertIn(self.fpr, asc)
        self.assertIn(self.fpr, gpgk)
        with open(f"{repo}/irsync.gpg", "rb") as f:
            self.assertNotIn(b"BEGIN PGP", f.read())
        with open(f"{repo}/irsync.repo") as f:
            self.assertIn("repo_gpgcheck=1", f.read())
        self.assertTrue(os.path.isfile(f"{repo}/index.html"))


if __name__ == "__main__":
    unittest.main()
