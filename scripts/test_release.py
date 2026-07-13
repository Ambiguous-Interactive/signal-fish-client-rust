#!/usr/bin/env python3
"""Tests for release.py."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SPEC = importlib.util.spec_from_file_location("release", Path(__file__).with_name("release.py"))
assert SPEC is not None and SPEC.loader is not None
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

    def test_release_type_requires_one_exact_component_bump(self) -> None:
        self.assertEqual(release.release_type("1.2.3", "2.0.0"), "major")
        self.assertEqual(release.release_type("1.2.3", "1.3.0"), "minor")
        self.assertEqual(release.release_type("1.2.3", "1.2.4"), "patch")
        for target in ("1.4.0", "1.3.1", "1.2.3", "1.2.2"):
            with self.subTest(target=target), self.assertRaises(release.ReleaseError):
                release.release_type("1.2.3", target)


class PreparationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        (self.root / "Cargo.toml").write_text('[package]\nversion = "1.2.3"\n', encoding="utf-8")
        for relative in release.VERSION_FILES:
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("release 1.2.3\n", encoding="utf-8")
        for relative in release.PROVENANCE_FILES:
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('client_version = "1.2.3"\nsynced = "2020-01-01"\n', encoding="utf-8")
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
        changelog = (self.root / "CHANGELOG.md").read_text(encoding="utf-8")
        self.assertIn("## [Unreleased]\n\n## [1.3.0] - 2026-07-13", changelog)
        self.assertIn("compare/v1.2.3...v1.3.0", changelog)
        self.assertIn("compare/v1.3.0...HEAD", changelog)
        compatibility = (self.root / "tests/compatibility.toml").read_text(encoding="utf-8")
        self.assertIn('client_version = "1.3.0"', compatibility)
        self.assertIn('synced = "2026-07-13"', compatibility)
        self.assertEqual(release.previous_version(self.root, "1.3.0"), "1.2.3")

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
        urlopen.side_effect = release.urllib.error.HTTPError("url", 404, "missing", {}, None)
        self.assertIsNone(release.registry_checksum("demo", "1.2.3"))


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

    def test_prepare_uses_app_token_and_supports_dry_run(self) -> None:
        self.assertIn("actions/create-github-app-token@v3.2.0", self.prepare)
        self.assertIn("RELEASE_APP_PRIVATE_KEY", self.prepare)
        self.assertIn("dry_run:", self.prepare)
        self.assertIn('branch=release/%s', self.prepare)
        self.assertIn("gh pr create", self.prepare)

    def test_publish_is_manual_only_and_protected(self) -> None:
        self.assertIn("workflow_dispatch:", self.publish)
        self.assertNotIn("push:\n", self.publish)
        self.assertIn("environment: crates-io", self.publish)
        self.assertIn("cancel-in-progress: false", self.publish)
        self.assertIn("expected strict X.Y.Z", self.publish)

    def test_publish_has_fail_closed_recovery_and_assets(self) -> None:
        for marker in (
            "Existing tag",
            "Published crate checksum",
            "registry-checksum",
            "SHA256SUMS",
            "cargo cyclonedx",
            "actions/attest@v4.1.1",
            "cargo publish --dry-run",
            "cargo publish",
            "gh release create",
        ):
            with self.subTest(marker=marker):
                self.assertIn(marker, self.publish)

    def test_semver_policy_is_derived_and_check_runs_are_latest_only(self) -> None:
        self.assertIn('release-type "$previous" "$VERSION"', self.publish)
        self.assertIn('--release-type "$RELEASE_TYPE"', self.publish)
        self.assertNotIn("chore!: prepare release", self.prepare)
        self.assertEqual(self.publish.count("check-runs?filter=latest"), 2)


if __name__ == "__main__":
    unittest.main()
