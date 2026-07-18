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
import tomllib
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any, Callable

SEMVER_RE = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
CORE_MANIFEST = "Cargo.toml"
LOCKSTEP_LOCKFILES = (
    "tests/godot-compat-min/Cargo.lock",
    "tests/godot-web-smoke/Cargo.lock",
)
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
    ".llm/skills/crate-publishing/SKILL.md",
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
    """Classify a strict SemVer increase by its highest changed component."""
    base_parts = parse_version(base)
    target_parts = parse_version(target)
    if target_parts <= base_parts:
        raise ReleaseError(f"{base} to {target} is not a version increase")
    if target_parts[0] > base_parts[0]:
        return "major"
    if target_parts[1] > base_parts[1]:
        return "minor"
    return "patch"


def read_toml(path: Path) -> dict[str, Any]:
    with path.open("rb") as stream:
        return tomllib.load(stream)


def workspace_version(root: Path) -> str:
    value = (
        read_toml(root / CORE_MANIFEST)
        .get("workspace", {})
        .get("package", {})
        .get("version")
    )
    if not isinstance(value, str):
        raise ReleaseError("Cargo.toml has no [workspace.package] version")
    parse_version(value)
    return value


def manifest_package_version(path: Path, inherited_version: str | None = None) -> str:
    value = read_toml(path).get("package", {}).get("version")
    if isinstance(value, str):
        parse_version(value)
        return value
    if value == {"workspace": True} and inherited_version is not None:
        return inherited_version
    raise ReleaseError(f"{path} must inherit its package version from the workspace")


def package_version(root: Path) -> str:
    return workspace_version(root)


def replace_workspace_version(path: Path, old: str, new: str) -> None:
    cargo = path.read_text(encoding="utf-8")
    package = re.search(
        r"^\[workspace\.package\][ \t]*$\n(.*?)(?=^\[|\Z)",
        cargo,
        re.MULTILINE | re.DOTALL,
    )
    if package is None:
        raise ReleaseError("Cargo.toml has no [workspace.package] section")
    updated, count = re.subn(
        rf'^version = "{re.escape(old)}"$',
        f'version = "{new}"',
        package.group(1),
        count=1,
        flags=re.MULTILINE,
    )
    if count != 1:
        raise ReleaseError(
            f"Cargo.toml [workspace.package] does not contain version {old!r}"
        )
    path.write_text(
        cargo[: package.start(1)] + updated + cargo[package.end(1) :], encoding="utf-8"
    )


def replace_workspace_requirements(
    path: Path, dependency_keys: list[str], old: str, new: str
) -> None:
    cargo = path.read_text(encoding="utf-8")
    workspace_dependencies = re.search(
        r"^\[workspace\.dependencies\][ \t]*$\n(.*?)(?=^\[|\Z)",
        cargo,
        re.MULTILINE | re.DOTALL,
    )
    if workspace_dependencies is None:
        raise ReleaseError("Cargo.toml has no [workspace.dependencies] section")
    updated = workspace_dependencies.group(1)
    for key in dependency_keys:
        pattern = re.compile(
            rf'^({re.escape(key)}\s*=\s*\{{[^\n]*version\s*=\s*")='
            rf'{re.escape(old)}("[^\n]*\}})$',
            re.MULTILINE,
        )
        updated, count = pattern.subn(rf"\g<1>={new}\2", updated, count=1)
        if count != 1:
            raise ReleaseError(
                f"Cargo.toml has no exact workspace requirement for {key} ={old}"
            )
    path.write_text(
        cargo[: workspace_dependencies.start(1)]
        + updated
        + cargo[workspace_dependencies.end(1) :],
        encoding="utf-8",
    )


def replace_lockstep_package_versions(
    path: Path, package_names: list[str], old: str, new: str
) -> None:
    lock = path.read_text(encoding="utf-8")
    for package in package_names:
        pattern = re.compile(
            rf'(^\[\[package\]\]\nname = "{re.escape(package)}"\nversion = ")'
            rf'{re.escape(old)}("$)',
            re.MULTILINE,
        )
        lock, count = pattern.subn(rf"\g<1>{new}\2", lock, count=1)
        if count != 1:
            raise ReleaseError(f"{path} has no unique locked {package} {old}")
    path.write_text(lock, encoding="utf-8")


def release_heading(version: str) -> re.Pattern[str]:
    parse_version(version)
    return re.compile(
        rf"^## \[{re.escape(version)}\](?: - [0-9]{{4}}-[0-9]{{2}}-[0-9]{{2}})?[ \t]*$",
        re.MULTILINE,
    )


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


