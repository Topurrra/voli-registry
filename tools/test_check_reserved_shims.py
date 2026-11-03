"""Tests for tools/check_reserved_shims.py (supply-chain review item 2.4)."""

import unittest

import check_reserved_shims as crs


class ShimStemTests(unittest.TestCase):
    def test_windows_and_unix_paths(self):
        self.assertEqual(crs.shim_stem("bin\\sudo.exe"), "sudo")
        self.assertEqual(crs.shim_stem("tools/rg.exe"), "rg")
        self.assertEqual(crs.shim_stem("gpg"), "gpg")

    def test_new_file_means_everything_added(self):
        # A new manifest (old text empty) flags any reserved bin it declares.
        gained = crs.added_reserved_names("", 'bin = ["ssh.exe"]')
        self.assertEqual(gained, {"ssh"})


class GatePolicyTests(unittest.TestCase):
    def test_adding_reserved_name_is_flagged(self):
        gained = crs.added_reserved_names(
            'bin = ["rg.exe"]', 'bin = ["rg.exe", "sudo"]'
        )
        self.assertEqual(gained & crs.RESERVED_NAMES, {"sudo"})

    def test_pre_existing_reserved_name_is_allowed(self):
        gained = crs.added_reserved_names('bin = ["ssh.exe"]', 'bin = ["ssh.exe"]')
        self.assertEqual(gained & crs.RESERVED_NAMES, set())

    def test_non_reserved_addition_is_allowed(self):
        gained = crs.added_reserved_names('bin = ["rg"]', 'bin = ["rg", "jq"]')
        self.assertEqual(gained & crs.RESERVED_NAMES, set())

    def test_table_bin_entries(self):
        old = '[[bin]]\nname = "rg"\n'
        new = '[[bin]]\nname = "rg"\n[[bin]]\nname = "curl"\n'
        gained = crs.added_reserved_names(old, new)
        self.assertEqual(gained & crs.RESERVED_NAMES, {"curl"})

    def test_reserved_list_covers_the_review_examples(self):
        for name in ["sudo", "su", "ssh", "scp", "sftp", "git", "gpg", "python",
                     "python3", "py", "pip", "pip3", "node", "npm", "npx",
                     "bash", "sh", "zsh", "pwsh", "powershell", "cmd", "curl",
                     "wget", "docker", "kubectl", "aws", "az", "gcloud", "gh",
                     "pyenv", "brew", "java", "code", "openssl"]:
            self.assertIn(name, crs.RESERVED_NAMES, f"{name} must be reserved")


class ManifestParsingTests(unittest.TestCase):
    def test_malformed_toml_yields_no_names(self):
        # validate.yml's own gate reports parse errors; this tool must not crash.
        self.assertEqual(crs.manifest_shim_names("bin = [ oops"), set())

    def test_no_bin_section(self):
        self.assertEqual(crs.manifest_shim_names('name = "x"\n'), set())


if __name__ == "__main__":
    unittest.main()
