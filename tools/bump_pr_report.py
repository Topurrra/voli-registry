#!/usr/bin/env python3
"""Vendor provenance report for auto-bump pull requests.

Supply-chain review item 4: `bump.yml` trusts whatever the vendor serves the
day it runs — a hacked vendor site yields a valid, CI-signed manifest, and the
review PR is the only safeguard. This tool makes that review effective by
writing, for every changed package:

  - the download host of the new version;
  - whether the host CHANGED from the previous version (bold flag);
  - whether the vendor publishes its own checksum or signature for the asset,
    and whether it matched the sha256 the bump computed (bold flag when no
    vendor checksum exists; a MISMATCH fails the whole bump).

Usage:
  python tools/bump_pr_report.py [--base origin/main] [--manifests manifests/]
                                 [--out pr-report.md]
  python tools/bump_pr_report.py --self-test
"""

import argparse


import subprocess
import sys
import tomllib
import urllib.request
from pathlib import Path
from urllib.parse import urlsplit

# Candidate suffixes for a vendor-published checksum next to the asset.
CHECKSUM_SUFFIXES = (".sha256", ".sha256sum", ".txt", ".sig", ".asc")


def host_of(url: str) -> str:
    return urlsplit(url).netloc.lower()


def manifest_sources(text: str) -> dict:
    """Map source-block label -> {url, sha256} for a manifest's TOML."""
    try:
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError:
        return {}
    out = {}
    # `[source.x64]` parses as nested dicts: source -> {x64: {...}}.
    for label, block in data.get("source", {}).items():
        if isinstance(block, dict):
            url = block.get("url")
            if isinstance(url, str):
                out[f"source.{label}"] = {"url": url, "sha256": str(block.get("sha256", ""))}
    return out


def default_fetch(url: str) -> str | None:
    """Fetch a small text URL, or None when it 404s/fails. Injectable for tests."""
    try:
        with urllib.request.urlopen(url, timeout=20) as resp:
            return resp.read(1024 * 1024).decode("utf-8", "replace")
    except Exception:
        return None


def find_vendor_checksum(url: str, sha256: str, fetch) -> tuple:
    """Look for a vendor checksum next to the asset.

    Returns (status, detail) where status is one of:
      verified   — found a checksum that matches
      mismatch   — found a checksum that does NOT match (caller must fail)
      signature  — found a signature we cannot verify, record it
      none       — nothing found (bold flag in the report)
    """
    for suffix in CHECKSUM_SUFFIXES:
        text = fetch(url + suffix)
        if text is None:
            continue
        first = text.split()[0].lower() if text.split() else ""
        if len(first) == 64 and all(c in "0123456789abcdef" for c in first):
            if first == sha256.lower():
                return "verified", url + suffix
            return "mismatch", url + suffix
        if suffix in (".sig", ".asc"):
            return "signature", url + suffix
    return "none", ""


def bold(flag: bool, text: str) -> str:
    return f"**{text}**" if flag else text


def render_package(name: str, version: str, hosts: list, host_changed: bool,
                   checksum_status: str, checksum_detail: str) -> str:
    lines = [f"### `{name}` {version}"]
    for label, host in hosts:
        lines.append(f"- download host ({label}): `{host}`")
    if host_changed:
        lines.append(f"- **HOST CHANGED from the previous version**")
    if checksum_status == "verified":
        lines.append(f"- vendor checksum: verified — {checksum_detail}")
    elif checksum_status == "mismatch":
        lines.append(f"- **VENDOR CHECKSUM MISMATCH** — {checksum_detail}")
    elif checksum_status == "signature":
        lines.append(f"- vendor signature present (not machine-verified): {checksum_detail}")
    else:
        lines.append(f"- **no vendor checksum or signature published** — only the bump's own sha256")
    return "\n".join(lines)


def collect_rows(base: str, manifests_dir: Path, fetch) -> tuple:
    """(sections, failed) for every manifest changed against base."""
    changed = [
        line.strip()
        for line in subprocess.run(
            ["git", "diff", "--name-only", f"{base}...", "--", str(manifests_dir)],
            check=True, capture_output=True, text=True,
        ).stdout.splitlines()
        if line.strip().endswith(".toml")
    ]
    sections, failed = [], False
    for path in changed:
        try:
            old = subprocess.run(["git", "show", f"{base}:{path}"], check=True,
                                 capture_output=True, text=True).stdout
        except subprocess.CalledProcessError:
            continue  # new manifest, nothing to compare hosts against
        new = Path(path).read_text(encoding="utf-8")
        old_src, new_src = manifest_sources(old), manifest_sources(new)
        if not new_src:
            continue
        pkg = Path(path).stem  # <version>.toml
        name = Path(path).parent.name
        old_hosts = {host_of(v["url"]) for v in old_src.values()}
        hosts = [(label, host_of(v["url"])) for label, v in sorted(new_src.items())]
        host_changed = bool(old_hosts) and any(h not in old_hosts for _, h in hosts)
        status, detail = "none", ""
        for v in new_src.values():
            status, detail = find_vendor_checksum(v["url"], v["sha256"], fetch)
            if status == "mismatch":
                failed = True
            break  # primary source block only; extras are recorded as hosts
        sections.append(render_package(name, pkg, hosts, host_changed, status, detail))
    return sections, failed


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", default="origin/main")
    parser.add_argument("--manifests", default="manifests")
    parser.add_argument("--out", default="pr-report.md")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        import unittest
        from test_bump_pr_report import suite
        runner = unittest.TextTestRunner(verbosity=2)
        return 0 if runner.run(suite()).wasSuccessful() else 1

    sections, failed = collect_rows(args.base, Path(args.manifests), default_fetch)
    with open(args.out, "w", encoding="utf-8") as f:
        f.write("## Vendor provenance report\n\n")
        f.write("Review the download hosts and vendor checksums before merging. ")
        f.write("Bold items need human attention.\n\n")
        if sections:
            f.write("\n\n".join(sections))
            f.write("\n")
        else:
            f.write("No package manifests changed.\n")
    if failed:
        print("::error::vendor checksum mismatch found — refusing to open the bump PR")
        return 1
    print(f"wrote {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
