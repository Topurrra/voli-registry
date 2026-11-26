"""Tests for tools/automerge_gate.py: what may merge without the owner."""

import datetime as dt
import hashlib
import io
import unittest

import automerge_gate as g

H1, H2, H3, H4 = ("1" * 64), ("2" * 64), ("3" * 64), ("4" * 64)

CROC_OLD = f"""name = "croc"
version = "11.6.0"
description = "Easily and securely send things from one computer to another"
homepage = "https://github.com/schollz/croc"
license = "MIT"
bin = ["croc.exe"]

[source.x64]
url = "https://github.com/schollz/croc/releases/download/v11.6.0/croc_v11.6.0_Windows-64bit.zip"
sha256 = "{H1}"

[source.arm64]
url = "https://github.com/schollz/croc/releases/download/v11.6.0/croc_v11.6.0_Windows-ARM64.zip"
sha256 = "{H2}"

[autoupdate]
github = "schollz/croc"
"""

CROC_NEW = (CROC_OLD.replace("11.6.0", "11.6.3")
            .replace(H1, H3).replace(H2, H4))

NOW = dt.datetime(2026, 11, 26, 12, 0, tzinfo=dt.timezone.utc)


def bot_pr(**over):
    pr = {
        "number": 150,
        "author": {"login": "app/github-actions", "is_bot": True},
        "headRefName": "auto-bump/2026-11-25-1-1",
        "headRefOid": "abc123",
        "isCrossRepository": False,
        "baseRefName": "main",
        "state": "OPEN",
        "isDraft": False,
        "labels": [],
        "commits": [{"oid": "abc123", "authors": [{"name": "voli-registry-ci"}]}],
        "createdAt": "2026-11-25T06:10:00Z",
    }
    pr.update(over)
    return pr


class PrFactsTests(unittest.TestCase):
    def test_bot_bump_past_cooldown_is_a_candidate(self):
        self.assertEqual(g.pr_refusals(bot_pr(), NOW, 24), [])

    def test_people_and_forks_wait_for_the_owner(self):
        self.assertTrue(g.pr_refusals(bot_pr(author={"login": "someone"}), NOW, 24))
        self.assertTrue(g.pr_refusals(bot_pr(isCrossRepository=True), NOW, 24))

    def test_other_bot_workflows_wait_for_the_owner(self):
        for branch in ("scoop-sync/2026-11-20-1", "skill-sync/2026-11-20-1", "main"):
            self.assertTrue(g.pr_refusals(bot_pr(headRefName=branch), NOW, 24), branch)

    def test_hold_label_vetoes(self):
        self.assertTrue(g.pr_refusals(bot_pr(labels=[{"name": "hold"}]), NOW, 24))

    def test_cooldown_gives_the_owner_a_window(self):
        young = bot_pr(createdAt="2026-11-26T06:00:00Z")
        why = g.pr_refusals(young, NOW, 24)
        self.assertTrue(any("cool-down" in w for w in why), why)

    def test_a_commit_pushed_by_anyone_else_waits(self):
        commits = [
            {"oid": "a" * 40, "authors": [{"name": "voli-registry-ci"}]},
            {"oid": "b" * 40, "authors": [{"name": "topurrra"}]},
        ]
        self.assertTrue(g.pr_refusals(bot_pr(commits=commits), NOW, 24))

    def test_closed_or_draft_or_wrong_base_waits(self):
        self.assertTrue(g.pr_refusals(bot_pr(state="MERGED"), NOW, 24))
        self.assertTrue(g.pr_refusals(bot_pr(isDraft=True), NOW, 24))
        self.assertTrue(g.pr_refusals(bot_pr(baseRefName="dev"), NOW, 24))