def cut_changelog(
    path: Path, old: str, new: str, date: str, breaking: bool = False
) -> None:
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
    if release_heading(new).search(text) is not None:
        raise ReleaseError(f"CHANGELOG.md already contains release {new}")

    policy_marker = "\n<!-- semver-checks: major -->\n" if breaking else ""
    body = (
        text[:start]
        + f"{heading}\n\n## [{new}] - {date}\n{policy_marker}\n{unreleased}\n"
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


def cargo_metadata(root: Path) -> dict[str, Any]:
    result = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
    )
    value = json.loads(result.stdout)
    if not isinstance(value, dict):
        raise ReleaseError("cargo metadata did not return an object")
    return value


def manifest_dependency_spec(
    manifest: dict[str, Any], dependency: dict[str, Any]
) -> Any:
    kind = dependency.get("kind")
    section = "build-dependencies" if kind == "build" else "dependencies"
    table: Any = manifest
    target = dependency.get("target")
    if target is not None:
        if not isinstance(target, str):
            return None
        table = manifest.get("target", {}).get(target, {})
    if not isinstance(table, dict):
        return None
    dependencies = table.get(section)
    if not isinstance(dependencies, dict):
        return None
    key = dependency.get("rename") or dependency.get("name")
    return dependencies.get(key) if isinstance(key, str) else None


def workspace_plan(
    root: Path, metadata: dict[str, Any] | None = None
) -> dict[str, Any]:
    """Discover publishable crates and order them dependency-first."""
    root = root.resolve()
    metadata = cargo_metadata(root) if metadata is None else metadata
    members = set(metadata.get("workspace_members", []))
    raw_packages = metadata.get("packages")
    if not isinstance(raw_packages, list):
        raise ReleaseError("cargo metadata has no package list")

    workspace_packages: dict[str, dict[str, Any]] = {}
    package_manifests: dict[str, dict[str, Any]] = {}
    publishable: dict[str, dict[str, Any]] = {}
    expected_version = workspace_version(root)
    for package in raw_packages:
        if not isinstance(package, dict) or package.get("id") not in members:
            continue
        name = package.get("name")
        manifest_value = package.get("manifest_path")
        if not isinstance(name, str) or not isinstance(manifest_value, str):
            raise ReleaseError("cargo metadata returned an invalid workspace package")
        if name in workspace_packages:
            raise ReleaseError(f"duplicate workspace package name {name}")
        workspace_packages[name] = package
        publish = package.get("publish")
        if publish is not None and (
            not isinstance(publish, list) or "crates-io" not in publish
        ):
            continue
        if package.get("version") != expected_version:
            raise ReleaseError(
                f"publishable package {name} must use workspace version {expected_version}"
            )
        manifest = Path(manifest_value).resolve()
        try:
            manifest.relative_to(root)
        except ValueError as error:
            raise ReleaseError(
                f"workspace package {name} is outside the workspace"
            ) from error
        manifest_data = read_toml(manifest)
        package_manifests[name] = manifest_data
        if manifest_data.get("package", {}).get("version") != {"workspace": True}:
            raise ReleaseError(
                f"publishable package {name} must set version.workspace = true"
            )
        publishable[name] = package

    if not publishable:
        raise ReleaseError("workspace has no crates publishable to crates.io")

    dependencies: dict[str, set[str]] = {name: set() for name in publishable}
    workspace_requirements: set[tuple[str, str]] = set()
    for name, package in publishable.items():
        raw_dependencies = package.get("dependencies", [])
        if not isinstance(raw_dependencies, list):
            raise ReleaseError(
                f"cargo metadata returned invalid dependencies for {name}"
            )
        for dependency in raw_dependencies:
            if not isinstance(dependency, dict) or dependency.get("kind") == "dev":
                continue
            dependency_name = dependency.get("name")
            if (
                dependency.get("source") is not None
                or dependency_name not in workspace_packages
            ):
                continue
            if dependency_name not in publishable:
                raise ReleaseError(
                    f"publishable package {name} depends on non-publishable workspace package "
                    f"{dependency_name}"
                )
            expected_requirement = f"={expected_version}"
            if dependency.get("req") != expected_requirement:
                raise ReleaseError(
                    f"{name} must require workspace package {dependency_name} exactly at "
                    f"{expected_requirement}"
                )
            specification = manifest_dependency_spec(
                package_manifests[name], dependency
            )
            if (
                not isinstance(specification, dict)
                or specification.get("workspace") is not True
            ):
                raise ReleaseError(
                    f"{name} must inherit workspace package {dependency_name} with "
                    "workspace = true"
                )
            dependencies[name].add(dependency_name)
            dependency_key = dependency.get("rename") or dependency_name
            if not isinstance(dependency_key, str):
                raise ReleaseError(
                    f"cargo metadata returned an invalid dependency key for {name}"
                )
            workspace_requirements.add((dependency_key, dependency_name))

    ordered_names: list[str] = []
    remaining = {name: set(values) for name, values in dependencies.items()}
    while remaining:
        ready = sorted(name for name, values in remaining.items() if not values)
        if not ready:
            cycle = ", ".join(sorted(remaining))
            raise ReleaseError(f"publishable workspace dependency cycle: {cycle}")
        for name in ready:
            ordered_names.append(name)
            del remaining[name]
        for values in remaining.values():
            values.difference_update(ready)

    packages: list[dict[str, Any]] = []
    for name in ordered_names:
        manifest = Path(publishable[name]["manifest_path"]).resolve()
        packages.append(
            {
                "name": name,
                "version": expected_version,
                "manifest_path": manifest.relative_to(root).as_posix(),
                "artifact": f"{name}-{expected_version}.crate",
                "dependencies": sorted(dependencies[name]),
            }
        )
    return {
        "version": expected_version,
        "packages": packages,
        "workspace_requirements": [
            {"key": key, "package": package}
            for key, package in sorted(workspace_requirements)
        ],
    }


