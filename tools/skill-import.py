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
from dataclasses import dataclass, replace
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
    # The name this skill is published under. Equal to `upstream_name` unless an
    # earlier source already claimed that name - see `resolve_names`.
    name: str
    description: str
    directory: Path
    relative_directory: str
    source: dict[str, Any]
    # The name in the upstream SKILL.md frontmatter, kept so the import can tell
    # a renamed skill from an untouched one and declare the rename.
    upstream_name: str

    @property
    def renamed(self) -> bool:
        return self.name != self.upstream_name


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


# Git for Windows ships core.autocrlf=true, which rewrites LF to CRLF on
# checkout. That changes the bytes of every file we hash: licence digests and
# skill archives would then differ between a maintainer's Windows box and Linux
# CI, so pins refreshed on one fail on the other and archives built on one fail
# their manifest sha256 on the other. Forced off for every invocation, so the
# checkout is byte-identical to what upstream serves regardless of host config.
GIT_BYTE_EXACT = ("-c", "core.autocrlf=false", "-c", "core.eol=lf")


def git(*args: str) -> str:
    result = subprocess.run(
        ["git", *GIT_BYTE_EXACT, *args],
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
    return parse_frontmatter_text(text, str(path))


def parse_frontmatter_text(text: str, path: str | Path) -> tuple[str, str]:
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


def find_skill_files(source: dict[str, Any], checkout: Path) -> dict[str, Path]:
    """Every `SKILL.md` this source ships, keyed by directory relative to the
    checkout. One definition of "which skills are in scope", shared by the import
    and by the license review, so the two can never disagree about what to check.
    """
    roots = source.get("roots")
    if not isinstance(roots, list) or not roots:
        raise ImportFailure(f"{source.get('id', 'source')} has no roots")
    prefixes = [safe_relative(item, "exclude") for item in source.get("exclude", [])]
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
    return found


def discover_skills(source: dict[str, Any], checkout: Path) -> list[Skill]:
    allowed = {required_string(source, "license_sha256").lower()}
    allowed.update(value.lower() for value in source.get("allowed_skill_license_sha256", []))
    require_skill_license = bool(source.get("require_skill_license", False))
    found = find_skill_files(source, checkout)

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
                upstream_name=name,
            )
        )
    return skills


# ------------------------------------------------------------ name collisions
#
# Two upstreams shipping the same skill name is normal and gets more common with
# every source added, so it resolves rather than failing the sync. The bare name
# belongs to the FIRST source to claim it in skill-sources.toml order; later
# claimants are published as `<prefix>-<name>`.
#
# First-claimant-keeps-it is the load-bearing half. If both sides were prefixed
# instead, a skill that is unique today would silently RENAME the day some other
# source ships that name - breaking `voli upgrade` and orphaning the installed
# directory on every machine that has it. Source order in skill-sources.toml is
# the curator's precedence list. `exclude` still drops a genuine duplicate.


def source_prefix(source: dict[str, Any]) -> str:
    """`prefix` if set, else the source id with a trailing `-skills` stripped
    (`mattpocock-skills` -> `mattpocock`)."""
    explicit = source.get("prefix")
    prefix = (
        required_string(source, "prefix")
        if explicit is not None
        else required_string(source, "id").removesuffix("-skills")
    )
    if not NAME_RE.fullmatch(prefix):
        raise ImportFailure(
            f"{source.get('id', 'source')}: collision prefix {prefix!r} is not a valid "
            "skill-name fragment; set an explicit `prefix` in skill-sources.toml"
        )
    return prefix


def resolve_names(skills: list[Skill]) -> tuple[list[Skill], list[Skill]]:
    """Assign every skill a unique published name, in source order.

    Returns the resolved skills and, separately, the ones that were renamed.
    """
    claimed: dict[str, Skill] = {}
    resolved: list[Skill] = []
    for skill in skills:
        if skill.name not in claimed:
            claimed[skill.name] = skill
            resolved.append(skill)
            continue
        holder = claimed[skill.name]
        candidate = f"{source_prefix(skill.source)}-{skill.name}"
        if not NAME_RE.fullmatch(candidate) or len(candidate) > 64:
            raise ImportFailure(
                f"{skill.source['repo']}/{skill.relative_directory}: prefixed name "
                f"{candidate!r} is not a valid skill name (lowercase alphanumeric and "
                "dashes, 64 chars max); set a shorter `prefix` or exclude the skill"
            )
        if candidate in claimed:
            raise ImportFailure(
                f"{skill.source['repo']}/{skill.relative_directory}: {skill.name!r} "
                f"collides with {holder.source['repo']} and the prefixed name "
                f"{candidate!r} is taken too; set an explicit `prefix` or `exclude`"
            )
        renamed = replace(skill, name=candidate)
        claimed[candidate] = renamed
        resolved.append(renamed)
    return resolved, [skill for skill in resolved if skill.renamed]


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


