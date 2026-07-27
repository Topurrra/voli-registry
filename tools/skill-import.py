#!/usr/bin/env python3
"""Build deterministic Voli skill archives and manifests from pinned sources."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import tomllib
import zipfile
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any

MAX_FILES = 10_000
MAX_BYTES = 256 * 1024 * 1024
NAME_RE = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
LICENSE_NAMES = ("LICENSE", "LICENSE.txt", "LICENSE.md", "COPYING")


class ImportFailure(RuntimeError):
    pass


@dataclass(frozen=True)
class Skill:
    name: str
    description: str
    directory: Path
    relative_directory: str
    source: dict[str, Any]


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def safe_relative(value: str, field: str) -> str:
    path = PurePosixPath(value)
    if path.is_absolute() or ".." in path.parts:
        raise ImportFailure(f"{field} must be a safe relative path: {value}")
    normalized = path.as_posix()
    if normalized in ("", "."):
        return "."
    return normalized.removeprefix("./").rstrip("/")


def excluded(path: str, prefixes: list[str]) -> bool:
    return any(path == prefix or path.startswith(f"{prefix}/") for prefix in prefixes)


def git(*args: str) -> str:
    result = subprocess.run(
        ["git", *args],
        check=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    if result.returncode:
        detail = result.stderr.strip() or result.stdout.strip()
        raise ImportFailure(f"git {' '.join(args)} failed: {detail}")
    return result.stdout.strip()


def checkout_source(source: dict[str, Any], checkouts: Path, offline: bool) -> Path:
    repo = required_string(source, "repo")
    revision = required_sha(source, "revision")
    destination = checkouts / repo.lower().replace("/", "--")
    if not destination.exists():
        if offline:
            raise ImportFailure(f"offline checkout is missing: {destination}")
        destination.parent.mkdir(parents=True, exist_ok=True)
        git("init", str(destination))
        git("-C", str(destination), "remote", "add", "origin", f"https://github.com/{repo}.git")
        git("-C", str(destination), "fetch", "--depth", "1", "origin", revision)
        git("-C", str(destination), "checkout", "--detach", "FETCH_HEAD")
    head = git(
        "-c",
        f"safe.directory={destination.resolve().as_posix()}",
        "-C",
        str(destination),
        "rev-parse",
        "HEAD",
    )
    if head.lower() != revision:
        raise ImportFailure(f"{repo} checkout is {head}, expected {revision}")
    return destination


def required_string(item: dict[str, Any], key: str) -> str:
    value = item.get(key)
    if not isinstance(value, str) or not value.strip():
        raise ImportFailure(f"missing non-empty {key}")
    return value.strip()


def required_sha(item: dict[str, Any], key: str) -> str:
    value = required_string(item, key).lower()
    if not re.fullmatch(r"[0-9a-f]{40}", value):
        raise ImportFailure(f"{key} must be a 40-character git revision")
    return value


def parse_scalar(value: str) -> str:
    value = value.strip()
    if not value:
        return ""
    if value.startswith('"'):
        try:
            parsed = json.loads(value)
        except json.JSONDecodeError as error:
            raise ImportFailure(f"invalid quoted YAML scalar: {error}") from error
        if not isinstance(parsed, str):
            raise ImportFailure("frontmatter scalar must be text")
        return parsed
    if value.startswith("'") and value.endswith("'"):
        return value[1:-1].replace("''", "'")
    return value


def parse_frontmatter(path: Path) -> tuple[str, str]:
    try:
        text = path.read_text(encoding="utf-8")
    except UnicodeDecodeError as error:
        raise ImportFailure(f"{path} is not UTF-8") from error
    lines = text.splitlines()
    if not lines or lines[0].strip() != "---":
        raise ImportFailure(f"{path} has no YAML frontmatter")
    try:
        end = next(index for index in range(1, len(lines)) if lines[index].strip() == "---")
    except StopIteration as error:
        raise ImportFailure(f"{path} has unterminated YAML frontmatter") from error

    values: dict[str, str] = {}
    index = 1
    while index < end:
        line = lines[index]
        match = re.match(r"^([A-Za-z0-9_-]+):(?:\s*(.*))?$", line)
        if not match:
            index += 1
            continue
        key = match.group(1)
        raw = (match.group(2) or "").strip()
        if raw in ("|", "|-", "|+", ">", ">-", ">+"):
            block: list[str] = []
            index += 1
            while index < end and (not lines[index] or lines[index][0].isspace()):
                block.append(lines[index].strip())
                index += 1
            values[key] = " ".join(part for part in block if part).strip()
            continue
        values[key] = parse_scalar(raw)
        index += 1

    name = values.get("name", "").strip()
    description = " ".join(values.get("description", "").split())
    if not NAME_RE.fullmatch(name) or len(name) > 64:
        raise ImportFailure(f"{path} has invalid Agent Skills name: {name!r}")
    if not description:
        raise ImportFailure(f"{path} has no description")
    return name, description


def find_direct_license(directory: Path) -> Path | None:
    for name in LICENSE_NAMES:
        candidate = directory / name
        if candidate.is_file():
            return candidate
    return None


def discover_skills(source: dict[str, Any], checkout: Path) -> list[Skill]:
    roots = source.get("roots")
    if not isinstance(roots, list) or not roots:
        raise ImportFailure(f"{source.get('id', 'source')} has no roots")
    prefixes = [safe_relative(item, "exclude") for item in source.get("exclude", [])]
    allowed = {required_string(source, "license_sha256").lower()}
    allowed.update(value.lower() for value in source.get("allowed_skill_license_sha256", []))
    require_skill_license = bool(source.get("require_skill_license", False))
    found: dict[str, Path] = {}

    for configured_root in roots:
        root_rel = safe_relative(configured_root, "root")
        root = checkout if root_rel == "." else checkout / Path(root_rel)
        if not root.is_dir():
            raise ImportFailure(f"skill root does not exist: {root}")
        for skill_md in root.rglob("SKILL.md"):
            relative = skill_md.parent.relative_to(checkout).as_posix()
            if excluded(relative, prefixes):
                continue
            found[relative] = skill_md

    skills: list[Skill] = []
    for relative, skill_md in sorted(found.items()):
        name, description = parse_frontmatter(skill_md)
        direct_license = find_direct_license(skill_md.parent)
        if require_skill_license and direct_license is None:
            raise ImportFailure(f"{relative} must contain its own approved license")
        if direct_license is not None:
            digest = sha256_file(direct_license)
            if digest not in allowed:
                raise ImportFailure(
                    f"{direct_license} has unapproved license hash {digest}"
                )
        skills.append(
            Skill(
                name=name,
                description=description,
                directory=skill_md.parent,
                relative_directory=relative,
                source=source,
            )
        )
    return skills


def archive_files(skill: Skill) -> list[tuple[str, Path]]:
    files: list[tuple[str, Path]] = []
    total = 0
    for root, directories, names in os.walk(skill.directory, followlinks=False):
        root_path = Path(root)
        for directory in list(directories):
            path = root_path / directory
            if path.is_symlink():
                raise ImportFailure(f"skill contains a symlink: {path}")
        for name in names:
            path = root_path / name
            info = path.lstat()
            if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
                raise ImportFailure(f"skill contains an unsupported file: {path}")
            relative = path.relative_to(skill.directory).as_posix()
            files.append((relative, path))
            total += info.st_size
            if len(files) > MAX_FILES or total > MAX_BYTES:
                raise ImportFailure(f"{skill.name} exceeds the archive safety limits")
    return sorted(files)


def write_archive(skill: Skill, checkout: Path, output: Path) -> str:
    files = archive_files(skill)
    direct_license = find_direct_license(skill.directory)
    root_license = checkout / safe_relative(
        required_string(skill.source, "license_file"), "license_file"
    )
    output.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(
        output,
        "w",
        compression=zipfile.ZIP_DEFLATED,
        compresslevel=9,
        strict_timestamps=True,
    ) as archive:
        for relative, path in files:
            write_zip_entry(archive, f"{skill.name}/{relative}", path.read_bytes())
        if direct_license is None:
            write_zip_entry(
                archive,
                f"{skill.name}/LICENSE.upstream",
                root_license.read_bytes(),
            )
    return sha256_file(output)


def write_zip_entry(archive: zipfile.ZipFile, name: str, contents: bytes) -> None:
    info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
    info.create_system = 3
    info.compress_type = zipfile.ZIP_DEFLATED
    info.external_attr = 0o100644 << 16
    archive.writestr(info, contents, compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)


def toml_string(value: str) -> str:
    return json.dumps(value, ensure_ascii=False)


def write_manifest(skill: Skill, sha256: str, release_base: str, output: Path) -> None:
    source = skill.source
    source_id = required_string(source, "id")
    version = required_string(source, "version")
    repo = required_string(source, "repo")
    revision = required_sha(source, "revision")
    archive_name = f"{source_id}-{version}-{skill.name}.zip"
    homepage = (
        f"https://github.com/{repo}/tree/{revision}/{skill.relative_directory}"
    )
    url = f"{release_base.rstrip('/')}/{archive_name}"
    content = (
        f"name = {toml_string(skill.name)}\n"
        f"version = {toml_string(version)}\n"
        f"description = {toml_string(skill.description)}\n"
        f"homepage = {toml_string(homepage)}\n"
        f"license = {toml_string(required_string(source, 'license'))}\n"
        'kind = "skill"\n\n'
        "[source.any]\n"
        f"url = {toml_string(url)}\n"
        f"sha256 = {toml_string(sha256)}\n"
    )
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(content, encoding="utf-8", newline="\n")


def verify_source_license(source: dict[str, Any], checkout: Path) -> None:
    relative = safe_relative(required_string(source, "license_file"), "license_file")
    license_path = checkout / relative
    if not license_path.is_file():
        raise ImportFailure(f"configured source license is missing: {license_path}")
    actual = sha256_file(license_path)
    expected = required_string(source, "license_sha256").lower()
    if actual != expected:
        raise ImportFailure(
            f"{source['repo']} license changed: expected {expected}, found {actual}"
        )


def import_catalog(
    config: dict[str, Any],
    checkouts: Path,
    manifests: Path,
    assets: Path,
    report: Path,
    offline: bool,
) -> tuple[int, int]:
    release_base = required_string(config, "release_base")
    sources = config.get("source")
    if not isinstance(sources, list) or not sources:
        raise ImportFailure("configuration has no sources")

    checkouts_by_id: dict[str, Path] = {}
    all_skills: list[Skill] = []
    counts: list[tuple[dict[str, Any], int]] = []
    source_ids: set[str] = set()
    for source in sources:
        source_id = required_string(source, "id")
        if source_id in source_ids:
            raise ImportFailure(f"duplicate source id: {source_id}")
        source_ids.add(source_id)
        checkout = checkout_source(source, checkouts, offline)
        verify_source_license(source, checkout)
        skills = discover_skills(source, checkout)
        checkouts_by_id[source_id] = checkout
        all_skills.extend(skills)
        counts.append((source, len(skills)))

    by_name: dict[str, list[Skill]] = {}
    for skill in all_skills:
        by_name.setdefault(skill.name, []).append(skill)
    collisions = {name: items for name, items in by_name.items() if len(items) > 1}
    if collisions:
        details = "; ".join(
            f"{name}: {', '.join(item.source['repo'] for item in items)}"
            for name, items in sorted(collisions.items())
        )
        raise ImportFailure(f"duplicate skill names require an explicit exclusion: {details}")

    for skill in sorted(all_skills, key=lambda item: item.name):
        source_id = required_string(skill.source, "id")
        version = required_string(skill.source, "version")
        archive_name = f"{source_id}-{version}-{skill.name}.zip"
        digest = write_archive(skill, checkouts_by_id[source_id], assets / archive_name)
        manifest = manifests / "skills" / skill.name[0] / skill.name / f"{version}.toml"
        write_manifest(skill, digest, release_base, manifest)

    report.parent.mkdir(parents=True, exist_ok=True)
    lines = [
        "# Skill import report",
        "",
        f"Generated {len(all_skills)} deterministic skill archives from {len(sources)} pinned sources.",
        "",
        "| Source | Revision | Skills | License |",
        "| --- | --- | ---: | --- |",
    ]
    for source, count in counts:
        lines.append(
            f"| `{source['repo']}` | `{source['revision']}` | {count} | {source['license']} |"
        )
    lines.extend(
        [
            "",
            "Release publishing and PR merging are deferred.",
            "",
        ]
    )
    report.write_text("\n".join(lines), encoding="utf-8", newline="\n")
    return len(sources), len(all_skills)


def load_config(path: Path) -> dict[str, Any]:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def rewrite_pins(
    text: str, revisions: dict[str, str], version: str
) -> tuple[str, list[tuple[str, str, str]]]:
    parts = text.split("[[source]]")
    changed: list[tuple[str, str, str]] = []
    rewritten = [parts[0]]
    for raw_block in parts[1:]:
        block = "[[source]]" + raw_block
        parsed = tomllib.loads(block)["source"][0]
        source_id = required_string(parsed, "id")
        old_revision = required_sha(parsed, "revision")
        new_revision = revisions.get(source_id, old_revision).lower()
        if new_revision != old_revision:
            required_sha({"revision": new_revision}, "revision")
            block = re.sub(
                r'(?m)^revision = "[0-9a-f]{40}"$',
                f'revision = "{new_revision}"',
                block,
                count=1,
            )
            block = re.sub(
                r'(?m)^version = "[^"]+"$',
                f'version = "{version}"',
                block,
                count=1,
            )
            changed.append((source_id, old_revision, new_revision))
        rewritten.append(block)
    return "".join(rewritten), changed


def refresh_pins(path: Path) -> int:
    text = path.read_text(encoding="utf-8")
    config = tomllib.loads(text)
    revisions: dict[str, str] = {}
    for source in config.get("source", []):
        source_id = required_string(source, "id")
        repo = required_string(source, "repo")
        output = git("ls-remote", f"https://github.com/{repo}.git", "HEAD")
        try:
            revision = output.split()[0].lower()
        except IndexError as error:
            raise ImportFailure(f"{repo} did not return a HEAD revision") from error
        required_sha({"revision": revision}, "revision")
        revisions[source_id] = revision
    now = datetime.now(timezone.utc)
    updated, changed = rewrite_pins(
        text, revisions, f"{now.year}.{now.month}.{now.day}"
    )
    if changed:
        path.write_text(updated, encoding="utf-8", newline="\n")
        for source_id, old, new in changed:
            print(f"updated {source_id}: {old[:12]} -> {new[:12]}")
    else:
        print("ok: all skill source pins are current")
    return len(changed)


def self_test() -> None:
    with tempfile.TemporaryDirectory(prefix="voli-skill-import-") as temp:
        root = Path(temp)
        checkout = root / "checkouts" / "test--skills"
        skill = checkout / "skills" / "example-skill"
        ignored = checkout / "skills" / "ignored"
        skill.mkdir(parents=True)
        ignored.mkdir(parents=True)
        license_bytes = b"Example license\n"
        (checkout / "LICENSE").write_bytes(license_bytes)
        (skill / "SKILL.md").write_text(
            "---\nname: example-skill\ndescription: |\n  Deterministic example.\n---\n# Test\n",
            encoding="utf-8",
            newline="\n",
        )
        (skill / "reference.txt").write_bytes(b"reference\n")
        (ignored / "SKILL.md").write_text(
            "---\nname: ignored\ndescription: Ignored.\n---\n",
            encoding="utf-8",
            newline="\n",
        )
        git("init", str(checkout))
        git("-C", str(checkout), "config", "user.name", "test")
        git("-C", str(checkout), "config", "user.email", "test@example.com")
        git("-C", str(checkout), "add", ".")
        git("-C", str(checkout), "commit", "-m", "fixture")
        revision = git("-C", str(checkout), "rev-parse", "HEAD")
        license_sha = hashlib.sha256(license_bytes).hexdigest()
        config = {
            "release_base": "https://example.com/releases/download/skills",
            "source": [
                {
                    "id": "test-skills",
                    "repo": "test/skills",
                    "revision": revision,
                    "version": "1.0.0",
                    "license": "MIT",
                    "license_file": "LICENSE",
                    "license_sha256": license_sha,
                    "roots": ["skills"],
                    "exclude": ["skills/ignored"],
                }
            ],
        }
        hashes: list[str] = []
        for run in ("one", "two"):
            assets = root / run / "assets"
            import_catalog(
                config,
                root / "checkouts",
                root / run / "manifests",
                assets,
                root / run / "report.md",
                offline=True,
            )
            archive = assets / "test-skills-1.0.0-example-skill.zip"
            hashes.append(sha256_file(archive))
            with zipfile.ZipFile(archive) as zipped:
                names = sorted(zipped.namelist())
                assert names == [
                    "example-skill/LICENSE.upstream",
                    "example-skill/SKILL.md",
                    "example-skill/reference.txt",
                ]
        if hashes[0] != hashes[1]:
            raise ImportFailure("self-test archives are not deterministic")
        sample = (
            'release_base = "https://example.com"\n\n'
            "[[source]]\n"
            'id = "test-skills"\n'
            'repo = "test/skills"\n'
            f'revision = "{revision}"\n'
            'version = "1.0.0"\n'
            'license = "MIT"\n'
            'license_file = "LICENSE"\n'
            f'license_sha256 = "{license_sha}"\n'
            'roots = ["skills"]\n'
        )
        new_revision = "a" * 40
        updated, changed = rewrite_pins(
            sample, {"test-skills": new_revision}, "2026.7.28"
        )
        assert len(changed) == 1
        assert f'revision = "{new_revision}"' in updated
        assert 'version = "2026.7.28"' in updated
    print("ok: skill importer self-test passed")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--refresh-pins", action="store_true")
    parser.add_argument("--config", type=Path, default=Path("skill-sources.toml"))
    parser.add_argument("--checkouts", type=Path)
    parser.add_argument("--manifests", type=Path, default=Path("manifests"))
    parser.add_argument("--assets", type=Path)
    parser.add_argument("--report", type=Path, default=Path("skill-import-report.md"))
    parser.add_argument("--offline", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        if args.self_test:
            self_test()
            return 0
        if args.refresh_pins:
            refresh_pins(args.config)
            return 0
        if args.checkouts is None or args.assets is None:
            raise ImportFailure("--checkouts and --assets are required")
        sources, skills = import_catalog(
            load_config(args.config),
            args.checkouts,
            args.manifests,
            args.assets,
            args.report,
            args.offline,
        )
        print(f"imported {skills} skills from {sources} pinned sources")
        print(f"archives: {args.assets}")
        print(f"manifests: {args.manifests / 'skills'}")
        return 0
    except (ImportFailure, OSError, tomllib.TOMLDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
