"""Offline regressions for the Windows -> exact-version Unix enrichment boundary."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
DRIVER = ROOT / "tools/enrich-changed-packages.py"


class ChangedPackageTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.repo = Path(self.tmp.name)
        self.git("init", "-q")
        self.git("config", "user.name", "test")
        self.git("config", "user.email", "test@example.invalid")
        (self.repo / "tools").mkdir()
        (self.repo / "tools/brew-sources.toml").write_text(
            '[[source]]\nname = "alpha"\n[[source]]\nname = "beta"\n'
        )
        self.manifest("alpha", "1.0.0")
        self.manifest("beta", "1.0.0")
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")
        self.importer = self.repo / "importer"
        self.importer.write_text(
            f"#!{sys.executable}\nimport json, pathlib, sys\n"
            "pathlib.Path('invocation.json').write_text(json.dumps(sys.argv[1:]))\n"
        )
        self.importer.chmod(0o755)

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.repo, check=True,
                              capture_output=True)

    def manifest(self, name, version, suffix=""):
        path = self.repo / "manifests" / name[0] / name / f"{version}.toml"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(f'name = "{name}"\nversion = "{version}"\n{suffix}')
        return path

    def run_driver(self, importer=None):
        return subprocess.run(
            [sys.executable, str(DRIVER), "--importer", str(importer or self.importer)],
            cwd=self.repo, text=True, capture_output=True,
        )

    def test_changed_tracked_staged_and_untracked_packages_are_allowlisted_once(self):
        self.manifest("alpha", "1.0.0", "# changed\n")
        self.git("add", "manifests/a/alpha/1.0.0.toml")
        self.manifest("alpha", "2.0.0")
        self.manifest("beta", "1.0.0", "# unstaged\n")
        self.manifest("unlisted", "1.0.0")
        result = self.run_driver()
        self.assertEqual(result.returncode, 0, result.stderr)
        argv = json.loads((self.repo / "invocation.json").read_text())
        self.assertEqual(argv, ["--sources", "tools/brew-sources.toml", "--manifests",
                                "manifests", "--registry-version", "--static-only", "--only", "alpha,beta"])

    def test_no_relevant_changes_never_invokes_importer(self):
        self.manifest("unlisted", "1.0.0")
        skill = self.repo / "manifests/skills/a/alpha/1.0.0.toml"
        skill.parent.mkdir(parents=True)
        skill.write_text("# independent skill\n")
        result = self.run_driver(self.repo / "nonexistent")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("No changed allowlisted packages", result.stdout)
        self.assertFalse((self.repo / "invocation.json").exists())

    def test_non_allowlisted_version_syntax_does_not_block_sync(self):
        self.manifest("conemu-color-themes", ".june.2020.2")
        self.git("add", "manifests/c/conemu-color-themes/.june.2020.2.toml")
        self.git("commit", "-qm", "unlisted historical filename")
        self.manifest("conemu-color-themes", ".june.2020.2", "# metadata refresh\n")
        result = self.run_driver(self.repo / "nonexistent")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("No changed allowlisted packages", result.stdout)

    def test_deleted_manifest_does_not_trigger_enrichment(self):
        (self.repo / "manifests/a/alpha/1.0.0.toml").unlink()
        result = self.run_driver(self.repo / "nonexistent")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_unsafe_path_is_rejected_without_running_importer(self):
        self.manifest("alpha", "2.0.0;touch PWNED")
        result = self.run_driver()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsafe manifest path", result.stderr)
        self.assertFalse((self.repo / "invocation.json").exists())
        self.assertFalse((self.repo / "PWNED").exists())

    def test_newline_filename_is_not_split_into_separate_paths(self):
        self.manifest("alpha", "2.0.0\nextra")
        result = self.run_driver()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsafe manifest path", result.stderr)
        self.assertFalse((self.repo / "invocation.json").exists())

    def test_symlink_manifest_is_rejected(self):
        path = self.repo / "manifests/a/alpha/2.0.0.toml"
        path.symlink_to("1.0.0.toml")
        result = self.run_driver()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsafe manifest path", result.stderr)
        self.assertFalse((self.repo / "invocation.json").exists())

    def test_tracked_manifest_replaced_by_symlink_is_rejected(self):
        path = self.repo / "manifests/a/alpha/1.0.0.toml"
        path.unlink()
        path.symlink_to("../../b/beta/1.0.0.toml")
        result = self.run_driver()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsafe manifest path", result.stderr)

    def test_importer_failure_propagates(self):
        self.manifest("alpha", "2.0.0")
        self.importer.write_text(f"#!{sys.executable}\nraise SystemExit(23)\n")
        result = self.run_driver()
        self.assertEqual(result.returncode, 23, result.stderr)


class WorkflowTests(unittest.TestCase):
    def test_enrichment_runs_between_windows_generation_and_validation_or_pr(self):
        for filename, producer in [("bump.yml", "voli-index-tool bump"),
                                   ("scoop-sync.yml", "/tmp/scoop-extras manifests/")]:
            with self.subTest(workflow=filename):
                text = (ROOT / ".github/workflows" / filename).read_text()
                self.assertIn("python tools/enrich-changed-packages.py", text)
                self.assertLess(text.index(producer), text.index("python tools/enrich-changed-packages.py"))
                self.assertLess(text.index("python tools/enrich-changed-packages.py"),
                                text.index("voli-index-tool validate manifests/"))
                self.assertLess(text.index("voli-index-tool validate manifests/"), text.index("gh pr create"))
                self.assertIn("cargo build --release --locked", text)
                self.assertIn('python-version: "3.11"', text)

    def test_bump_pipeline_preserves_generator_exit_status(self):
        text = (ROOT / ".github/workflows/bump.yml").read_text()
        step = text.split("      - name: Run bump\n", 1)[1].split("      - name:", 1)[0]
        self.assertIn("shell: bash", step)

    def test_host_verification_builds_once_and_aggregates_package_failures(self):
        text = (ROOT / ".github/workflows/tools.yml").read_text()
        run = text.split("- name: Verify unix payloads from brew-sources.toml", 1)[1]
        self.assertIn("cargo build --locked", run)
        self.assertIn("failed=0", run)
        self.assertIn("for name in $names", run)
        self.assertIn('target/debug/brew-import --verify-only --sources ../brew-sources.toml --manifests ../../manifests "$name"', run)
        self.assertIn("failed=1", run)
        self.assertIn('exit "$failed"', run)

    def test_native_smoke_jobs_are_read_only_without_persisted_credentials(self):
        text = (ROOT / ".github/workflows/tools.yml").read_text()
        self.assertIn("permissions:\n  contents: read\n", text)
        self.assertEqual(text.count("persist-credentials: false"), text.count("uses: actions/checkout@"))
        self.assertNotIn("secrets.", text)

    def test_native_verification_continues_after_failure_and_returns_failure(self):
        text = (ROOT / ".github/workflows/tools.yml").read_text()
        run = text.split("- name: Verify unix payloads from brew-sources.toml", 1)[1]
        run = run.split("        run: |\n", 1)[1]
        run = "\n".join(line[10:] for line in run.splitlines())
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            workdir = root / "tools/brew-import"
            binary = workdir / "target/debug/brew-import"
            binary.parent.mkdir(parents=True)
            binary.write_text(
                '#!/bin/sh\nfor last; do :; done\nprintf "%s\\n" "$last" >> verified.txt\n'
                '[ "$last" != alpha ]\n'
            )
            binary.chmod(0o755)
            (root / "tools/brew-sources.toml").write_text(
                '[[source]]\nname = "alpha"\n[[source]]\nname = "beta"\n'
            )
            mock_bin = root / "bin"
            mock_bin.mkdir()
            cargo = mock_bin / "cargo"
            cargo.write_text('#!/bin/sh\necho "$*" >> build.txt\n')
            cargo.chmod(0o755)
            result = subprocess.run(
                ["bash", "-eo", "pipefail", "-c", run], cwd=workdir,
                env={**os.environ, "PATH": f"{mock_bin}:{os.environ['PATH']}", "RUNNER_OS": "test"},
                capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual((workdir / "verified.txt").read_text().splitlines(), ["alpha", "beta"])
            self.assertEqual((workdir / "build.txt").read_text(), "build --locked\n")


if __name__ == "__main__":
    unittest.main()
