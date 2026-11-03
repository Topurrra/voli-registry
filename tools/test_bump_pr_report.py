"""Tests for tools/bump_pr_report.py (supply-chain review item 4)."""

import unittest

import bump_pr_report as bpr


def fetch_map(mapping):
    """Fetcher stub: only the URLs in `mapping` return their text."""
    return lambda url: mapping.get(url)


CHECK = "a" * 64


class HostTests(unittest.TestCase):
    def test_host_of(self):
        self.assertEqual(bpr.host_of("https://CDN.example.com/x/y.zip"), "cdn.example.com")
        self.assertEqual(bpr.host_of("https://github.com/foo/bar/releases/download/v1/t.zip"),
                         "github.com")


class VendorChecksumTests(unittest.TestCase):
    URL = "https://vendor.example.com/tool-2.0.0.zip"

    def test_verified(self):
        status, detail = bpr.find_vendor_checksum(
            self.URL, CHECK, fetch_map({self.URL + ".sha256": CHECK + "  tool.zip"}))
        self.assertEqual(status, "verified")
        self.assertEqual(detail, self.URL + ".sha256")

    def test_mismatch(self):
        status, _ = bpr.find_vendor_checksum(
            self.URL, CHECK, fetch_map({self.URL + ".sha256": "b" * 64}))
        self.assertEqual(status, "mismatch")

    def test_none_when_vendor_publishes_nothing(self):
        status, detail = bpr.find_vendor_checksum(self.URL, CHECK, fetch_map({}))
        self.assertEqual((status, detail), ("none", ""))

    def test_signature_recorded_not_verified(self):
        status, detail = bpr.find_vendor_checksum(
            self.URL, CHECK, fetch_map({self.URL + ".sig": "-----BEGIN PGP SIGNATURE-----"}))
        self.assertEqual(status, "signature")


class ManifestParsingTests(unittest.TestCase):
    def test_sources_across_platform_blocks(self):
        src = bpr.manifest_sources(
            '[source.x64]\nurl = "https://a.com/t.zip"\nsha256 = "%s"\n'
            '[source.linux-x64]\nurl = "https://a.com/t.tar.gz"\nsha256 = "%s"\n' % (CHECK, CHECK)
        )
        self.assertEqual(set(src), {"source.x64", "source.linux-x64"})
        self.assertEqual(src["source.x64"]["sha256"], CHECK)

    def test_malformed_toml_is_empty(self):
        self.assertEqual(bpr.manifest_sources("nope ["), {})


class ReportRenderingTests(unittest.TestCase):
    def test_bold_flags(self):
        text = bpr.render_package(
            "tool", "2.0.0", [("source.x64", "cdn2.example.com")],
            host_changed=True, checksum_status="none", checksum_detail="")
        self.assertIn("**HOST CHANGED from the previous version**", text)
        self.assertIn("**no vendor checksum or signature published**", text)

    def test_changed_host_unbold_when_same(self):
        text = bpr.render_package(
            "tool", "2.0.0", [("source.x64", "cdn.example.com")],
            host_changed=False, checksum_status="verified",
            checksum_detail="https://cdn.example.com/t.zip.sha256")
        self.assertNotIn("**HOST CHANGED", text)
        self.assertIn("vendor checksum: verified", text)

    def test_mismatch_is_bold(self):
        text = bpr.render_package(
            "tool", "2.0.0", [("source.x64", "cdn.example.com")],
            host_changed=False, checksum_status="mismatch", checksum_detail="u")
        self.assertIn("**VENDOR CHECKSUM MISMATCH**", text)


def suite():
    """Loader used by the tool's --self-test."""
    suite = unittest.TestSuite()
    for cls in (HostTests, VendorChecksumTests, ManifestParsingTests, ReportRenderingTests):
        suite.addTests(unittest.TestLoader().loadTestsFromTestCase(cls))
    return suite


if __name__ == "__main__":
    unittest.main()