def rewrite_frontmatter_name(contents: bytes, skill: Skill) -> bytes:
    """Return `contents` with the frontmatter `name:` value set to
    `skill.name`, and every other byte untouched.

    The client (voli-core `skill.rs`) enforces a three-way match between the
    manifest name, the archive's top-level directory, and this field, so a
    prefixed skill has to carry the prefixed name here too. The line's ending is
    preserved, the rest of the file is copied verbatim, and the result is parsed
    back to prove the edit landed on the real key and changed nothing else.
    """
    lines = contents.splitlines(keepends=True)
    if not lines or lines[0].strip() != b"---":
        raise ImportFailure(f"{skill.relative_directory}/SKILL.md has no YAML frontmatter")
    for index in range(1, len(lines)):
        stripped = lines[index].rstrip(b"\r\n")
        if stripped.strip() == b"---":
            break
        if not stripped.startswith(b"name:"):
            continue
        ending = lines[index][len(stripped) :]
        lines[index] = b"name: " + skill.name.encode("utf-8") + ending
        rewritten = b"".join(lines)
        name, description = parse_frontmatter_text(
            rewritten.decode("utf-8"), f"{skill.relative_directory}/SKILL.md"
        )
        if name != skill.name or description != skill.description:
            raise ImportFailure(
                f"{skill.relative_directory}/SKILL.md did not survive the rename to "
                f"{skill.name!r}: got name {name!r}"
            )
        return rewritten
    raise ImportFailure(
        f"{skill.relative_directory}/SKILL.md has no frontmatter name to rename"
    )


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
            contents = path.read_bytes()
            if relative == "SKILL.md" and skill.renamed:
                contents = rewrite_frontmatter_name(contents, skill)
            write_zip_entry(archive, f"{skill.name}/{relative}", contents)
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


# --------------------------------------------------------------- license review
#
# Pinning a license by hash is a tripwire: it fires when upstream's licence text
# changes. A revision bump changes that text routinely (a new year in a copyright
# line is enough), so the pins have to be re-approved as part of the bump or the
# next import fails. Re-approval must NOT mean "the text moved, so trust the new
# text" — that would disarm the tripwire it is meant to keep. It means: the file
# still grants the permissive licence the source declares, with the grant intact
# and nothing restrictive added. Anything else is a genuine relicense and stops
# the run for a human.
#
# Identification is by the licence's own operative clauses rather than by an
# exact body match, because real repositories legitimately wrap lines
# differently and append their own notice to an Apache LICENSE. The denylist is
# then scanned across the whole file, so a restriction appended below an
# otherwise-standard grant is still caught.

LICENSE_GRANTS: dict[str, tuple[str, ...]] = {
    "MIT": (
        "permission is hereby granted, free of charge, to any person obtaining a copy",
        "without restriction, including without limitation the rights to use, copy, "
        "modify, merge, publish, distribute, sublicense, and/or sell",
        'the software is provided "as is", without warranty of any kind',
    ),
    "Apache-2.0": (
        "apache license version 2.0, january 2004",
        "http://www.apache.org/licenses/",
        "perpetual, worldwide, non-exclusive, no-charge, royalty-free, irrevocable "
        "copyright license",
        # Section 7's wording, which is part of the terms. The appendix phrases
        # this as "software distributed under the License is distributed on an
        # ...", but the appendix is optional boilerplate and real repositories
        # ship terms-only Apache files without it.
        'on an "as is" basis, without warranties or conditions of any kind',
    ),
}