class CompareBumpTests(unittest.TestCase):
    def test_pure_version_bump_passes(self):
        reasons, dls = g.compare_bump(CROC_OLD, CROC_NEW)
        self.assertEqual(reasons, [])
        self.assertEqual(sorted(d["where"] for d in dls), ["source.arm64", "source.x64"])
        self.assertTrue(all("11.6.3" in d["url"] for d in dls))

    def refused(self, new, old=CROC_OLD):
        reasons, _ = g.compare_bump(old, new)
        self.assertTrue(reasons, "expected a refusal")
        return reasons

    def test_host_change_waits(self):
        self.refused(CROC_NEW.replace("https://github.com/schollz/croc/releases/download/v11.6.3/croc_v11.6.3_Windows-64bit.zip",
                                      "https://evil.example/croc_v11.6.3_Windows-64bit.zip"))

    def test_other_path_on_the_same_host_waits(self):
        # github.com hosts everyone's releases: same host is not enough.
        self.refused(CROC_NEW.replace("schollz/croc/releases/download/v11.6.3/croc_v11.6.3_Windows-64bit",
                                      "mallory/croc/releases/download/v11.6.3/croc_v11.6.3_Windows-64bit"))

    def test_new_bin_waits(self):
        self.refused(CROC_NEW.replace('bin = ["croc.exe"]', 'bin = ["croc.exe", "ssh.exe"]'))

    def test_new_env_waits(self):
        self.refused(CROC_NEW + '\n[env]\nPATH = "{dir}"\n')

    def test_new_source_block_waits(self):
        self.refused(CROC_NEW + f'\n[source.linux-x64]\nurl = "https://github.com/schollz/croc/releases/download/v11.6.3/croc_v11.6.3_Linux-64bit.tar.gz"\nsha256 = "{H1}"\n')

    def test_removed_source_block_waits(self):
        cut = CROC_NEW[:CROC_NEW.index("[source.arm64]")] + CROC_NEW[CROC_NEW.index("[autoupdate]"):]
        self.refused(cut)

    def test_description_change_waits(self):
        self.refused(CROC_NEW.replace("Easily and securely", "Run this"))

    def test_http_waits(self):
        old = CROC_OLD.replace("https://github.com", "http://github.com")
        new = CROC_NEW.replace("https://github.com", "http://github.com")
        reasons = self.refused(new, old)
        self.assertTrue(any("https" in r for r in reasons), reasons)

    def test_downgrade_or_same_version_waits(self):
        self.refused(CROC_OLD.replace(H1, H3), CROC_OLD)
        self.refused(CROC_OLD.replace("11.6.0", "11.5.0"), CROC_OLD)

    def test_hash_must_be_a_hex_digest(self):
        self.refused(CROC_NEW.replace(H3, "not-a-hash"))

    def test_bad_toml_waits(self):
        self.refused("name = ")


class ClassifyTests(unittest.TestCase):
    NEW = "manifests/c/croc/11.6.3.toml"
    OLD = "manifests/c/croc/11.6.0.toml"

    def verdict(self, rows, texts=None, baselines=None):
        texts = texts or {self.NEW: CROC_NEW}
        baselines = baselines if baselines is not None else {self.NEW: (self.OLD, CROC_OLD)}
        return g.classify(rows, read_new=texts.__getitem__,
                          baseline=lambda p: baselines.get(p, (None, "")))

    def test_bump_pr_is_eligible(self):
        v = self.verdict([("A", self.NEW)])
        self.assertTrue(v["eligible"], v["reasons"])
        self.assertEqual(len(v["downloads"]), 2)

    def test_editing_an_existing_version_waits(self):
        self.assertFalse(self.verdict([("M", self.OLD)])["eligible"])

    def test_deleting_waits(self):
        self.assertFalse(self.verdict([("D", self.OLD)])["eligible"])

    def test_new_package_waits(self):
        v = self.verdict([("A", "manifests/n/newpkg/1.0.0.toml")],
                         texts={"manifests/n/newpkg/1.0.0.toml": CROC_NEW}, baselines={})
        self.assertFalse(v["eligible"])

    def test_any_file_outside_manifests_waits(self):
        for path in (".github/workflows/publish.yml", "tools/automerge_gate.py",
                     "packages.json", "manifests/c/croc/notes.md"):
            v = self.verdict([("A", self.NEW), ("M", path)])
            self.assertFalse(v["eligible"], path)

    def test_empty_pr_waits(self):
        self.assertFalse(self.verdict([])["eligible"])


class RehashTests(unittest.TestCase):
    DL = [{"manifest": "m.toml", "where": "source.x64", "url": "https://x/y.zip",
           "sha256": hashlib.sha256(b"payload").hexdigest()}]

    def test_matching_download_passes(self):
        self.assertEqual(g.rehash(self.DL, fetch=lambda u, w: {
            "sha256": hashlib.sha256(b"payload").hexdigest()}), [])

    def test_mismatch_fails(self):
        fails = g.rehash(self.DL, fetch=lambda u, w: {"sha256": "0" * 64})
        self.assertEqual(len(fails), 1)
        self.assertIn("mismatch", fails[0])

    def test_download_error_fails(self):
        def boom(u, w):
            raise OSError("404")
        self.assertIn("download failed", g.rehash(self.DL, fetch=boom)[0])

    def test_hash_url_streams_and_enforces_the_cap(self):
        opener = lambda req, timeout: io.BytesIO(b"payload")  # noqa: E731
        got = g.hash_url("https://x", {"sha256", "sha512"}, opener=opener)
        self.assertEqual(got["sha256"], hashlib.sha256(b"payload").hexdigest())
        self.assertEqual(got["sha512"], hashlib.sha512(b"payload").hexdigest())
        with self.assertRaises(ValueError):
            g.hash_url("https://x", {"sha256"}, limit=3, opener=opener)


if __name__ == "__main__":
    unittest.main()
