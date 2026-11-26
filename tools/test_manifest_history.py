"""Tests for tools/manifest_history.py and the gates that use it.

The git-backed tests build a throwaway repo shaped like this registry, with a
base commit on `main` and a bump branch that ADDS a new version file, which
is exactly what bump.yml produces.
"""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

import bump_pr_report
import check_reserved_shims
import manifest_history as mh

GIT_2_0 = """name = "git"
version = "2.0.0"
description = "Distributed version control"
bin = ["cmd/git.exe"]

[source.x64]
url = "https://github.com/git-for-windows/git/releases/download/v2.0.0/MinGit-2.0.0-64-bit.zip"
sha256 = "%s"
""" % ("a" * 64)


def run(*args, cwd):
    subprocess.run(args, cwd=cwd, check=True, capture_output=True)


class VersionOrderTests(unittest.TestCase):
    def test_numeric_aware(self):
        self.assertLess(mh.version_key("1.9.0"), mh.version_key("1.10.0"))
        self.assertLess(mh.version_key("8.23.0_6"), mh.version_key("8.23.0_8"))
        self.assertLess(mh.version_key("1.0.rc1"), mh.version_key("1.0.0"))

    def test_previous_version_is_newest_older_sibling(self):
        sibs = [
            "manifests/c/croc/11.5.5.toml",
            "manifests/c/croc/11.6.0.toml",
            "manifests/c/croc/9.0.0.toml",
        ]
        self.assertEqual(
            mh.previous_version_path("manifests/c/croc/11.6.3.toml", sibs),
            "manifests/c/croc/11.6.0.toml",
        )

    def test_no_older_sibling_means_no_baseline(self):
        self.assertIsNone(mh.previous_version_path("manifests/n/new/1.0.toml", []))
        # a downgrade has no OLDER sibling either
        self.assertIsNone(
            mh.previous_version_path("manifests/c/croc/1.0.0.toml",
                                     ["manifests/c/croc/2.0.0.toml"])
        )


class BumpRepo(unittest.TestCase):
    """A registry repo with git 2.0.0 on main and a branch adding 2.1.0."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self._tmp.name)
        run("git", "init", "-q", "-b", "main", cwd=self.repo)
        run("git", "config", "user.email", "t@example.invalid", cwd=self.repo)
        run("git", "config", "user.name", "t", cwd=self.repo)
        run("git", "config", "core.autocrlf", "false", cwd=self.repo)
        d = self.repo / "manifests" / "g" / "git"
        d.mkdir(parents=True)
        (d / "2.0.0.toml").write_text(GIT_2_0, encoding="utf-8", newline="\n")
        run("git", "add", ".", cwd=self.repo)
        run("git", "commit", "-q", "-m", "base", cwd=self.repo)
        run("git", "checkout", "-q", "-b", "auto-bump/x", cwd=self.repo)
        self.new = d / "2.1.0.toml"
        self.new.write_text(GIT_2_0.replace("2.0.0", "2.1.0"), encoding="utf-8", newline="\n")
        self._cwd = os.getcwd()
        os.chdir(self.repo)

    def tearDown(self):
        os.chdir(self._cwd)
        self._tmp.cleanup()


class BaselineTests(BumpRepo):
    def test_untracked_new_version_counts_as_changed(self):
        fork = mh.git_merge_base("main")
        self.assertEqual(mh.git_changed_manifests(fork, "manifests"),
                         ["manifests/g/git/2.1.0.toml"])

    def test_baseline_of_a_bump_is_the_previous_version(self):
        fork = mh.git_merge_base("main")
        path, text = mh.git_baseline(fork, "manifests/g/git/2.1.0.toml")
        self.assertEqual(path, "manifests/g/git/2.0.0.toml")
        self.assertIn('version = "2.0.0"', text)


class ReservedShimGateOnBumps(BumpRepo):
    def test_bumping_a_package_that_already_had_git_is_allowed(self):
        # Before the fix, the new 2.1.0.toml had no baseline, so `git` counted
        # as newly added and every routine bump of git failed validate.
        run("git", "add", ".", cwd=self.repo)
        run("git", "commit", "-q", "-m", "bump", cwd=self.repo)
        self.assertEqual(check_reserved_shims.check_pr("main", Path("manifests")), [])

    def test_a_new_package_claiming_git_is_still_refused(self):
        d = self.repo / "manifests" / "f" / "fakegit"
        d.mkdir(parents=True)
        (d / "1.0.0.toml").write_text(GIT_2_0.replace('name = "git"', 'name = "fakegit"'),
                                      encoding="utf-8", newline="\n")
        run("git", "add", ".", cwd=self.repo)
        run("git", "commit", "-q", "-m", "add fakegit", cwd=self.repo)
        problems = check_reserved_shims.check_pr("main", Path("manifests"))
        self.assertEqual(problems, [("manifests/f/fakegit/1.0.0.toml", ["git"])])


class ProvenanceReportOnBumps(BumpRepo):
    def test_report_covers_an_uncommitted_new_version_file(self):
        # bump.yml runs the report before committing. Before the fix the new
        # file was invisible and the PR said "No package manifests changed".
        sections, failed = bump_pr_report.collect_rows(
            "main", Path("manifests"), fetch=lambda url: None)
        self.assertFalse(failed)
        self.assertEqual(len(sections), 1, sections)
        self.assertIn("git", sections[0])
        self.assertIn("2.1.0", sections[0])
        self.assertIn("github.com", sections[0])


def suite():
    return unittest.defaultTestLoader.loadTestsFromModule(
        __import__(__name__))


if __name__ == "__main__":
    unittest.main()
