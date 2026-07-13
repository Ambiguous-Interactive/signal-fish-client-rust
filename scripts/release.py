#!/usr/bin/env python3
"""Deterministic release preparation and artifact verification helpers."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import re
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path

SEMVER_RE = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
VERSION_FILES = (
    "README.md",
    "docs/client.md",
    "docs/examples.md",
    "docs/getting-started.md",
    "docs/index.md",
    "docs/mesh-guide.md",
    "docs/protocol.md",
    "docs/wasm.md",
    ".llm/context.md",
    ".llm/skills/crate-publishing.md",
    ".llm/skills/godot-websocket.md",
)
PROVENANCE_FILES = (
    "tests/compatibility.toml",
    "tests/server-spec/PROVENANCE.toml",
    "tests/wire-samples/PROVENANCE.toml",
)


class ReleaseError(RuntimeError):
    """A release invariant was not satisfied."""


def parse_version(value: str) -> tuple[int, int, int]:
    match = SEMVER_RE.fullmatch(value)
    if match is None:
        raise ReleaseError(f"invalid version {value!r}; expected strict X.Y.Z")
    return tuple(int(part) for part in match.groups())  # type: ignore[return-value]


def bump_version(current: str, level: str) -> str:
    major, minor, patch = parse_version(current)
    if level == "major":
        return f"{major + 1}.0.0"
    if level == "minor":
        return f"{major}.{minor + 1}.0"
    if level == "patch":
        return f"{major}.{minor}.{patch + 1}"
    raise ReleaseError(f"invalid bump {level!r}; expected major, minor, or patch")


def release_type(base: str, target: str) -> str:
    """Return the one-component SemVer bump from base to target."""
    base_parts = parse_version(base)
    target_parts = parse_version(target)
    major, minor, patch = base_parts
    if target_parts == (major + 1, 0, 0):
        return "major"
    if target_parts == (major, minor + 1, 0):
        return "minor"
    if target_parts == (major, minor, patch + 1):
        return "patch"
    raise ReleaseError(f"{base} to {target} is not a single major, minor, or patch bump")


def package_version(root: Path) -> str:
    cargo = (root / "Cargo.toml").read_text(encoding="utf-8")
    package = re.search(
        r"^\[package\][ \t]*$\n(.*?)(?=^\[|\Z)",
        cargo,
        re.MULTILINE | re.DOTALL,
    )
    if package is None:
        raise ReleaseError("Cargo.toml has no [package] section")
    match = re.search(r'^version = "([^"]+)"$', package.group(1), re.MULTILINE)
    if match is None:
        raise ReleaseError("Cargo.toml has no package version")
    parse_version(match.group(1))
    return match.group(1)


def previous_version(root: Path, version: str) -> str:
    parse_version(version)
    changelog = (root / "CHANGELOG.md").read_text(encoding="utf-8")
    pattern = re.compile(
        rf"^\[{re.escape(version)}\]: .*/compare/v"
        rf"({SEMVER_RE.pattern[1:-1]})\.\.\.v{re.escape(version)}$",
        re.MULTILINE,
    )
    match = pattern.search(changelog)
    if match is None:
        raise ReleaseError(f"CHANGELOG.md has no exact compare link for {version}")
    previous = match.group(1)
    parse_version(previous)
    return previous


def replace_required(path: Path, old: str, new: str) -> None:
    text = path.read_text(encoding="utf-8")
    if old not in text:
        raise ReleaseError(f"{path} does not contain required value {old!r}")
    path.write_text(text.replace(old, new), encoding="utf-8")


def cut_changelog(path: Path, old: str, new: str, date: str) -> None:
    text = path.read_text(encoding="utf-8")
    heading = "## [Unreleased]"
    start = text.find(heading)
    if start < 0:
        raise ReleaseError("CHANGELOG.md has no [Unreleased] section")
    content_start = start + len(heading)
    next_heading = text.find("\n## [", content_start)
    if next_heading < 0:
        raise ReleaseError("CHANGELOG.md has no released section after [Unreleased]")
    unreleased = text[content_start:next_heading].strip()
    if not unreleased:
        raise ReleaseError("CHANGELOG.md [Unreleased] section is empty")
    if f"## [{new}]" in text:
        raise ReleaseError(f"CHANGELOG.md already contains release {new}")

    body = (
        text[:start]
        + f"{heading}\n\n## [{new}] - {date}\n\n{unreleased}\n"
        + text[next_heading:]
    )
    reference_re = re.compile(r"^\[Unreleased\]:.*$", re.MULTILINE)
    release_link = (
        f"[{new}]: https://github.com/Ambiguous-Interactive/"
        f"signal-fish-client-rust/compare/v{old}...v{new}"
    )
    unreleased_link = (
        "[Unreleased]: https://github.com/Ambiguous-Interactive/"
        f"signal-fish-client-rust/compare/v{new}...HEAD"
    )
    if reference_re.search(body):
        body = reference_re.sub(f"{unreleased_link}\n{release_link}", body, count=1)
    else:
        body = body.rstrip() + f"\n\n{unreleased_link}\n{release_link}\n"
    path.write_text(body, encoding="utf-8")


def require_clean(root: Path) -> None:
    result = subprocess.run(
        ["git", "status", "--porcelain"],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
    )
    if result.stdout:
        raise ReleaseError("worktree must be clean before release preparation")


def prepare(root: Path, level: str, date: str, allow_dirty: bool = False) -> str:
    if re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", date) is None:
        raise ReleaseError(f"invalid date {date!r}; expected YYYY-MM-DD")
    dt.date.fromisoformat(date)
    if not allow_dirty:
        require_clean(root)
    old = package_version(root)
    new = bump_version(old, level)

    # Validate every required source before writing any file. A stale inventory
    # must not leave a plausible-looking partial release bump behind.
    for relative in VERSION_FILES:
        if old not in (root / relative).read_text(encoding="utf-8"):
            raise ReleaseError(f"{relative} does not contain required value {old!r}")
    compatibility_text = (root / "tests/compatibility.toml").read_text(encoding="utf-8")
    if len(re.findall(r'^client_version = "[^"]+"$', compatibility_text, re.MULTILINE)) != 1:
        raise ReleaseError("tests/compatibility.toml has no unique client_version")
    for relative in PROVENANCE_FILES:
        provenance = (root / relative).read_text(encoding="utf-8")
        if len(re.findall(r'^synced = "[0-9]{4}-[0-9]{2}-[0-9]{2}"$', provenance, re.MULTILINE)) != 1:
            raise ReleaseError(f"{relative} has no unique synced date")
    changelog_text = (root / "CHANGELOG.md").read_text(encoding="utf-8")
    unreleased_start = changelog_text.find("## [Unreleased]")
    next_release = changelog_text.find("\n## [", unreleased_start + len("## [Unreleased]"))
    if unreleased_start < 0 or next_release < 0:
        raise ReleaseError("CHANGELOG.md has no complete [Unreleased] section")
    if not changelog_text[unreleased_start + len("## [Unreleased]") : next_release].strip():
        raise ReleaseError("CHANGELOG.md [Unreleased] section is empty")
    if f"## [{new}]" in changelog_text:
        raise ReleaseError(f"CHANGELOG.md already contains release {new}")

    replace_required(root / "Cargo.toml", f'version = "{old}"', f'version = "{new}"')
    for relative in VERSION_FILES:
        replace_required(root / relative, old, new)
    compatibility = root / "tests/compatibility.toml"
    text = compatibility_text
    text = re.sub(
        r'^client_version = "[^"]+"$',
        f'client_version = "{new}"',
        text,
        count=1,
        flags=re.MULTILINE,
    )
    compatibility.write_text(text, encoding="utf-8")
    for relative in PROVENANCE_FILES:
        path = root / relative
        provenance = path.read_text(encoding="utf-8")
        provenance, count = re.subn(
            r'^synced = "[0-9]{4}-[0-9]{2}-[0-9]{2}"$',
            f'synced = "{date}"',
            provenance,
            count=1,
            flags=re.MULTILINE,
        )
        if count != 1:
            raise ReleaseError(f"{relative} has no unique synced date")
        path.write_text(provenance, encoding="utf-8")
    cut_changelog(root / "CHANGELOG.md", old, new, date)
    return new


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def verify_artifact(crate: Path, expected_checksum: str | None) -> str:
    if not crate.is_file():
        raise ReleaseError(f"crate artifact does not exist: {crate}")
    checksum = sha256(crate)
    if expected_checksum is not None and checksum != expected_checksum.lower():
        raise ReleaseError(
            f"crate checksum mismatch: local {checksum}, registry {expected_checksum.lower()}"
        )
    return checksum


def registry_checksum(crate_name: str, version: str) -> str | None:
    parse_version(version)
    request = urllib.request.Request(
        f"https://crates.io/api/v1/crates/{crate_name}/{version}",
        headers={"User-Agent": "signal-fish-client-release-workflow"},
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            payload = json.load(response)
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return None
        raise ReleaseError(f"crates.io query failed with HTTP {error.code}") from error
    checksum = payload.get("version", {}).get("checksum")
    if not isinstance(checksum, str) or not re.fullmatch(r"[0-9a-f]{64}", checksum):
        raise ReleaseError("crates.io returned an invalid checksum")
    return checksum


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    prep = subparsers.add_parser("prepare")
    prep.add_argument("bump", choices=("major", "minor", "patch"))
    prep.add_argument("--root", type=Path, default=Path.cwd())
    prep.add_argument("--date", default=dt.date.today().isoformat())
    prep.add_argument("--allow-dirty", action="store_true", help=argparse.SUPPRESS)

    checksum = subparsers.add_parser("checksum")
    checksum.add_argument("crate", type=Path)
    checksum.add_argument("--expected")

    registry = subparsers.add_parser("registry-checksum")
    registry.add_argument("crate_name")
    registry.add_argument("version")

    previous = subparsers.add_parser("previous-version")
    previous.add_argument("version")
    previous.add_argument("--root", type=Path, default=Path.cwd())

    policy = subparsers.add_parser("release-type")
    policy.add_argument("base")
    policy.add_argument("target")

    args = parser.parse_args(argv)
    try:
        if args.command == "prepare":
            print(prepare(args.root.resolve(), args.bump, args.date, args.allow_dirty))
        elif args.command == "checksum":
            print(verify_artifact(args.crate, args.expected))
        elif args.command == "registry-checksum":
            value = registry_checksum(args.crate_name, args.version)
            print(value or "UNPUBLISHED")
        elif args.command == "previous-version":
            print(previous_version(args.root.resolve(), args.version))
        else:
            print(release_type(args.base, args.target))
    except (OSError, ValueError, ReleaseError, subprocess.CalledProcessError) as error:
        print(f"release error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
