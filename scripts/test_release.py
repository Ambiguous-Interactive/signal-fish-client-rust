#!/usr/bin/env python3
"""Tests for release.py."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SPEC = importlib.util.spec_from_file_location(
    "release", Path(__file__).with_name("release.py")
)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load scripts/release.py for testing")
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)


class VersionTests(unittest.TestCase):
    def test_bumps_reset_lower_components(self) -> None:
        self.assertEqual(release.bump_version("1.2.3", "major"), "2.0.0")
        self.assertEqual(release.bump_version("1.2.3", "minor"), "1.3.0")
        self.assertEqual(release.bump_version("1.2.3", "patch"), "1.2.4")

    def test_rejects_non_strict_versions(self) -> None:
        for value in ("v1.2.3", "1.2", "1.2.3-rc.1", "01.2.3"):
            with self.subTest(value=value), self.assertRaises(release.ReleaseError):
                release.parse_version(value)

    def test_package_version_is_scoped_to_workspace_package_section(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.toml").write_text(
                '[package]\nname = "demo"\nversion.workspace = true\n\n'
                '[dependencies]\ndemo = { version = "8.8.8" }\n'
                '\n[workspace]\n\n[workspace.package]\nversion = "1.2.3"\n',
                encoding="utf-8",
            )
            self.assertEqual(release.package_version(root), "1.2.3")
            release.replace_workspace_version(root / "Cargo.toml", "1.2.3", "1.2.4")
            cargo = (root / "Cargo.toml").read_text(encoding="utf-8")
            self.assertIn("version.workspace = true", cargo)
            self.assertIn('demo = { version = "8.8.8" }', cargo)
            self.assertEqual(release.package_version(root), "1.2.4")

    def test_release_type_classifies_strict_increases(self) -> None:
        self.assertEqual(release.release_type("1.2.3", "2.0.0"), "major")
        self.assertEqual(release.release_type("1.2.3", "1.3.0"), "minor")
        self.assertEqual(release.release_type("1.2.3", "1.2.4"), "patch")
        self.assertEqual(release.release_type("1.2.3", "3.4.5"), "major")
        self.assertEqual(release.release_type("1.2.3", "1.4.1"), "minor")
        self.assertEqual(release.release_type("1.2.3", "1.2.9"), "patch")
        self.assertEqual(release.release_type("0.6.0", "0.8.0"), "minor")
        for target in ("1.2.3", "1.2.2", "1.1.9", "0.9.9"):
            with self.subTest(target=target), self.assertRaises(release.ReleaseError):
                release.release_type("1.2.3", target)


class WorkspacePlanTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        (self.root / "Cargo.toml").write_text(
            '[workspace]\n\n[workspace.package]\nversion = "1.2.3"\n',
            encoding="utf-8",
        )

    def tearDown(self) -> None:
        self.temp.cleanup()

    def metadata(self, packages: list[dict[str, object]]) -> dict[str, object]:
        values = []
        for package in packages:
            name = str(package["name"])
            manifest = self.root / name / "Cargo.toml"
            manifest.parent.mkdir(exist_ok=True)
            dependency_lines = [
                f"{dependency.get('rename') or dependency['name']} = {{ workspace = true }}"
                for dependency in package.get("dependencies", [])
                if dependency.get("kind") != "dev"
            ]
            dependencies = (
                "\n[dependencies]\n" + "\n".join(dependency_lines) + "\n"
                if dependency_lines
                else ""
            )
            manifest.write_text(
                f'[package]\nname = "{name}"\nversion.workspace = true\n{dependencies}',
                encoding="utf-8",
            )
            values.append(
                {
                    "id": name,
                    "name": name,
                    "version": package.get("version", "1.2.3"),
                    "manifest_path": str(manifest),
                    "publish": package.get("publish", ["crates-io"]),
                    "dependencies": package.get("dependencies", []),
                }
            )
        return {
            "workspace_members": [value["id"] for value in values],
            "packages": values,
        }

    @staticmethod
    def dependency(name: str, requirement: str = "=1.2.3") -> dict[str, object]:
        return {"name": name, "req": requirement, "kind": None, "source": None}

    def test_discovers_publishable_crates_in_dependency_order(self) -> None:
        metadata = self.metadata(
            [
                {"name": "adapter", "dependencies": [self.dependency("core")]},
                {"name": "tool", "publish": []},
                {"name": "core"},
            ]
        )
        plan = release.workspace_plan(self.root, metadata)
        self.assertEqual(
            [package["name"] for package in plan["packages"]], ["core", "adapter"]
        )
        self.assertEqual(plan["packages"][1]["dependencies"], ["core"])

    def test_rejects_non_exact_internal_requirement(self) -> None:
        metadata = self.metadata(
            [
                {"name": "core"},
                {
                    "name": "adapter",
                    "dependencies": [self.dependency("core", "^1.2.3")],
                },
            ]
        )
        with self.assertRaisesRegex(release.ReleaseError, "exactly"):
            release.workspace_plan(self.root, metadata)

    def test_rejects_inline_exact_internal_requirement(self) -> None:
        metadata = self.metadata(
            [
                {"name": "core"},
                {"name": "adapter", "dependencies": [self.dependency("core")]},
            ]
        )
        (self.root / "adapter" / "Cargo.toml").write_text(
            '[package]\nname = "adapter"\nversion.workspace = true\n\n'
            '[dependencies]\ncore = { version = "=1.2.3", path = "../core" }\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(release.ReleaseError, "workspace = true"):
            release.workspace_plan(self.root, metadata)

    def test_accepts_renamed_target_workspace_dependency(self) -> None:
        dependency = self.dependency("core")
        dependency.update({"rename": "core_alias", "target": "cfg(unix)"})
        metadata = self.metadata(
            [
                {"name": "core"},
                {"name": "adapter", "dependencies": [dependency]},
            ]
        )
        (self.root / "adapter" / "Cargo.toml").write_text(
            '[package]\nname = "adapter"\nversion.workspace = true\n\n'
            "[target.'cfg(unix)'.dependencies]\ncore_alias = { workspace = true }\n",
            encoding="utf-8",
        )
        plan = release.workspace_plan(self.root, metadata)
        self.assertEqual(plan["packages"][1]["dependencies"], ["core"])
        self.assertEqual(
            plan["workspace_requirements"],
            [{"key": "core_alias", "package": "core"}],
        )

    def test_rejects_dependency_on_non_publishable_member(self) -> None:
        metadata = self.metadata(
            [
                {"name": "tool", "publish": []},
                {"name": "adapter", "dependencies": [self.dependency("tool")]},
            ]
        )
        with self.assertRaisesRegex(release.ReleaseError, "non-publishable"):
            release.workspace_plan(self.root, metadata)

    def test_rejects_publish_dependency_cycle(self) -> None:
        metadata = self.metadata(
            [
                {"name": "a", "dependencies": [self.dependency("b")]},
                {"name": "b", "dependencies": [self.dependency("a")]},
            ]
        )
        with self.assertRaisesRegex(release.ReleaseError, "cycle"):
            release.workspace_plan(self.root, metadata)


class PreparationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        (self.root / "Cargo.toml").write_text(
            '[package]\nname = "signal-fish-client"\nversion.workspace = true\n'
            'publish = ["crates-io"]\nedition = "2021"\n\n'
            '[workspace]\nmembers = ["crates/signal-fish-client-godot"]\nresolver = "2"\n\n'
            '[workspace.package]\nversion = "1.2.3"\n\n'
            "[workspace.dependencies]\n"
            'signal-fish-client = { version = "=1.2.3", path = "." }\n',
            encoding="utf-8",
        )
        (self.root / "src").mkdir()
        (self.root / "src/lib.rs").write_text("", encoding="utf-8")
        adapter = self.root / "crates/signal-fish-client-godot/Cargo.toml"
        adapter.parent.mkdir(parents=True, exist_ok=True)
        adapter.write_text(
            '[package]\nname = "signal-fish-client-godot"\nversion.workspace = true\n'
            'publish = ["crates-io"]\nedition = "2021"\n\n'
            "[dependencies]\nsignal-fish-client.workspace = true\n",
            encoding="utf-8",
        )
        (adapter.parent / "src").mkdir()
        (adapter.parent / "src/lib.rs").write_text("", encoding="utf-8")
        for relative in release.VERSION_FILES:
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("release 1.2.3\n", encoding="utf-8")
        for relative in release.PROVENANCE_FILES:
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(
                'client_version = "1.2.3"\nsynced = "2020-01-01"\n', encoding="utf-8"
            )
        for relative in release.LOCKSTEP_LOCKFILES:
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(
                'version = 4\n\n[[package]]\nname = "signal-fish-client"\n'
                'version = "1.2.3"\n\n[[package]]\n'
                'name = "signal-fish-client-godot"\nversion = "1.2.3"\n',
                encoding="utf-8",
            )
        (self.root / "CHANGELOG.md").write_text(
            "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- Good thing.\n\n"
            "## [1.2.3] - 2020-01-01\n\n- Old.\n\n"
            "[Unreleased]: https://example.test/compare/v1.2.3...HEAD\n",
            encoding="utf-8",
        )

    def tearDown(self) -> None:
        self.temp.cleanup()

    def test_prepare_updates_all_release_references(self) -> None:
        version = release.prepare(self.root, "minor", "2026-07-13", allow_dirty=True)
        self.assertEqual(version, "1.3.0")
        self.assertEqual(release.package_version(self.root), "1.3.0")
        cargo = (self.root / "Cargo.toml").read_text(encoding="utf-8")
        self.assertIn('version = "1.3.0"', cargo)
        self.assertIn('version = "=1.3.0"', cargo)
        for relative in release.LOCKSTEP_LOCKFILES:
            lock = (self.root / relative).read_text(encoding="utf-8")
            self.assertEqual(lock.count('version = "1.3.0"'), 2)
        changelog = (self.root / "CHANGELOG.md").read_text(encoding="utf-8")
        self.assertIn("## [Unreleased]\n\n## [1.3.0] - 2026-07-13", changelog)
        self.assertIn("compare/v1.2.3...v1.3.0", changelog)
        self.assertIn("compare/v1.3.0...HEAD", changelog)
        compatibility = (self.root / "tests/compatibility.toml").read_text(
            encoding="utf-8"
        )
        self.assertIn('client_version = "1.3.0"', compatibility)
        self.assertIn('synced = "2026-07-13"', compatibility)
        self.assertEqual(release.previous_version(self.root, "1.3.0"), "1.2.3")
        self.assertEqual(release.semver_policy(self.root, "1.3.0"), "minor")

    def test_prepare_updates_renamed_workspace_requirement(self) -> None:
        root_manifest = self.root / "Cargo.toml"
        root_manifest.write_text(
            root_manifest.read_text(encoding="utf-8").replace(
                'signal-fish-client = { version = "=1.2.3", path = "." }',
                'core_alias = { package = "signal-fish-client", '
                'version = "=1.2.3", path = "." }',
            ),
            encoding="utf-8",
        )
        adapter_manifest = self.root / "crates/signal-fish-client-godot/Cargo.toml"
        adapter_manifest.write_text(
            adapter_manifest.read_text(encoding="utf-8").replace(
                "signal-fish-client.workspace = true",
                "core_alias.workspace = true",
            ),
            encoding="utf-8",
        )

        version = release.prepare(self.root, "minor", "2026-07-13", allow_dirty=True)

        self.assertEqual(version, "1.3.0")
        cargo = root_manifest.read_text(encoding="utf-8")
        self.assertIn(
            'core_alias = { package = "signal-fish-client", '
            'version = "=1.3.0", path = "." }',
            cargo,
        )

    def test_pre_one_minor_can_persist_intentional_breaking_policy(self) -> None:
        for path in self.root.rglob("*"):
            if path.is_file():
                path.write_text(
                    path.read_text(encoding="utf-8").replace("1.2.3", "0.7.0"),
                    encoding="utf-8",
                )
        version = release.prepare(
            self.root,
            "minor",
            "2026-07-13",
            allow_dirty=True,
            breaking=True,
        )
        self.assertEqual(version, "0.8.0")
        self.assertEqual(release.semver_policy(self.root, version), "major")
        changelog = (self.root / "CHANGELOG.md").read_text(encoding="utf-8")
        self.assertIn("<!-- semver-checks: major -->", changelog)

    def test_breaking_patch_is_rejected_before_writes(self) -> None:
        with self.assertRaisesRegex(release.ReleaseError, "breaking releases"):
            release.prepare(
                self.root,
                "patch",
                "2026-07-13",
                allow_dirty=True,
                breaking=True,
            )
        self.assertEqual(release.package_version(self.root), "1.2.3")

    def test_empty_unreleased_section_fails_closed(self) -> None:
        (self.root / "CHANGELOG.md").write_text(
            "# Changelog\n\n## [Unreleased]\n\n## [1.2.3] - 2020-01-01\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(release.ReleaseError, "empty"):
            release.prepare(self.root, "patch", "2026-07-13", allow_dirty=True)

    def test_missing_reference_does_not_partially_update(self) -> None:
        missing = self.root / release.VERSION_FILES[-1]
        missing.write_text("stale\n", encoding="utf-8")
        with self.assertRaisesRegex(release.ReleaseError, "required value"):
            release.prepare(self.root, "patch", "2026-07-13", allow_dirty=True)
        self.assertEqual(release.package_version(self.root), "1.2.3")

    def test_explicit_member_version_fails_before_writes(self) -> None:
        adapter = self.root / "crates/signal-fish-client-godot/Cargo.toml"
        adapter.write_text(
            adapter.read_text(encoding="utf-8").replace(
                "version.workspace = true", 'version = "1.2.3"', 1
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(release.ReleaseError, "version.workspace"):
            release.prepare(self.root, "minor", "2026-07-13", allow_dirty=True)
        self.assertEqual(release.package_version(self.root), "1.2.3")

    def test_date_must_use_canonical_iso_form(self) -> None:
        with self.assertRaisesRegex(release.ReleaseError, "YYYY-MM-DD"):
            release.prepare(self.root, "patch", "20260713", allow_dirty=True)

    def test_duplicate_release_fails_before_writes(self) -> None:
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text(
            changelog.read_text(encoding="utf-8") + "\n## [1.2.4] - 2026-01-01\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(release.ReleaseError, "already contains"):
            release.prepare(self.root, "patch", "2026-07-13", allow_dirty=True)
        self.assertEqual(release.package_version(self.root), "1.2.3")

    def test_release_heading_does_not_prefix_match(self) -> None:
        changelog = self.root / "CHANGELOG.md"
        text = changelog.read_text(encoding="utf-8").replace(
            "## [1.2.3]", "## [1.2.30]"
        )
        changelog.write_text(text, encoding="utf-8")
        self.assertIsNone(release.release_heading("1.2.3").search(text))

    @mock.patch.object(release.subprocess, "run")
    def test_dirty_worktree_is_rejected(self, run: mock.Mock) -> None:
        run.return_value = mock.Mock(stdout=" M Cargo.toml\n")
        with self.assertRaisesRegex(release.ReleaseError, "clean"):
            release.prepare(self.root, "patch", "2026-07-13")
        self.assertEqual(release.package_version(self.root), "1.2.3")


class ArtifactTests(unittest.TestCase):
    def test_checksum_recovery_requires_exact_match(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            artifact = Path(directory) / "crate"
            artifact.write_bytes(b"package")
            checksum = release.sha256(artifact)
            self.assertEqual(release.verify_artifact(artifact, checksum), checksum)
            with self.assertRaisesRegex(release.ReleaseError, "mismatch"):
                release.verify_artifact(artifact, "0" * 64)

    @mock.patch.object(release.urllib.request, "urlopen")
    def test_registry_404_means_unpublished(self, urlopen: mock.Mock) -> None:
        urlopen.side_effect = release.urllib.error.HTTPError(
            "url", 404, "missing", {}, None
        )
        self.assertIsNone(release.registry_checksum("demo", "1.2.3"))

    def test_registry_plan_state_matrix_and_publish_order(self) -> None:
        plan = {
            "version": "1.2.3",
            "packages": [
                {
                    "name": "core",
                    "version": "1.2.3",
                    "manifest_path": "Cargo.toml",
                    "artifact": "core-1.2.3.crate",
                    "dependencies": [],
                },
                {
                    "name": "adapter",
                    "version": "1.2.3",
                    "manifest_path": "adapter/Cargo.toml",
                    "artifact": "adapter-1.2.3.crate",
                    "dependencies": ["core"],
                },
            ],
        }
        with tempfile.TemporaryDirectory() as directory:
            artifacts = Path(directory)
            (artifacts / "core-1.2.3.crate").write_bytes(b"core")
            (artifacts / "adapter-1.2.3.crate").write_bytes(b"adapter")
            checksums = {
                "core": release.sha256(artifacts / "core-1.2.3.crate"),
                "adapter": release.sha256(artifacts / "adapter-1.2.3.crate"),
            }
            cases = (
                ({}, ["core", "adapter"]),
                ({"core": checksums["core"]}, ["adapter"]),
                (checksums, []),
            )
            for published, expected in cases:
                with self.subTest(published=published):
                    state = release.registry_plan(
                        plan, artifacts, lambda name, _version: published.get(name)
                    )
                    self.assertEqual(state["pending"], expected)

            with self.assertRaisesRegex(release.ReleaseError, "unpublished workspace"):
                release.registry_plan(
                    plan,
                    artifacts,
                    lambda name, _version: (
                        checksums["adapter"] if name == "adapter" else None
                    ),
                )
            with self.assertRaisesRegex(release.ReleaseError, "does not match"):
                release.registry_plan(
                    plan,
                    artifacts,
                    lambda name, _version: "0" * 64 if name == "core" else None,
                )


class WorkflowPolicyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        root = Path(__file__).resolve().parents[1]
        cls.prepare = (root / ".github/workflows/prepare-release.yml").read_text(
            encoding="utf-8"
        )
        cls.publish = (root / ".github/workflows/publish.yml").read_text(
            encoding="utf-8"
        )
        cls.ci = (root / ".github/workflows/ci.yml").read_text(encoding="utf-8")

    def test_prepare_uses_app_token_and_supports_dry_run(self) -> None:
        self.assertIn("actions/create-github-app-token@v3.2.0", self.prepare)
        self.assertIn("RELEASE_APP_CLIENT_ID is not configured", self.prepare)
        self.assertIn("RELEASE_APP_PRIVATE_KEY", self.prepare)
        self.assertIn("dry_run:", self.prepare)
        self.assertIn("branch=release/%s", self.prepare)
        self.assertIn("gh pr create", self.prepare)

    def test_publish_is_input_free_manual_only_and_protected(self) -> None:
        self.assertIn("workflow_dispatch:", self.publish)
        dispatch = self.publish.split("workflow_dispatch:", 1)[1].split(
            "permissions:", 1
        )[0]
        self.assertNotIn("inputs:", dispatch)
        self.assertNotIn("push:\n", self.publish)
        self.assertIn("environment: crates-io", self.publish)
        self.assertIn("cancel-in-progress: false", self.publish)
        self.assertIn("checks: read", self.publish)
        self.assertIn("Require the default branch", self.publish)

    def test_publish_has_fail_closed_recovery_and_assets(self) -> None:
        for marker in (
            "Existing tag",
            "registry-plan",
            "SHA256SUMS",
            "cargo cyclonedx",
            "actions/attest@v4.2.0",
            "cargo publish --dry-run",
            "cargo publish",
            "gh release create",
        ):
            with self.subTest(marker=marker):
                self.assertIn(marker, self.publish)

    def test_semver_policy_is_derived_and_check_runs_are_latest_only(self) -> None:
        self.assertIn('semver-policy "$version"', self.publish)
        self.assertIn('--release-type "$RELEASE_TYPE"', self.publish)
        self.assertIn('if [ "$BREAKING" = true ]', self.prepare)
        self.assertIn("chore!: prepare release", self.prepare)
        self.assertEqual(self.publish.count("check-runs?filter=latest"), 1)
        self.assertIn("scripts/check-required-checks.py", self.publish)
        self.assertIn("Expected one CycloneDX file", self.publish)
        self.assertIn("$RUNNER_TEMP/release-assets", self.publish)
        self.assertIn("Release tooling dirtied the checkout", self.publish)
        self.assertIn("Release publication", self.publish)
        self.assertIn("fetch-tags: true", self.publish)
        self.assertIn("scripts/release.py workspace-plan", self.publish)

    def test_workflows_enumerate_publishable_workspace_crates(self) -> None:
        for marker in (
            "workspace-plan",
            "mapfile -t packages",
            'package_args+=(-p "$package")',
        ):
            with self.subTest(marker=marker):
                self.assertIn(marker, self.publish)
                self.assertIn(marker, self.prepare)
        self.assertIn("workspace-plan", self.ci)

    def test_release_toolchain_and_runner_are_pinned(self) -> None:
        for workflow in (self.prepare, self.publish):
            with self.subTest(workflow=workflow[:40]):
                self.assertIn('RELEASE_RUST: "1.96.1"', workflow)
                self.assertIn("runs-on: ubuntu-24.04", workflow)
                self.assertIn("toolchain: ${{ env.RELEASE_RUST }}", workflow)


if __name__ == "__main__":
    unittest.main()