# Phrases that contradict a permissive grant. None of these appear in a standard
# MIT or Apache-2.0 text, so a hit means a term was added or the licence is a
# different one wearing a familiar filename.
RESTRICTIVE_MARKERS: tuple[str, ...] = (
    "noncommercial",
    "non-commercial",
    "no derivative works",
    "noderivatives",
    "evaluation purposes only",
    "evaluation use only",
    "internal use only",
    "personal use only",
    "commercial use is prohibited",
    "may not be used for commercial",
    "proprietary and confidential",
    "source-available",
    "gnu general public license",
    "gnu affero general public license",
    "gnu lesser general public license",
    "mozilla public license",
    "creative commons",
)


def normalize_license(text: str) -> str:
    """Lowercase, straighten quotes, and collapse whitespace, so that line
    wrapping and typographic quotes cannot change the verdict."""
    lowered = text.lower().replace("’", "'")
    for quote in ("“", "”"):
        lowered = lowered.replace(quote, '"')
    return " ".join(lowered.split())


def review_license_file(path: Path, declared: str, label: str) -> None:
    """Raise unless `path` still grants `declared` with its grant intact and no
    restrictive term anywhere in it."""
    expected = LICENSE_GRANTS.get(declared)
    if expected is None:
        raise ImportFailure(
            f"{label}: license {declared!r} is not auto-approvable; "
            f"known types are {', '.join(sorted(LICENSE_GRANTS))}"
        )
    body = normalize_license(path.read_text(encoding="utf-8", errors="replace"))
    missing = [phrase for phrase in expected if phrase not in body]
    if missing:
        raise ImportFailure(
            f"{label}: does not read as standard {declared} - missing {missing[0]!r}. "
            "This is a relicense, not a routine text change: review it by hand and "
            "update skill-sources.toml deliberately."
        )
    for marker in RESTRICTIVE_MARKERS:
        if marker in body:
            raise ImportFailure(
                f"{label}: {declared} text carries a restrictive term ({marker!r}). "
                "Review it by hand; this source cannot be auto-approved."
            )


def review_licenses(source: dict[str, Any], checkout: Path) -> tuple[str, list[str]]:
    """Re-approve every license this source ships at its current checkout.

    Returns the source license digest and the sorted digests of the per-skill
    LICENSE files, ready to be pinned. Raises naming the offending file if any of
    them is no longer the declared permissive type.
    """
    declared = required_string(source, "license")
    repo = required_string(source, "repo")
    relative = safe_relative(required_string(source, "license_file"), "license_file")
    source_license = checkout / relative
    if not source_license.is_file():
        raise ImportFailure(f"configured source license is missing: {source_license}")
    review_license_file(source_license, declared, f"{repo}/{relative}")

    skill_digests: set[str] = set()
    for skill_relative, skill_md in sorted(find_skill_files(source, checkout).items()):
        direct = find_direct_license(skill_md.parent)
        if direct is None:
            continue
        review_license_file(direct, declared, f"{repo}/{skill_relative}/{direct.name}")
        skill_digests.add(sha256_file(direct))
    source_digest = sha256_file(source_license)
    # A skill LICENSE identical to the source's is already covered by the source
    # pin; keeping it out of the allowlist keeps the file free of noise.
    skill_digests.discard(source_digest)
    return source_digest, sorted(skill_digests)


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

    # `all_skills` is in skill-sources.toml order (outer loop), then sorted
    # relative path (discover_skills), so precedence is deterministic.
    all_skills, renamed = resolve_names(all_skills)

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
    lines.extend(["", "## Name collisions", ""])
    if renamed:
        lines.extend(
            [
                "Two or more sources shipped the same skill name. The bare name stays with",
                "the first source in `skill-sources.toml` order; every later claimant is",
                "published under `<prefix>-<name>`.",
                "",
                "Renaming is not metadata-only: the client requires the manifest name, the",
                "archive's top-level directory, and the `name:` field in the archived",
                "`SKILL.md` to agree. So for each row below the importer rewrites that one",
                "frontmatter field inside the archive. Every other byte of upstream content",
                "is copied verbatim.",
                "",
                "| Upstream name | Published as | Source | Path |",
                "| --- | --- | --- | --- |",
            ]
        )
        for skill in sorted(renamed, key=lambda item: item.name):
            lines.append(
                f"| `{skill.upstream_name}` | `{skill.name}` | "
                f"`{skill.source['repo']}` | `{skill.relative_directory}` |"
            )
    else:
        lines.append("None: every skill name was unique across all sources.")
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


