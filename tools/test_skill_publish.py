"""Offline regressions for the release upload boundary; never contacts GitHub."""
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("skill_publish", Path(__file__).with_name("publish-skill-assets.py"))
publisher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(publisher)


class PublishTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.name = "test-1.0.0-demo.zip"
        (self.root / self.name).write_bytes(b"fixture zip")
        self.digest = hashlib.sha256(b"fixture zip").hexdigest()
        self.plan = {"releases": [{"tag": "skills-test-v1.0.0", "assets": [
            {"path": self.name, "sha256": self.digest}
        ]}]}

    def run_plan(self):
        publisher.publish(self.plan, self.root, "owner/repo", "a" * 40)

    def test_create_then_upload_without_clobber(self):
        with patch.object(publisher, "gh", side_effect=[None, "release created", ""]) as gh:
            self.run_plan()
        calls = [call.args for call in gh.call_args_list]
        self.assertEqual(calls[0][:2], ("api", "repos/owner/repo/releases/tags/skills-test-v1.0.0"))
        self.assertEqual(calls[1][:3], ("release", "create", "skills-test-v1.0.0"))
        self.assertIn("--latest=false", calls[1])
        self.assertIn("a" * 40, calls[1])
        self.assertEqual(calls[2][:3], ("release", "upload", "skills-test-v1.0.0"))
        self.assertNotIn("--clobber", calls[2])

    def test_retry_skips_identical_existing_asset(self):
        release = {"assets": [{"name": self.name, "digest": "sha256:" + self.digest}]}
        with patch.object(publisher, "gh", return_value=json.dumps(release)) as gh:
            self.run_plan()
        self.assertEqual(gh.call_count, 1)

    def test_draft_release_cannot_back_public_manifest_urls(self):
        release = {"draft": True, "assets": [
            {"name": self.name, "digest": "sha256:" + self.digest}
        ]}
        with patch.object(publisher, "gh", return_value=json.dumps(release)) as gh:
            with self.assertRaisesRegex(publisher.PublishFailure, "draft"):
                self.run_plan()
        self.assertEqual(gh.call_count, 1)

    def test_conflicting_or_unknown_remote_digest_stops_without_mutation(self):
        for digest in ("sha256:" + "0" * 64, None):
            release = {"assets": [{"name": self.name, "digest": digest}]}
            with patch.object(publisher, "gh", return_value=json.dumps(release)) as gh:
                with self.assertRaises(publisher.PublishFailure):
                    self.run_plan()
            self.assertEqual(gh.call_count, 1)

    def test_tampered_local_archive_stops_before_github(self):
        (self.root / self.name).write_bytes(b"different")
        with patch.object(publisher, "gh") as gh:
            with self.assertRaises(publisher.PublishFailure):
                self.run_plan()
        gh.assert_not_called()

    def test_rejects_unsafe_paths_tags_and_duplicate_destinations(self):
        invalid = [
            {"tag": "../escape", "assets": self.plan["releases"][0]["assets"]},
            {"tag": "skills-test-v1.0.0", "assets": [{"path": "../escape.zip", "sha256": self.digest}]},
            {"tag": "skills-test-v1.0.0", "assets": self.plan["releases"][0]["assets"] * 2},
        ]
        for release in invalid:
            with patch.object(publisher, "gh") as gh:
                with self.assertRaises(publisher.PublishFailure):
                    publisher.publish({"releases": [release]}, self.root, "owner/repo", "a" * 40)
            gh.assert_not_called()

    def test_refuses_full_release_before_upload(self):
        existing = {"assets": [{"name": f"old-{i}.zip"} for i in range(1000)]}
        with patch.object(publisher, "gh", return_value=json.dumps(existing)) as gh:
            with self.assertRaises(publisher.PublishFailure):
                self.run_plan()
        self.assertEqual(gh.call_count, 1)

    def test_empty_plan_performs_no_github_calls(self):
        with patch.object(publisher, "gh") as gh:
            publisher.publish({"releases": []}, self.root, "owner/repo", "a" * 40)
        gh.assert_not_called()


if __name__ == "__main__":
    unittest.main()