def registry_plan(
    plan: dict[str, Any],
    artifacts_dir: Path,
    checksum_fetcher: Callable[[str, str], str | None] | None = None,
) -> dict[str, Any]:
    """Classify an interrupted release without ever overwriting registry state."""
    if checksum_fetcher is None:
        checksum_fetcher = registry_checksum
    states: dict[str, str] = {}
    packages: list[dict[str, Any]] = []
    for package in plan["packages"]:
        name = package["name"]
        version = package["version"]
        artifact = artifacts_dir / package["artifact"]
        local_checksum = verify_artifact(artifact, None)
        remote_checksum = checksum_fetcher(name, version)
        if remote_checksum is None:
            state = "unpublished"
        elif remote_checksum.lower() == local_checksum:
            state = "published-matching"
        else:
            raise ReleaseError(
                f"published {name} {version} checksum does not match the local artifact"
            )
        if state == "published-matching":
            incomplete = [
                dependency
                for dependency in package["dependencies"]
                if states.get(dependency) != "published-matching"
            ]
            if incomplete:
                raise ReleaseError(
                    f"published {name} {version} has unpublished workspace dependencies: "
                    + ", ".join(incomplete)
                )
        states[name] = state
        packages.append(
            {
                **package,
                "checksum": local_checksum,
                "registry_checksum": remote_checksum,
                "state": state,
            }
        )
    return {
        "version": plan["version"],
        "packages": packages,
        "pending": [
            package["name"] for package in packages if package["state"] == "unpublished"
        ],
    }


def prepare(
    root: Path,
    level: str,
    date: str,
    allow_dirty: bool = False,
    breaking: bool = False,
) -> str:
    if re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", date) is None:
        raise ReleaseError(f"invalid date {date!r}; expected YYYY-MM-DD")
    dt.date.fromisoformat(date)
    if not allow_dirty:
        require_clean(root)
    plan = workspace_plan(root)
    old = plan["version"]
    package_names = [package["name"] for package in plan["packages"]]
    required_workspace_keys = [
        requirement["key"] for requirement in plan["workspace_requirements"]
    ]
    new = bump_version(old, level)
    if breaking:
        policy = release_type(old, new)
        old_major = parse_version(old)[0]
        if policy != "major" and not (old_major == 0 and policy == "minor"):
            raise ReleaseError(
                "breaking releases require a major bump or a pre-1.0 minor bump"
            )

    # Validate every required source before writing any file. A stale inventory
    # must not leave a plausible-looking partial release bump behind.
    for relative in VERSION_FILES:
        if old not in (root / relative).read_text(encoding="utf-8"):
            raise ReleaseError(f"{relative} does not contain required value {old!r}")
    for relative in LOCKSTEP_LOCKFILES:
        lock = (root / relative).read_text(encoding="utf-8")
        for package in package_names:
            marker = f'name = "{package}"\nversion = "{old}"'
            if lock.count(marker) != 1:
                raise ReleaseError(f"{relative} has no unique locked {package} {old}")
    compatibility_text = (root / "tests/compatibility.toml").read_text(encoding="utf-8")
    if (
        len(re.findall(r'^client_version = "[^"]+"$', compatibility_text, re.MULTILINE))
        != 1
    ):
        raise ReleaseError("tests/compatibility.toml has no unique client_version")
    for relative in PROVENANCE_FILES:
        provenance = (root / relative).read_text(encoding="utf-8")
        if (
            len(
                re.findall(
                    r'^synced = "[0-9]{4}-[0-9]{2}-[0-9]{2}"$', provenance, re.MULTILINE
                )
            )
            != 1
        ):
            raise ReleaseError(f"{relative} has no unique synced date")
    changelog_text = (root / "CHANGELOG.md").read_text(encoding="utf-8")
    unreleased_start = changelog_text.find("## [Unreleased]")
    next_release = changelog_text.find(
        "\n## [", unreleased_start + len("## [Unreleased]")
    )
    if unreleased_start < 0 or next_release < 0:
        raise ReleaseError("CHANGELOG.md has no complete [Unreleased] section")
    if not changelog_text[
        unreleased_start + len("## [Unreleased]") : next_release
    ].strip():
        raise ReleaseError("CHANGELOG.md [Unreleased] section is empty")
    if release_heading(new).search(changelog_text) is not None:
        raise ReleaseError(f"CHANGELOG.md already contains release {new}")

    replace_workspace_version(root / CORE_MANIFEST, old, new)
    replace_workspace_requirements(
        root / CORE_MANIFEST, required_workspace_keys, old, new
    )
    for relative in LOCKSTEP_LOCKFILES:
        replace_lockstep_package_versions(root / relative, package_names, old, new)
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
    cut_changelog(root / "CHANGELOG.md", old, new, date, breaking)
    return new