def render_allowlist(digests: list[str]) -> str:
    """The `allowed_skill_license_sha256` array, in the file's existing style."""
    if not digests:
        return ""
    body = "".join(f'  "{digest}",\n' for digest in digests)
    return f"allowed_skill_license_sha256 = [\n{body}]\n"


def set_allowlist(block: str, digests: list[str]) -> str:
    """Replace, insert, or drop the allowlist array in one source block."""
    rendered = render_allowlist(digests)
    existing = re.search(r"(?ms)^allowed_skill_license_sha256 = \[.*?^\]\n", block)
    if existing:
        return block[: existing.start()] + rendered + block[existing.end() :]
    if not rendered:
        return block
    # Keep it next to the source pin it extends.
    anchor = re.search(r'(?m)^license_sha256 = "[0-9a-f]{64}"\n', block)
    if not anchor:
        raise ImportFailure("source block has no license_sha256 to anchor the allowlist")
    return block[: anchor.end()] + rendered + block[anchor.end() :]


def rewrite_pins(
    text: str, pins: dict[str, "SourcePin"], version: str
) -> tuple[str, list[tuple[str, str, str]]]:
    """Apply reviewed pins. A source's revision, version, license digest, and
    skill-license allowlist move together, so a bumped revision can never leave
    the licence pins describing the revision before it.
    """
    parts = text.split("[[source]]")
    changed: list[tuple[str, str, str]] = []
    rewritten = [parts[0]]
    for raw_block in parts[1:]:
        block = "[[source]]" + raw_block
        parsed = tomllib.loads(block)["source"][0]
        source_id = required_string(parsed, "id")
        old_revision = required_sha(parsed, "revision")
        pin = pins.get(source_id)
        if pin is None:
            rewritten.append(block)
            continue
        new_revision = pin.revision.lower()
        old_license = required_string(parsed, "license_sha256").lower()
        old_allowed = sorted(
            value.lower() for value in parsed.get("allowed_skill_license_sha256", [])
        )
        moved = (
            new_revision != old_revision
            or pin.license_sha256 != old_license
            or pin.allowed_skill_license_sha256 != old_allowed
        )
        if not moved:
            rewritten.append(block)
            continue
        required_sha({"revision": new_revision}, "revision")
        block = re.sub(
            r'(?m)^revision = "[0-9a-f]{40}"$',
            f'revision = "{new_revision}"',
            block,
            count=1,
        )
        if new_revision != old_revision:
            block = re.sub(
                r'(?m)^version = "[^"]+"$',
                f'version = "{version}"',
                block,
                count=1,
            )
        block = re.sub(
            r'(?m)^license_sha256 = "[0-9a-f]{64}"$',
            f'license_sha256 = "{pin.license_sha256}"',
            block,
            count=1,
        )
        block = set_allowlist(block, pin.allowed_skill_license_sha256)
        changed.append((source_id, old_revision, new_revision))
        rewritten.append(block)
    return "".join(rewritten), changed


def fetch_revision(repo: str, revision: str, checkouts: Path) -> Path:
    """Materialize `repo` at `revision`, moving an existing checkout if needed.
    Unlike `checkout_source` this is used before the config is updated, so the
    working tree is expected to be at some other revision.
    """
    destination = checkouts / repo.lower().replace("/", "--")
    if not destination.exists():
        destination.parent.mkdir(parents=True, exist_ok=True)
        git("init", str(destination))
        git(
            "-C",
            str(destination),
            "remote",
            "add",
            "origin",
            f"https://github.com/{repo}.git",
        )
    safe = f"safe.directory={destination.resolve().as_posix()}"
    git("-c", safe, "-C", str(destination), "fetch", "--depth", "1", "origin", revision)
    git("-c", safe, "-C", str(destination), "checkout", "--detach", "FETCH_HEAD")
    head = git("-c", safe, "-C", str(destination), "rev-parse", "HEAD")
    if head.lower() != revision:
        raise ImportFailure(f"{repo} checkout is {head}, expected {revision}")
    return destination


