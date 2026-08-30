#!/usr/bin/env python3
"""Tests for release.py."""

from __future__ import annotations

import importlib.util
import json
import re
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
        (self.root / "tests").mkdir()
        (self.root / "tests/compatibility.toml").write_text(
            'client_version = "1.2.3"\nsynced = "2020-01-01"\n\n'
            '[protocol_authority]\ncommit = "abc"\nsynced = "2020-01-02"\n',
            encoding="utf-8",
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
        # Partial locks record only the members their graph reaches (the
        # fuzz graph never contains the Godot adapter).
        for relative in release.PARTIAL_LOCKFILES:
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(
                'version = 4\n\n[[package]]\nname = "signal-fish-client"\n'
                'version = "1.2.3"\n',
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
        for relative in release.PARTIAL_LOCKFILES:
            lock = (self.root / relative).read_text(encoding="utf-8")
            # The fuzz graph contains only the core; the adapter must stay
            # absent, and the core's recorded version must move in lockstep.
            self.assertEqual(lock.count('version = "1.3.0"'), 1)
            self.assertNotIn("signal-fish-client-godot", lock)
        changelog = (self.root / "CHANGELOG.md").read_text(encoding="utf-8")
        self.assertIn("## [Unreleased]\n\n## [1.3.0] - 2026-07-13", changelog)
        self.assertIn("compare/v1.2.3...v1.3.0", changelog)
        self.assertIn("compare/v1.3.0...HEAD", changelog)
        compatibility = (self.root / "tests/compatibility.toml").read_text(
            encoding="utf-8"
        )
        self.assertIn('client_version = "1.3.0"', compatibility)
        # Only the top-level release-sync date is stamped; upstream provenance
        # dates ([protocol_authority] and vendored PROVENANCE.toml files) are
        # never rewritten by a release.
        self.assertEqual(compatibility.count('synced = "2026-07-13"'), 1)
        self.assertEqual(compatibility.count('synced = "2020-01-02"'), 1)
        self.assertEqual(release.previous_version(self.root, "1.3.0"), "1.2.3")
        self.assertEqual(release.semver_policy(self.root, "1.3.0"), "minor")

    def test_prepare_requires_top_level_identity_in_header_table(self) -> None:
        path = self.root / "tests/compatibility.toml"
        cases = {
            "duplicate top-level synced": (
                'client_version = "1.2.3"\nsynced = "2020-01-01"\n'
                'synced = "2020-01-05"\n\n'
                '[protocol_authority]\ncommit = "abc"\nsynced = "2020-01-02"\n'
            ),
            "client_version only in a section": (
                'synced = "2020-01-01"\n\n'
                '[protocol_authority]\ncommit = "abc"\n'
                'client_version = "1.2.3"\nsynced = "2020-01-02"\n'
            ),
        }
        for name, content in cases.items():
            with self.subTest(case=name):
                path.write_text(content, encoding="utf-8")
                with self.assertRaisesRegex(
                    release.ReleaseError, "top-level"
                ):
                    release.prepare(
                        self.root, "minor", "2026-07-13", allow_dirty=True
                    )
                self.assertEqual(release.package_version(self.root), "1.2.3")

    def test_release_intent_derives_minor_from_added_entries(self) -> None:
        intent = release.release_intent(self.root)

        self.assertEqual(
            intent,
            {
                "current": "1.2.3",
                "target": "1.3.0",
                "bump": "minor",
                "breaking": False,
                "semver_policy": "minor",
                "categories": ["Added"],
            },
        )

    def test_release_intent_derives_patch_from_fix_only_entries(self) -> None:
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text(
            changelog.read_text(encoding="utf-8").replace("### Added", "### Fixed"),
            encoding="utf-8",
        )

        intent = release.release_intent(self.root)

        self.assertEqual(intent["target"], "1.2.4")
        self.assertEqual(intent["bump"], "patch")
        self.assertEqual(intent["semver_policy"], "patch")

    def test_release_intent_derives_pre_one_breaking_minor(self) -> None:
        for path in self.root.rglob("*"):
            if path.is_file():
                path.write_text(
                    path.read_text(encoding="utf-8").replace("1.2.3", "0.7.0"),
                    encoding="utf-8",
                )
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text(
            changelog.read_text(encoding="utf-8").replace(
                "- Good thing.", "- **Breaking:** Good thing."
            ),
            encoding="utf-8",
        )

        intent = release.release_intent(self.root)

        self.assertEqual(intent["target"], "0.8.0")
        self.assertEqual(intent["bump"], "minor")
        self.assertTrue(intent["breaking"])
        self.assertEqual(intent["semver_policy"], "major")

    def test_release_intent_rejects_unknown_changelog_categories(self) -> None:
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text(
            changelog.read_text(encoding="utf-8").replace("### Added", "### Surprise"),
            encoding="utf-8",
        )

        with self.assertRaisesRegex(release.ReleaseError, "unsupported.*Surprise"):
            release.release_intent(self.root)

    def test_release_intent_rejects_empty_changelog_categories(self) -> None:
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text(
            changelog.read_text(encoding="utf-8").replace(
                "## [1.2.3]", "### Fixed\n\n## [1.2.3]"
            ),
            encoding="utf-8",
        )

        with self.assertRaisesRegex(release.ReleaseError, "empty.*Fixed"):
            release.release_intent(self.root)

    def test_release_intent_ignores_flush_left_fenced_changelog_syntax(self) -> None:
        # The regression this guards: pre-fix parsing read fenced headings and
        # bullets as release intent, silently inflating the derived bump.
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text(
            "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- Good thing.\n\n"
            "```text\n### Fixed\n\n- **Breaking:** fenced example text.\n```\n\n"
            "## [1.2.3] - 2020-01-01\n\n- Old.\n",
            encoding="utf-8",
        )

        intent = release.release_intent(self.root)

        self.assertEqual(intent["bump"], "minor")
        self.assertFalse(intent["breaking"])
        self.assertEqual(intent["categories"], ["Added"])

    def test_release_intent_ignores_indented_list_item_fences(self) -> None:
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text(
            changelog.read_text(encoding="utf-8").replace(
                "- Good thing.",
                "- Good thing.\n\n"
                "  ```markdown\n"
                "  ### Surprise\n\n"
                "  - **Breaking:** fenced snippet text.\n"
                "  ```",
            ),
            encoding="utf-8",
        )

        intent = release.release_intent(self.root)

        self.assertEqual(intent["bump"], "minor")
        self.assertFalse(intent["breaking"])
        self.assertEqual(intent["categories"], ["Added"])

    def test_release_intent_keeps_bullets_after_commonmark_nested_fence(self) -> None:
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text(
            "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- Good thing.\n\n"
            "````markdown\n### Fixed\n\n- Innocent sample.\n\n```rust\nlet x = 1;\n```\n"
            "````\n\n- Fence-adjacent follow-up.\n\n"
            "## [1.2.3] - 2020-01-01\n\n- Old.\n",
            encoding="utf-8",
        )

        intent = release.release_intent(self.root)

        self.assertFalse(intent["breaking"])
        self.assertEqual(intent["bump"], "minor")
        self.assertEqual(intent["semver_policy"], "minor")
        self.assertEqual(intent["categories"], ["Added"])

    def test_release_intent_ignores_tilde_fenced_changelog_syntax(self) -> None:
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text(
            "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- Good thing.\n\n"
            "~~~\n### Fixed\n\n- **Breaking:** fenced example text.\n~~~\n\n"
            "## [1.2.3] - 2020-01-01\n\n- Old.\n",
            encoding="utf-8",
        )

        intent = release.release_intent(self.root)

        self.assertEqual(intent["bump"], "minor")
        self.assertFalse(intent["breaking"])
        self.assertEqual(intent["categories"], ["Added"])

    def test_release_intent_ignores_fenced_version_heading_example(self) -> None:
        # A fenced example whose quoted changelog heading sits at line start
        # previously truncated the [Unreleased] section mid-fence (the naive
        # `\n## [` boundary search matched it), failing downstream as an
        # unterminated fence and bricking Prepare Release.
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text(
            "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- Good thing.\n\n"
            "```markdown\n"
            "## [1.2.4] - 2026-01-01\n\n- Sample entry inside the example.\n"
            "```\n\n"
            "- Follow-up entry after the fenced example.\n\n"
            "## [1.2.3] - 2020-01-01\n\n- Old.\n",
            encoding="utf-8",
        )

        intent = release.release_intent(self.root)

        self.assertEqual(intent["bump"], "minor")
        self.assertFalse(intent["breaking"])
        self.assertEqual(intent["categories"], ["Added"])

    def test_release_intent_still_fails_closed_when_fence_bleeds_across_sections(
        self,
    ) -> None:
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text(
            "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- Good thing.\n\n"
            "```markdown\n- example never closed here\n\n"
            "## [1.2.3] - 2020-01-01\n\n- Old.\n",
            encoding="utf-8",
        )

        with self.assertRaisesRegex(release.ReleaseError, "unterminated fenced"):
            release.release_intent(self.root)

    def test_release_intent_rejects_unterminated_fence(self) -> None:
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text(
            changelog.read_text(encoding="utf-8").replace(
                "- Good thing.",
                "- Good thing.\n\n```markdown\n- **Breaking:** never closed.",
            ),
            encoding="utf-8",
        )

        with self.assertRaisesRegex(release.ReleaseError, "unterminated fenced"):
            release.release_intent(self.root)

    def test_release_intent_treats_fence_only_unreleased_as_empty(self) -> None:
        (self.root / "CHANGELOG.md").write_text(
            "# Changelog\n\n## [Unreleased]\n\n```text\n- Looks like an entry.\n```\n\n"
            "## [1.2.3] - 2020-01-01\n\n- Old.\n",
            encoding="utf-8",
        )

        with self.assertRaisesRegex(release.ReleaseError, "section is empty"):
            release.release_intent(self.root)

    def test_current_semver_policy_prefers_unreleased_intent(self) -> None:
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text(
            changelog.read_text(encoding="utf-8").replace(
                "- Good thing.", "- **Breaking:** Good thing."
            ),
            encoding="utf-8",
        )

        self.assertEqual(release.current_semver_policy(self.root, "1.2.3"), "major")

    def test_current_semver_policy_falls_back_to_cut_release(self) -> None:
        version = release.prepare(
            self.root,
            "major",
            "2026-07-13",
            allow_dirty=True,
            breaking=True,
        )

        self.assertEqual(version, "2.0.0")
        self.assertEqual(release.current_semver_policy(self.root, "1.2.3"), "major")

    def test_current_semver_policy_combines_unpublished_cut_and_new_notes(self) -> None:
        version = release.prepare(
            self.root,
            "major",
            "2026-07-13",
            allow_dirty=True,
            breaking=True,
        )
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text(
            changelog.read_text(encoding="utf-8").replace(
                "## [Unreleased]\n\n",
                "## [Unreleased]\n\n### Fixed\n\n- Follow-up fix.\n\n",
                1,
            ),
            encoding="utf-8",
        )

        self.assertEqual(version, "2.0.0")
        self.assertEqual(release.current_semver_policy(self.root, "1.2.3"), "major")
        self.assertEqual(release.current_semver_policy(self.root, "2.0.0"), "patch")

    def test_current_semver_policy_rejects_non_predecessor_registry_lag(self) -> None:
        release.prepare(
            self.root,
            "major",
            "2026-07-13",
            allow_dirty=True,
            breaking=True,
        )

        with self.assertRaisesRegex(release.ReleaseError, "immediate predecessor"):
            release.current_semver_policy(self.root, "1.0.0")

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

    def test_duplicate_partial_lock_entry_fails_before_writes(self) -> None:
        # A partial lock must never carry a member twice; that would make the
        # lockstep stamp ambiguous and the --locked verify unreliable.
        path = self.root / release.PARTIAL_LOCKFILES[0]
        path.write_text(
            path.read_text(encoding="utf-8")
            + '\n[[package]]\nname = "signal-fish-client"\nversion = "1.2.3"\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(release.ReleaseError, "duplicate locked"):
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

    def test_prepare_ignores_fenced_target_heading_example(self) -> None:
        # release_intent is fence-aware, so a documentation example quoting
        # the next version heading must not brick prepare with "already
        # contains release" after intent already cleared it. The example
        # survives the cut verbatim (inside its fence) while the real section
        # stays machine-parseable.
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text(
            "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- Good thing.\n\n"
            "```markdown\n## [1.3.0] - 2026-07-13\n\n- Example only.\n```\n\n"
            "## [1.2.3] - 2020-01-01\n\n- Old.\n\n"
            "[Unreleased]: https://example.test/compare/v1.2.3...HEAD\n",
            encoding="utf-8",
        )

        self.assertEqual(
            release.prepare(self.root, "minor", "2026-07-13", allow_dirty=True),
            "1.3.0",
        )
        cut = changelog.read_text(encoding="utf-8")
        self.assertIn("```markdown\n## [1.3.0] - 2026-07-13\n", cut)
        self.assertEqual(release.previous_version(self.root, "1.3.0"), "1.2.3")
        self.assertEqual(release.semver_policy(self.root, "1.3.0"), "minor")

    def test_prepare_keeps_notes_around_fenced_other_version_heading(self) -> None:
        # A fenced example quoting a different version heading must not bound
        # the section: notes on both sides survive the cut and no phantom
        # release heading becomes real.
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text(
            "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- Before fence.\n\n"
            "```markdown\n## [9.9.9] - 2024-01-01\n```\n\n"
            "- After fence.\n\n"
            "## [1.2.3] - 2020-01-01\n\n- Old.\n\n"
            "[Unreleased]: https://example.test/compare/v1.2.3...HEAD\n",
            encoding="utf-8",
        )

        release.prepare(self.root, "minor", "2026-07-13", allow_dirty=True)
        cut = changelog.read_text(encoding="utf-8")
        self.assertIn("- Before fence.", cut)
        self.assertIn("- After fence.", cut)
        self.assertIn("```markdown\n## [9.9.9] - 2024-01-01\n```", cut)
        self.assertNotIn("## [9.9.9]", release.strip_fenced_blocks(cut, self.root))
        self.assertEqual(release.previous_version(self.root, "1.3.0"), "1.2.3")

    def test_semver_policy_ignores_fenced_policy_marker(self) -> None:
        release.prepare(self.root, "minor", "2026-07-13", allow_dirty=True)
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text(
            changelog.read_text(encoding="utf-8").replace(
                "## [1.3.0] - 2026-07-13\n",
                "## [1.3.0] - 2026-07-13\n\n```markdown\n"
                "<!-- semver-checks: major -->\n```\n",
                1,
            ),
            encoding="utf-8",
        )

        self.assertEqual(release.semver_policy(self.root, "1.3.0"), "minor")

    def test_previous_version_ignores_fenced_compare_link(self) -> None:
        release.prepare(self.root, "minor", "2026-07-13", allow_dirty=True)
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text(
            changelog.read_text(encoding="utf-8").replace(
                "- Good thing.",
                "- Good thing.\n\n```text\n"
                "[1.3.0]: https://example.test/compare/v0.9.0...v1.3.0\n```",
                1,
            ),
            encoding="utf-8",
        )

        self.assertEqual(release.previous_version(self.root, "1.3.0"), "1.2.3")

    def test_cut_changelog_updates_the_real_footer_link(self) -> None:
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text(
            "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- Good thing.\n\n"
            "```text\n[Unreleased]: https://example.test/fenced\n```\n\n"
            "## [1.2.3] - 2020-01-01\n\n- Old.\n\n"
            "[Unreleased]: https://example.test/compare/v1.2.3...HEAD\n",
            encoding="utf-8",
        )

        release.cut_changelog(changelog, "1.2.3", "1.3.0", "2026-07-13")
        cut = changelog.read_text(encoding="utf-8")
        self.assertIn("compare/v1.3.0...HEAD", cut)
        self.assertIn("compare/v1.2.3...v1.3.0", cut)
        self.assertIn("[Unreleased]: https://example.test/fenced\n```\n", cut)

    def test_duplicate_unreleased_section_fails_closed(self) -> None:
        (self.root / "CHANGELOG.md").write_text(
            "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- First.\n\n"
            "## [Unreleased]\n\n### Fixed\n\n- Second.\n\n"
            "## [1.2.3] - 2020-01-01\n\n- Old.\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(release.ReleaseError, "duplicate"):
            release.release_intent(self.root)

    def test_release_notes_extracts_the_complete_section(self) -> None:
        version = release.prepare(self.root, "minor", "2026-07-13", allow_dirty=True)

        notes = release.release_notes(self.root, version)
        self.assertIn("- Good thing.", notes)
        self.assertNotIn("## [1.3.0]", notes)
        self.assertNotIn("- Old.", notes)
        self.assertNotIn("compare/v1.2.3...v1.3.0", notes)

    def test_release_notes_survive_fenced_release_syntax(self) -> None:
        # The extraction this replaced (an awk heading scan) truncated the
        # notes at the first fenced heading line; notes on both sides of a
        # fenced example must survive intact.
        release.prepare(self.root, "minor", "2026-07-13", allow_dirty=True)
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text(
            changelog.read_text(encoding="utf-8").replace(
                "- Good thing.",
                "- Good thing.\n\n```markdown\n## [1.3.0] - 2020-01-01\n```\n\n"
                "- After fenced heading.",
                1,
            ),
            encoding="utf-8",
        )

        notes = release.release_notes(self.root, "1.3.0")
        self.assertIn("- Good thing.", notes)
        self.assertIn("## [1.3.0] - 2020-01-01", notes)
        self.assertIn("- After fenced heading.", notes)
        self.assertNotIn("- Old.", notes)

    def test_release_notes_rejects_missing_section(self) -> None:
        with self.assertRaisesRegex(
            release.ReleaseError, "no complete release section"
        ):
            release.release_notes(self.root, "9.9.9")

    def test_release_notes_rejects_empty_section(self) -> None:
        (self.root / "CHANGELOG.md").write_text(
            "# Changelog\n\n## [1.2.3] - 2020-01-01\n\n## [1.2.2] - 2019-01-01\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(release.ReleaseError, "empty notes"):
            release.release_notes(self.root, "1.2.3")

    def test_release_notes_rejects_unterminated_fence(self) -> None:
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text(
            "# Changelog\n\n## [Unreleased]\n\n- Note.\n\n"
            "## [1.2.3] - 2020-01-01\n\n- Old.\n\n```text\nnever closed\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(release.ReleaseError, "unterminated fenced"):
            release.release_notes(self.root, "1.2.3")


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

    @mock.patch.object(release.urllib.request, "urlopen")
    def test_registry_checksum_rejects_malformed_payloads(self, urlopen: mock.Mock) -> None:
        class Response:
            def __init__(self, payload: bytes) -> None:
                self.payload = payload

            def read(self) -> bytes:
                return self.payload

            def __enter__(self) -> "Response":
                return self

            def __exit__(self, *_args: object) -> None:
                return None

        for payload in (b'{"version": null}', b"[]", b'{"version": []}'):
            with self.subTest(payload=payload):
                urlopen.return_value = Response(payload)
                with self.assertRaisesRegex(
                    release.ReleaseError, "invalid checksum"
                ):
                    release.registry_checksum("demo", "1.2.3")

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
                ({}, ["core", "adapter"], False),
                ({"core": checksums["core"]}, ["adapter"], True),
                (checksums, [], False),
            )
            for published, expected, requires_no_verify in cases:
                with self.subTest(published=published):
                    state = release.registry_plan(
                        plan, artifacts, lambda name, _version: published.get(name)
                    )
                    self.assertEqual(state["pending"], expected)
                    self.assertEqual(
                        state["resume_requires_no_verify"], requires_no_verify
                    )

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
        cls.releasing = (root / "docs/releasing.md").read_text(encoding="utf-8")

    def test_required_checks_policy_maps_to_dispatchable_workflows(self) -> None:
        # Release day reads this file twice: the Prepare dispatch loop runs
        # every listed workflow, and the Release gate expects every aggregate
        # job. A malformed file used to dispatch nothing silently; a renamed
        # aggregate would gate on a check no workflow produces.
        root = Path(__file__).resolve().parents[1]
        policy = json.loads(
            (root / ".github/required-checks.json").read_text(encoding="utf-8")
        )
        required = policy["required_checks"]
        self.assertTrue(required, "required-checks.json must list checks")
        for check in required:
            with self.subTest(file=check.get("file"), job=check.get("job")):
                path = root / ".github/workflows" / check["file"]
                self.assertTrue(path.is_file(), f"{check['file']} is missing")
                workflow = path.read_text(encoding="utf-8")
                self.assertIn("workflow_dispatch:", workflow)
                self.assertIn(f"name: {check['job']}", workflow)

    def test_prepare_uses_builtin_token_and_supports_dry_run(self) -> None:
        self.assertNotIn("actions/create-github-app-token", self.prepare)
        self.assertNotIn("RELEASE_APP_", self.prepare)
        self.assertIn("contents: write", self.prepare)
        self.assertIn("pull-requests: write", self.prepare)
        self.assertIn("actions: write", self.prepare)
        self.assertIn("persist-credentials: true", self.prepare)
        self.assertIn("GH_TOKEN: ${{ github.token }}", self.prepare)
        self.assertIn("dry_run:", self.prepare)
        dispatch = self.prepare.split("workflow_dispatch:", 1)[1].split(
            "permissions:", 1
        )[0]
        self.assertNotIn("bump:", dispatch)
        self.assertNotIn("breaking:", dispatch)
        self.assertIn("release-intent", self.prepare)
        self.assertIn("steps.intent.outputs.bump", self.prepare)
        self.assertIn("steps.intent.outputs.breaking", self.prepare)
        self.assertIn("branch=release/%s", self.prepare)
        self.assertIn("gh pr create", self.prepare)
        self.assertIn(".github/required-checks.json", self.prepare)
        self.assertIn('gh workflow run "$workflow" --ref "$BRANCH"', self.prepare)
        self.assertNotIn("Approve workflows to run", self.prepare)
        self.assertNotIn("Approve workflows to run", self.releasing)
        self.assertNotIn("signal-fish-release[bot]", self.prepare)
        self.assertIn('git config user.name "github-actions[bot]"', self.prepare)
        bot_email = (
            'git config user.email '
            '"41898282+github-actions[bot]@users.noreply.github.com"'
        )
        self.assertIn(bot_email, self.prepare)
        self.assertIn(bot_email, self.publish)
        self.assertNotIn(
            'git config user.email "github-actions[bot]@users.noreply.github.com"',
            self.prepare + self.publish,
        )

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
            "actions/attest@v4.2.2",
            "cargo publish --dry-run",
            "cargo publish",
            "gh release create",
        ):
            with self.subTest(marker=marker):
                self.assertIn(marker, self.publish)
        self.assertEqual(
            self.publish.count("resume_args+=(--no-verify)"),
            2,
            "both resumed dry-run and publish must tolerate crates.io index lag",
        )

    def test_pinned_cyclonedx_uses_its_workspace_flag(self) -> None:
        self.assertIn("cargo cyclonedx --format json --all", self.publish)
        self.assertNotIn("cargo cyclonedx --format json --workspace", self.publish)

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

    def test_ci_publish_dry_run_uses_the_release_toolchain(self) -> None:
        publish_job = self.ci.split("  publish-dry-run:", 1)[1].split(
            "\n  required:", 1
        )[0]
        self.assertIn("toolchain: ${{ env.RELEASE_RUST }}", publish_job)
        self.assertIn('cargo +"${RELEASE_RUST}" publish --dry-run', publish_job)
        self.assertIn('RELEASE_RUST: "1.96.1"', self.ci)

    def test_version_file_inventory_matches_this_checkout(self) -> None:
        # Guards the release-day inventory against documentation drift like
        # docs/examples.md losing its version reference in 3f38367, which made
        # every Prepare Release run fail before writing anything.
        root = Path(__file__).resolve().parents[1]
        version = release.package_version(root)
        for relative in release.VERSION_FILES:
            with self.subTest(file=relative):
                path = root / relative
                self.assertTrue(path.is_file(), f"{relative} is missing")
                self.assertIn(
                    version,
                    path.read_text(encoding="utf-8"),
                    f"{relative} does not mention workspace version {version}",
                )

    def test_compatibility_top_level_identity_matches_this_checkout(self) -> None:
        # Guards the release-day identity inventory against drift like #133
        # adding a section-level synced date to tests/compatibility.toml,
        # which made every Prepare Release run fail before writing anything.
        # Prepare stamps exactly one top-level client_version and synced
        # date; section tables hold upstream provenance and may carry their
        # own synced dates.
        root = Path(__file__).resolve().parents[1]
        text = (root / "tests/compatibility.toml").read_text(encoding="utf-8")
        header = text.partition("\n[")[0]
        client_versions = re.findall(
            r'^client_version = "[^"]+"$', header, re.MULTILINE
        )
        self.assertEqual(
            len(client_versions),
            1,
            "tests/compatibility.toml must have exactly one top-level "
            "client_version",
        )
        dates = re.findall(
            r'^synced = "[0-9]{4}-[0-9]{2}-[0-9]{2}"$', header, re.MULTILINE
        )
        self.assertEqual(
            len(dates),
            1,
            "tests/compatibility.toml must have exactly one top-level synced date",
        )


if __name__ == "__main__":
    unittest.main()