def semver_policy(root: Path, version: str) -> str:
    previous = previous_version(root, version)
    policy = release_type(previous, version)
    changelog = (root / "CHANGELOG.md").read_text(encoding="utf-8")
    heading = release_heading(version).search(changelog)
    if heading is None:
        raise ReleaseError(
            f"CHANGELOG.md has no complete release section for {version}"
        )
    start = heading.start()
    end = changelog.find("\n## [", heading.end())
    if end < 0:
        raise ReleaseError(
            f"CHANGELOG.md has no complete release section for {version}"
        )
    breaking = "<!-- semver-checks: major -->" in changelog[start:end]
    if not breaking:
        return policy
    previous_major = parse_version(previous)[0]
    if policy == "major" or (previous_major == 0 and policy == "minor"):
        return "major"
    raise ReleaseError("major semver policy is invalid for this version bump")


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
    prep.add_argument("--breaking", action="store_true")

    checksum = subparsers.add_parser("checksum")
    checksum.add_argument("crate", type=Path)
    checksum.add_argument("--expected")

    package = subparsers.add_parser("package-version")
    package.add_argument("--root", type=Path, default=Path.cwd())
    package.add_argument("--manifest", type=Path, default=Path(CORE_MANIFEST))

    workspace = subparsers.add_parser("workspace-plan")
    workspace.add_argument("--root", type=Path, default=Path.cwd())

    registry_state = subparsers.add_parser("registry-plan")
    registry_state.add_argument("--root", type=Path, default=Path.cwd())
    registry_state.add_argument("--artifacts-dir", type=Path, required=True)

    registry = subparsers.add_parser("registry-checksum")
    registry.add_argument("crate_name")
    registry.add_argument("version")

    previous = subparsers.add_parser("previous-version")
    previous.add_argument("version")
    previous.add_argument("--root", type=Path, default=Path.cwd())

    policy = subparsers.add_parser("release-type")
    policy.add_argument("base")
    policy.add_argument("target")

    semver = subparsers.add_parser("semver-policy")
    semver.add_argument("version")
    semver.add_argument("--root", type=Path, default=Path.cwd())

    args = parser.parse_args(argv)
    try:
        if args.command == "prepare":
            print(
                prepare(
                    args.root.resolve(),
                    args.bump,
                    args.date,
                    args.allow_dirty,
                    args.breaking,
                )
            )
        elif args.command == "checksum":
            print(verify_artifact(args.crate, args.expected))
        elif args.command == "package-version":
            root = args.root.resolve()
            print(
                manifest_package_version(root / args.manifest, workspace_version(root))
            )
        elif args.command == "workspace-plan":
            print(json.dumps(workspace_plan(args.root.resolve()), indent=2))
        elif args.command == "registry-plan":
            root = args.root.resolve()
            print(
                json.dumps(
                    registry_plan(workspace_plan(root), args.artifacts_dir.resolve()),
                    indent=2,
                )
            )
        elif args.command == "registry-checksum":
            value = registry_checksum(args.crate_name, args.version)
            print(value or "UNPUBLISHED")
        elif args.command == "previous-version":
            print(previous_version(args.root.resolve(), args.version))
        elif args.command == "release-type":
            print(release_type(args.base, args.target))
        else:
            print(semver_policy(args.root.resolve(), args.version))
    except (OSError, ValueError, ReleaseError, subprocess.CalledProcessError) as error:
        print(f"release error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