@dataclass(frozen=True)
class SourcePin:
    """One source's pins, all resolved at the same revision."""

    revision: str
    license_sha256: str
    allowed_skill_license_sha256: list[str]


def resolve_head(repo: str) -> str:
    output = git("ls-remote", f"https://github.com/{repo}.git", "HEAD")
    try:
        revision = output.split()[0].lower()
    except IndexError as error:
        raise ImportFailure(f"{repo} did not return a HEAD revision") from error
    required_sha({"revision": revision}, "revision")
    return revision


def refresh_pins(path: Path, checkouts: Path) -> int:
    """Advance every source to upstream HEAD and re-approve its licenses there.

    Every source is reviewed on every run, not just the ones whose revision
    moved, so a pin that drifted earlier is repaired rather than inherited.
    """
    text = path.read_text(encoding="utf-8")
    config = tomllib.loads(text)
    pins: dict[str, SourcePin] = {}
    for source in config.get("source", []):
        source_id = required_string(source, "id")
        repo = required_string(source, "repo")
        revision = resolve_head(repo)
        checkout = fetch_revision(repo, revision, checkouts)
        license_sha256, allowed = review_licenses(source, checkout)
        pins[source_id] = SourcePin(revision, license_sha256, allowed)
    now = datetime.now(timezone.utc)
    updated, changed = rewrite_pins(text, pins, f"{now.year}.{now.month}.{now.day}")
    if changed:
        path.write_text(updated, encoding="utf-8", newline="\n")
        for source_id, old, new in changed:
            if old == new:
                print(f"re-approved {source_id} licenses at {new[:12]}")
            else:
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
        new_license = "b" * 64
        allowed = ["c" * 64, "d" * 64]
        updated, changed = rewrite_pins(
            sample,
            {"test-skills": SourcePin(new_revision, new_license, allowed)},
            "2026.7.28",
        )
        assert len(changed) == 1
        assert f'revision = "{new_revision}"' in updated
        assert 'version = "2026.7.28"' in updated
        # The licence pins move with the revision - the bug this guards is a
        # bumped revision left describing the previous revision's licences.
        assert f'license_sha256 = "{new_license}"' in updated
        for digest in allowed:
            assert f'"{digest}"' in updated
        assert tomllib.loads(updated)["source"][0][
            "allowed_skill_license_sha256"
        ] == allowed

        # An emptied allowlist is removed rather than left behind stale.
        emptied, _ = rewrite_pins(
            updated, {"test-skills": SourcePin(new_revision, new_license, [])}, "2026.7.28"
        )
        assert "allowed_skill_license_sha256" not in emptied
        assert tomllib.loads(emptied)["source"][0]["license_sha256"] == new_license

        # ---- licence review: what a bump may and may not auto-approve --------
        review = root / "review"
        review.mkdir()
        mit = (
            "MIT License\n\nCopyright (c) 2026 Example\n\n"
            "Permission is hereby granted, free of charge, to any person obtaining a copy\n"
            "of this software and associated documentation files (the \"Software\"), to deal\n"
            "in the Software without restriction, including without limitation the rights\n"
            "to use, copy, modify, merge, publish, distribute, sublicense, and/or sell\n"
            "copies of the Software, and to permit persons to whom the Software is\n"
            "furnished to do so, subject to the following conditions:\n\n"
            "The above copyright notice and this permission notice shall be included in all\n"
            "copies or substantial portions of the Software.\n\n"
            "THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR\n"
            "IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY.\n"
        )
        good = review / "LICENSE"
        good.write_text(mit, encoding="utf-8", newline="\n")
        review_license_file(good, "MIT", "fixture")

        # A new copyright year is exactly the routine drift that used to break
        # the sync; it must still pass review.
        rolled = review / "LICENSE.rolled"
        rolled.write_text(mit.replace("2026", "2027"), encoding="utf-8", newline="\n")
        review_license_file(rolled, "MIT", "fixture")

        # A terms-only Apache file (ending at END OF TERMS AND CONDITIONS, with
        # no appendix) is complete and valid - anthropics/skills ships several.
        # An earlier version of this reviewer required an appendix-only phrase
        # and rejected them all.
        apache_terms = (
            "Apache License\nVersion 2.0, January 2004\n"
            "http://www.apache.org/licenses/\n\n"
            "2. Grant of Copyright License. Subject to the terms and conditions of this\n"
            "License, each Contributor hereby grants to You a perpetual, worldwide,\n"
            "non-exclusive, no-charge, royalty-free, irrevocable copyright license to\n"
            "reproduce, prepare Derivative Works of, and distribute the Work.\n\n"
            "7. Disclaimer of Warranty. Unless required by applicable law or agreed to in\n"
            "writing, Licensor provides the Work (and each Contributor provides its\n"
            "Contributions) on an \"AS IS\" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY\n"
            "KIND, either express or implied.\n\n"
            "END OF TERMS AND CONDITIONS\n"
        )
        terms_only = review / "LICENSE.apache-terms"
        terms_only.write_text(apache_terms, encoding="utf-8", newline="\n")
        review_license_file(terms_only, "Apache-2.0", "fixture")

        def must_reject(name: str, text: str, declared: str = "MIT") -> None:
            path = review / name
            path.write_text(text, encoding="utf-8", newline="\n")
            try:
                review_license_file(path, declared, "fixture")
            except ImportFailure:
                return
            raise ImportFailure(f"review should have rejected {name}")

        # A restriction appended below an otherwise-standard grant.
        must_reject("LICENSE.rider", mit + "\nThis software is for NonCommercial use.\n")
        # The grant removed - a relicense wearing a familiar filename.
        must_reject("LICENSE.gone", "All rights reserved. Contact us to licence.\n")
        # Right text, wrong declared type.
        must_reject("LICENSE.apache", mit, "Apache-2.0")
        # A licence type outside the auto-approvable set.
        must_reject("LICENSE.cc", mit, "CC-BY-4.0")

        # A source whose skills carry their own licence gets them collected.
        skill_license = skill / "LICENSE"
        skill_license.write_text(mit, encoding="utf-8", newline="\n")
        (checkout / "LICENSE").write_text(mit, encoding="utf-8", newline="\n")
        source_digest, skill_digests = review_licenses(config["source"][0], checkout)
        assert source_digest == sha256_file(checkout / "LICENSE")
        # Identical to the source licence, so it needs no separate entry.
        assert skill_digests == []
        skill_license.write_text(
            mit.replace("Example", "Example Skill"), encoding="utf-8", newline="\n"
        )
        _, skill_digests = review_licenses(config["source"][0], checkout)
        assert skill_digests == [sha256_file(skill_license)]
        # And a skill that relicenses stops the run.
        skill_license.write_text(mit + "\nNoncommercial only.\n", encoding="utf-8", newline="\n")
        try:
            review_licenses(config["source"][0], checkout)
        except ImportFailure:
            pass
        else:
            raise ImportFailure("review should have rejected a relicensed skill")
        skill_license.unlink()

        # ---- name collisions -------------------------------------------------
        # Three sources ship `prototype`. The first in config order keeps the
        # bare name; the rest are prefixed. This is the case that used to abort
        # the whole 267-skill sync.
        plain_license = b"Plain license\n"
        plain_sha = hashlib.sha256(plain_license).hexdigest()

        def fixture_source(source_id: str, names: list[str]) -> dict[str, Any]:
            repo = f"test/{source_id}"
            where = root / "checkouts" / repo.lower().replace("/", "--")
            (where / "skills").mkdir(parents=True)
            (where / "LICENSE").write_bytes(plain_license)
            for name in names:
                directory = where / "skills" / name
                directory.mkdir()
                (directory / "SKILL.md").write_text(
                    f"---\nname: {name}\n"
                    f"description: The {name} skill from {source_id}.\n"
                    f"---\n# {name}\n\nBody from {source_id}.\n",
                    encoding="utf-8",
                    newline="\n",
                )
            git("init", str(where))
            git("-C", str(where), "config", "user.name", "test")
            git("-C", str(where), "config", "user.email", "test@example.com")
            git("-C", str(where), "add", ".")
            git("-C", str(where), "commit", "-m", "fixture")
            return {
                "id": source_id,
                "repo": repo,
                "revision": git("-C", str(where), "rev-parse", "HEAD"),
                "version": "1.0.0",
                "license": "MIT",
                "license_file": "LICENSE",
                "license_sha256": plain_sha,
                "roots": ["skills"],
            }

        collide = {
            "release_base": "https://example.com/releases/download/skills",
            "source": [
                fixture_source("alpha-skills", ["prototype", "alpha-only"]),
                fixture_source("beta-skills", ["prototype"]),
                fixture_source("gamma-skills", ["prototype"]),
            ],
        }
        published: list[list[str]] = []
        for run in ("three", "four"):
            assets = root / run / "assets"
            manifests = root / run / "manifests"
            report = root / run / "report.md"
            import_catalog(
                collide,
                root / "checkouts",
                manifests,
                assets,
                report,
                offline=True,
            )
            # Every rename is visible in review, along with the fact that the
            # archived SKILL.md was edited.
            text = report.read_text(encoding="utf-8")
            assert "| `prototype` | `beta-prototype` | `test/beta-skills` |" in text, text
            assert "| `prototype` | `gamma-prototype` | `test/gamma-skills` |" in text
            assert "frontmatter field inside the archive" in text
            published.append(sorted(path.name for path in assets.glob("*.zip")))
            # First claimant keeps the bare name; later ones are prefixed with
            # the source id minus its trailing `-skills`.
            assert published[-1] == [
                "alpha-skills-1.0.0-alpha-only.zip",
                "alpha-skills-1.0.0-prototype.zip",
                "beta-skills-1.0.0-beta-prototype.zip",
                "gamma-skills-1.0.0-gamma-prototype.zip",
            ], published[-1]
            for name in ("prototype", "beta-prototype", "gamma-prototype"):
                # All three names agree: manifest, archive directory, SKILL.md.
                manifest = manifests / "skills" / name[0] / name / "1.0.0.toml"
                assert tomllib.loads(manifest.read_text(encoding="utf-8"))["name"] == name
                source_id = {"prototype": "alpha"}.get(name, name.split("-")[0])
                archive = assets / f"{source_id}-skills-1.0.0-{name}.zip"
                with zipfile.ZipFile(archive) as zipped:
                    entries = sorted(zipped.namelist())
                    assert entries == [
                        f"{name}/LICENSE.upstream",
                        f"{name}/SKILL.md",
                    ], entries
                    body = zipped.read(f"{name}/SKILL.md").decode("utf-8")
                assert f"name: {name}\n" in body, body
            # Only the frontmatter name moves; the rest is byte-identical.
            upstream = (
                root
                / "checkouts"
                / "test--beta-skills"
                / "skills"
                / "prototype"
                / "SKILL.md"
            ).read_bytes()
            with zipfile.ZipFile(assets / "beta-skills-1.0.0-beta-prototype.zip") as zipped:
                archived = zipped.read("beta-prototype/SKILL.md")
            assert archived == upstream.replace(
                b"name: prototype\n", b"name: beta-prototype\n"
            )
        assert published[0] == published[1], "collision resolution is not deterministic"

        # Prefix derivation, and the explicit per-source override.
        assert source_prefix({"id": "mattpocock-skills"}) == "mattpocock"
        assert source_prefix({"id": "obra-superpowers"}) == "obra-superpowers"
        assert source_prefix({"id": "x-skills", "prefix": "matt"}) == "matt"

        # A prefixed name that would break the client's 64-char limit stops the
        # run rather than shipping a manifest the client refuses to install.
        long_name = "a" * 60
        def collider(source_id: str) -> Skill:
            source = {"id": source_id, "repo": f"test/{source_id}"}
            return Skill(long_name, "d", Path("."), f"skills/{long_name}", source, long_name)

        try:
            resolve_names([collider("alpha-skills"), collider("beta-skills")])
        except ImportFailure:
            pass
        else:
            raise ImportFailure("resolve_names should have rejected an over-long name")
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
            # Licenses are reviewed at the new revision, so a checkout is needed.
            # Reuse the import's if given, otherwise use a scratch one.
            if args.checkouts is not None:
                refresh_pins(args.config, args.checkouts)
            else:
                with tempfile.TemporaryDirectory(prefix="voli-skill-pins-") as scratch:
                    refresh_pins(args.config, Path(scratch))
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
