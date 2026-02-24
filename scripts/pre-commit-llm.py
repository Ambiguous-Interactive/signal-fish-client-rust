#!/usr/bin/env python3
"""
Pre-commit hook for .llm/ folder enforcement.

Checks include:
- Sync selected crate-version references to Cargo.toml package version.
- Enforce .llm markdown line limits.
- Auto-generate .llm/skills/index.md from skill file headings/descriptions.
- Validate docs and mkdocs consistency checks.
- Reject stale release-specific wording for unstable rustdoc removals.
- Advisory: warn about absolute guarantee language in Rust doc comments.
"""

import subprocess
import sys
import re
from pathlib import Path

MAX_LINES = 300
REPO_ROOT = Path(__file__).resolve().parent.parent
LLM_DIR = REPO_ROOT / ".llm"
SKILLS_DIR = LLM_DIR / "skills"
INDEX_FILE = SKILLS_DIR / "index.md"


def read_cargo_package_version() -> str:
    """Read `[package].version` from Cargo.toml."""
    cargo_toml = REPO_ROOT / "Cargo.toml"
    try:
        content = cargo_toml.read_text(encoding="utf-8")
    except OSError as e:
        raise RuntimeError(f"Could not read {cargo_toml}: {e}") from e

    in_package_section = False
    for line in content.splitlines():
        stripped = line.strip()

        if stripped.startswith("[") and stripped.endswith("]"):
            in_package_section = stripped == "[package]"
            continue

        if not in_package_section:
            continue

        match = re.match(r'^version\s*=\s*"([^"]+)"\s*$', stripped)
        if match:
            return match.group(1)

    raise RuntimeError("Cargo.toml [package].version is missing or invalid.")


def sync_crate_version_references(crate_version: str) -> tuple[list[str], list[Path]]:
    """Sync selected docs/context references to the canonical crate version."""
    errors = []
    changed_files = []
    replacements = {
        REPO_ROOT / "README.md": [
            (
                re.compile(r'(signal-fish-client\s*=\s*")([^"]+)(")'),
                rf"\g<1>{crate_version}\g<3>",
            ),
            (
                re.compile(
                    r'(signal-fish-client\s*=\s*\{[^}\n]*\bversion\s*=\s*")([^"]+)(")'
                ),
                rf"\g<1>{crate_version}\g<3>",
            ),
        ],
        REPO_ROOT / "docs" / "getting-started.md": [
            (
                re.compile(r'(signal-fish-client\s*=\s*")([^"]+)(")'),
                rf"\g<1>{crate_version}\g<3>",
            ),
            (
                re.compile(
                    r'(signal-fish-client\s*=\s*\{[^}\n]*\bversion\s*=\s*")([^"]+)(")'
                ),
                rf"\g<1>{crate_version}\g<3>",
            ),
        ],
        REPO_ROOT / "docs" / "index.md": [
            (
                re.compile(
                    r'(signal-fish-client\s*=\s*\{[^}\n]*\bversion\s*=\s*")([^"]+)(")'
                ),
                rf"\g<1>{crate_version}\g<3>",
            )
        ],
        REPO_ROOT / "docs" / "client.md": [
            (
                re.compile(r'(sdk_version:\s*Some\(")([^"]+)("\.into\(\)\),)'),
                rf"\g<1>{crate_version}\g<3>",
            )
        ],
        REPO_ROOT / "docs" / "protocol.md": [
            (
                re.compile(r'("sdk_version"\s*:\s*")([^"]+)(")'),
                rf"\g<1>{crate_version}\g<3>",
            )
        ],
        REPO_ROOT / ".llm" / "context.md": [
            (
                re.compile(r"(- \*\*Version:\*\*\s*)([^\s]+)"),
                rf"\g<1>{crate_version}",
            )
        ],
        REPO_ROOT / ".llm" / "skills" / "crate-publishing.md": [
            (
                re.compile(r'(^version\s*=\s*")([^"]+)(")', flags=re.MULTILINE),
                rf"\g<1>{crate_version}\g<3>",
            )
        ],
    }

    for path, path_replacements in replacements.items():
        if not path.exists():
            continue
        try:
            original = path.read_text(encoding="utf-8")
        except OSError as e:
            errors.append(f"  Could not read {path}: {e}")
            continue

        updated = original
        for pattern, replacement in path_replacements:
            updated = pattern.sub(replacement, updated)

        if updated != original:
            try:
                path.write_text(updated, encoding="utf-8")
            except OSError as e:
                errors.append(f"  Could not write {path}: {e}")
                continue
            changed_files.append(path)

    return errors, changed_files


def find_md_files(directory: Path) -> list[Path]:
    """Recursively find all .md files under a directory."""
    return sorted(directory.rglob("*.md"))


def check_line_counts(md_files: list[Path]) -> list[str]:
    """Return a list of error messages for files exceeding MAX_LINES."""
    errors = []
    for path in md_files:
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
            count = len(lines)
            if count > MAX_LINES:
                rel = path.relative_to(REPO_ROOT)
                errors.append(
                    f"  {rel}: {count} lines (limit is {MAX_LINES})"
                )
        except OSError as e:
            errors.append(f"  Could not read {path}: {e}")
    return errors


def extract_title(text: str) -> str:
    """Extract the first H1 heading from markdown text.

    Headings inside fenced code blocks (``` or ~~~) are ignored.
    """
    fence_char = None
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("```") or stripped.startswith("~~~"):
            char = stripped[0]
            if fence_char is not None:
                if char == fence_char:
                    fence_char = None
                continue
            fence_char = char
            continue
        if fence_char is not None:
            continue
        if stripped.startswith("# "):
            return stripped[2:].strip()
    return "(Untitled)"


def extract_first_paragraph(text: str) -> str:
    """Extract the first non-heading, non-blank paragraph of text.

    Fenced code blocks (``` ... ``` and ~~~ ... ~~~) are properly skipped
    so that lines inside a code fence are never treated as paragraph content.
    Backtick fences and tilde fences are tracked independently per CommonMark.
    """
    lines = text.splitlines()
    in_paragraph = False
    fence_char = None
    paragraph_lines = []

    for line in lines:
        stripped = line.strip()
        # Toggle code-fence state on opening/closing markers
        if stripped.startswith("```") or stripped.startswith("~~~"):
            char = stripped[0]
            if fence_char is not None:
                if char == fence_char:
                    fence_char = None
                continue
            fence_char = char
            if in_paragraph:
                break
            continue
        # While inside a code fence, skip all lines
        if fence_char is not None:
            continue
        # Skip headings
        if stripped.startswith("#"):
            if in_paragraph:
                break
            continue
        # Blank line ends a paragraph
        if not stripped:
            if in_paragraph:
                break
            continue
        in_paragraph = True
        paragraph_lines.append(stripped)

    return " ".join(paragraph_lines).strip()


def generate_index(skill_files: list[Path]) -> str:
    """Generate the content of skills/index.md."""
    lines = [
        "# Skills Index",
        "",
        "> **AUTO-GENERATED** — Do not edit this file manually.",
        "> It is regenerated by `scripts/pre-commit-llm.py` on every commit.",
        "",
        "This index lists all skill reference guides available in `.llm/skills/`.",
        "Each skill is a focused, practical guide for a specific topic in this codebase.",
        "",
        "## Available Skills",
        "",
    ]

    for path in skill_files:
        rel = path.relative_to(SKILLS_DIR)
        try:
            text = path.read_text(encoding="utf-8")
        except OSError:
            continue

        title = extract_title(text)
        description = extract_first_paragraph(text)

        # Truncate long descriptions
        if len(description) > 120:
            description = description[:117] + "..."

        lines.append(f"### [{title}]({rel})")
        lines.append("")
        if description:
            lines.append(description)
            lines.append("")

    lines.append("---")
    lines.append("")
    lines.append(
        "*Generated by `scripts/pre-commit-llm.py`. "
        "Run `bash scripts/install-hooks.sh` to install the pre-commit hook.*"
    )
    lines.append("")

    return "\n".join(lines)


def git_add(path: Path) -> None:
    """Stage a file with git add."""
    rel = str(path.relative_to(REPO_ROOT))
    result = subprocess.run(
        ["git", "add", rel],
        cwd=str(REPO_ROOT),
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        print(f"Warning: could not stage {rel}: {result.stderr.strip()}", file=sys.stderr)


def validate_mkdocs_nav() -> list[str]:
    """Validate that all files referenced in mkdocs.yml nav exist in docs/.

    Returns a list of error messages for missing files.
    """
    mkdocs_yml = REPO_ROOT / "mkdocs.yml"
    docs_dir = REPO_ROOT / "docs"
    errors = []

    if not mkdocs_yml.exists():
        return errors
    if not docs_dir.is_dir():
        return errors

    try:
        content = mkdocs_yml.read_text(encoding="utf-8")
    except OSError as e:
        errors.append(f"  Could not read {mkdocs_yml}: {e}")
        return errors
    in_nav = False

    for line_num, line in enumerate(content.splitlines(), start=1):
        trimmed = line.strip()

        # Detect the start of the nav section
        if trimmed == "nav:":
            in_nav = True
            continue

        # Detect exit from nav section (top-level key)
        if in_nav and line and not line.startswith(" ") and not line.startswith("#"):
            break

        if not in_nav:
            continue

        # Nav entries look like: `  - Label: filename.md`
        # or bare entries: `  - filename.md`
        if trimmed.startswith("- "):
            rest = trimmed[2:]
            # Split on the LAST `: ` to handle labels with colons
            colon_pos = rest.rfind(": ")
            if colon_pos != -1:
                file_ref = rest[colon_pos + 2:].strip()
            elif rest.strip().endswith(".md"):
                # Bare entry without a label (e.g., `- filename.md`)
                file_ref = rest.strip()
            else:
                file_ref = None

            if file_ref and file_ref.endswith(".md"):
                full_path = docs_dir / file_ref
                try:
                    exists = full_path.is_file()
                except OSError as e:
                    errors.append(f"  Could not check {full_path}: {e}")
                    continue
                if not exists:
                    errors.append(
                        f"  mkdocs.yml nav (line {line_num}) references "
                        f"'{file_ref}' but docs/{file_ref} does not exist."
                    )

    return errors


def validate_yaml_step_indentation(md_files: list[Path]) -> list[str]:
    """Validate fenced YAML step blocks use consistent step key indentation.

    Detects malformed snippets where step keys (`name:`, `uses:`, `with:`, `run:`)
    are not aligned exactly to the list-item mapping key indentation established
    by each `- ...` step item.
    """
    errors = []

    for path in md_files:
        try:
            content = path.read_text(encoding="utf-8")
        except OSError as e:
            errors.append(f"  Could not read {path}: {e}")
            continue

        in_yaml_fence = False
        fence_char = None
        expected_step_key_indent = None
        step_item_indent = None

        for line_num, line in enumerate(content.splitlines(), start=1):
            stripped = line.strip()

            if stripped.startswith("```") or stripped.startswith("~~~"):
                char = stripped[0]
                if fence_char is not None:
                    if char == fence_char:
                        fence_char = None
                        in_yaml_fence = False
                        expected_step_key_indent = None
                        step_item_indent = None
                    continue

                fence_char = char
                fence_lang = stripped[3:].strip().lower()
                in_yaml_fence = fence_lang in {"yaml", "yml"}
                expected_step_key_indent = None
                step_item_indent = None
                continue

            if fence_char is None or not in_yaml_fence:
                continue

            step_name_match = re.match(r"^(\s*)-\s+name\s*:", line)
            if step_name_match:
                step_item_indent = len(step_name_match.group(1))
                expected_step_key_indent = step_item_indent + 2
                continue

            # Keep alignment context only when a sibling top-level step item starts.
            # Nested list items under `with`/other mappings must not reset alignment.
            step_item_match = re.match(r"^(\s*)-\s+", line)
            if step_item_match and step_item_indent is not None:
                item_indent = len(step_item_match.group(1))
                if item_indent == step_item_indent:
                    expected_step_key_indent = step_item_indent + 2
                continue

            step_key_match = re.match(r"^(\s*)(uses|with|run)\s*:", line)
            if step_key_match and expected_step_key_indent is not None:
                actual_indent = len(step_key_match.group(1))
                key_name = step_key_match.group(2)
                if actual_indent != expected_step_key_indent:
                    direction = (
                        "over-indented"
                        if actual_indent > expected_step_key_indent
                        else "under-indented"
                    )
                    rel = path.relative_to(REPO_ROOT)
                    errors.append(
                        f"  {rel}:{line_num} malformed fenced YAML step: "
                        f"`{key_name}:` is {direction} (got {actual_indent}, "
                        f"expected {expected_step_key_indent})."
                    )

    return errors


def validate_doc_nav_card_consistency() -> list[str]:
    """Validate that nav card labels in docs/index.md match page H1 headings.

    Returns a list of error messages for mismatched labels.
    """
    index_path = REPO_ROOT / "docs" / "index.md"
    docs_dir = REPO_ROOT / "docs"
    errors = []

    if not index_path.exists():
        return errors

    try:
        content = index_path.read_text(encoding="utf-8")
    except OSError as e:
        errors.append(f"  Could not read {index_path}: {e}")
        return errors

    card_pattern = re.compile(
        r"\[:octicons-arrow-right-24:\s+(.+?)\]\(([^)]+\.md)\)"
    )

    for match in card_pattern.finditer(content):
        label = match.group(1)
        filename = match.group(2)

        # Skip external URLs
        if filename.startswith("http"):
            continue

        target_path = docs_dir / filename
        try:
            target_content = target_path.read_text(encoding="utf-8")
        except OSError as e:
            errors.append(f"  Could not read docs/{filename}: {e}")
            continue

        h1 = extract_title(target_content)
        if h1 == "(Untitled)":
            errors.append(
                f"  docs/{filename} has no H1 heading. "
                f"Every docs page must start with a `# Title` heading."
            )
            continue

        if label != h1:
            errors.append(
                f"  Card label \"{label}\" does not match H1 \"{h1}\" "
                f"in docs/{filename}."
            )

    return errors


def validate_changelog_example_links(md_files: list[Path]) -> list[str]:
    """Validate Keep a Changelog-style reference links are internally consistent.

    Rules:
    - If a markdown file defines an `[Unreleased]: ...` link plus one or more
      version links (`[X.Y.Z]: ...`), then:
      1. `[Unreleased]` must compare from the latest linked version to HEAD.
      2. The latest linked version must point to either:
         - `/releases/tag/vX.Y.Z`, or
         - `/compare/vPREV...vX.Y.Z`
    """
    errors = []
    link_ref_re = re.compile(r"^\[([^\]]+)\]:\s*(\S+)\s*$")
    semver_label_re = re.compile(r"^\d+\.\d+\.\d+$")
    compare_re = re.compile(r"/compare/v(\d+\.\d+\.\d+)\.\.\.HEAD(?:[#?].*)?$")
    release_tag_re = re.compile(r"/releases/tag/v(\d+\.\d+\.\d+)(?:[#?].*)?$")
    release_compare_re = re.compile(
        r"/compare/v(\d+\.\d+\.\d+)\.\.\.v(\d+\.\d+\.\d+)(?:[#?].*)?$"
    )

    for path in md_files:
        try:
            content = path.read_text(encoding="utf-8")
        except OSError as e:
            errors.append(f"  Could not read {path}: {e}")
            continue

        refs = {}
        for line in content.splitlines():
            match = link_ref_re.match(line.strip())
            if match:
                refs[match.group(1)] = match.group(2)

        if "Unreleased" not in refs:
            continue

        version_labels = [label for label in refs if semver_label_re.match(label)]
        if not version_labels:
            continue

        latest = max(
            version_labels,
            key=lambda v: tuple(int(part) for part in v.split(".")),
        )

        unreleased_url = refs["Unreleased"]
        compare_match = compare_re.search(unreleased_url)
        try:
            rel = path.resolve().relative_to(REPO_ROOT.resolve())
        except ValueError:
            rel = path
        if compare_match is None:
            errors.append(
                f"  {rel}: [Unreleased] link should use '/compare/v{latest}...HEAD'. "
                f"Found: {unreleased_url}"
            )
        elif compare_match.group(1) != latest:
            errors.append(
                f"  {rel}: [Unreleased] compares from v{compare_match.group(1)} "
                f"but latest linked version is {latest}. "
                f"Expected compare/v{latest}...HEAD."
            )

        latest_url = refs.get(latest)
        if latest_url is None:
            errors.append(
                f"  {rel}: missing link reference for latest version [{latest}]."
            )
            continue

        release_tag_match = release_tag_re.search(latest_url)
        release_compare_match = release_compare_re.search(latest_url)
        if release_tag_match is None and release_compare_match is None:
            errors.append(
                f"  {rel}: [{latest}] link should use either "
                f"'/releases/tag/v{latest}' or '/compare/vPREV...v{latest}'. "
                f"Found: {latest_url}"
            )
        elif (
            release_tag_match is not None
            and release_tag_match.group(1) != latest
        ):
            errors.append(
                f"  {rel}: [{latest}] link points to v{release_tag_match.group(1)}; "
                f"expected v{latest}."
            )
        elif (
            release_compare_match is not None
            and release_compare_match.group(2) != latest
        ):
            errors.append(
                f"  {rel}: [{latest}] compare link ends at v{release_compare_match.group(2)}; "
                f"expected v{latest}."
            )

    return errors


def validate_unstable_feature_wording(md_files: list[Path]) -> list[str]:
    """Reject stale release-specific wording for unstable rustdoc removals."""
    errors = []

    for path in md_files:
        try:
            content = path.read_text(encoding="utf-8")
        except OSError as e:
            errors.append(f"  Could not read {path}: {e}")
            continue

        if "doc_auto_cfg" in content and "removed in Rust " in content:
            rel = path.relative_to(REPO_ROOT)
            errors.append(
                f"  {rel}: avoid release-specific wording ('removed in Rust ...') "
                "for `doc_auto_cfg`. Use stable wording such as "
                "'removed from rustdoc'."
            )

    return errors


def warn_absolute_guarantee_language() -> list[str]:
    """Scan src/**/*.rs doc comments for absolute guarantee language.

    Detects words like "always", "never", "guaranteed", "unconditional" when
    they appear alongside delivery/event-related terms in doc comments.
    Returns a list of advisory warning strings (does not cause hook failure).
    """
    warnings = []
    src_dir = REPO_ROOT / "src"
    if not src_dir.is_dir():
        return warnings

    guarantee_re = re.compile(
        r"\b(always|never|guaranteed|unconditional(?:ly)?)\b", re.IGNORECASE
    )
    delivery_re = re.compile(
        r"\b(deliver(?:y|ed|s)?|event|message|dispatch(?:ed|es)?|"
        r"emit(?:ted|s)?|send|sent|receive[ds]?|notify|notif(?:ied|ication))\b",
        re.IGNORECASE,
    )

    for path in sorted(src_dir.rglob("*.rs")):
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except OSError:
            continue

        for line_num, line in enumerate(lines, start=1):
            stripped = line.strip()
            if not (stripped.startswith("///") or stripped.startswith("//!")):
                continue
            if guarantee_re.search(stripped) and delivery_re.search(stripped):
                rel = path.relative_to(REPO_ROOT)
                warnings.append(
                    f"  {rel}:{line_num}: {stripped.strip()}"
                )

    return warnings


def main() -> int:
    if not LLM_DIR.exists():
        print("No .llm/ directory found — skipping LLM hook.", file=sys.stderr)
        return 0

    # 1. Sync selected version references from Cargo.toml (blocking on errors)
    version_sync_errors = []
    try:
        crate_version = read_cargo_package_version()
    except RuntimeError as e:
        version_sync_errors.append(f"  {e}")
        crate_version = None

    if crate_version is not None:
        sync_errors, changed_files = sync_crate_version_references(crate_version)
        version_sync_errors.extend(sync_errors)
        for changed in changed_files:
            git_add(changed)
            rel = changed.relative_to(REPO_ROOT)
            print(f"Synced crate version reference in {rel} -> {crate_version}")

    # 2. Collect all .md files under .llm/
    all_md = find_md_files(LLM_DIR)
    index_generation_errors = []

    # Separate skill files (excluding index.md itself) from other .llm/ files
    skill_files = sorted(
        f for f in SKILLS_DIR.glob("*.md")
        if f.name != "index.md"
    ) if SKILLS_DIR.exists() else []

    # 3. Generate the index BEFORE line-count checks
    #    so that the index can be checked too
    if skill_files:
        index_content = generate_index(skill_files)
        try:
            INDEX_FILE.write_text(index_content, encoding="utf-8")
        except OSError as e:
            index_generation_errors.append(
                f"  Could not write {INDEX_FILE}: {e}"
            )
        else:
            git_add(INDEX_FILE)
            print(
                f"Generated .llm/skills/index.md "
                f"({len(index_content.splitlines())} lines)"
            )

    # Refresh the list after generating index
    all_md = find_md_files(LLM_DIR)

    # 4. Check line counts for all .md files under .llm/
    line_count_errors = check_line_counts(all_md)

    # 5. Run devcontainer documentation validation (non-blocking)
    validate_script = REPO_ROOT / "scripts" / "validate-devcontainer-docs.sh"
    if validate_script.exists():
        result = subprocess.run(
            ["bash", str(validate_script)],
            cwd=str(REPO_ROOT),
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            print(
                "\nWarning: devcontainer documentation validation failed:",
                file=sys.stderr,
            )
            if result.stdout.strip():
                for line in result.stdout.strip().splitlines():
                    print(f"  {line}", file=sys.stderr)
            if result.stderr.strip():
                for line in result.stderr.strip().splitlines():
                    print(f"  {line}", file=sys.stderr)
        else:
            if result.stdout.strip():
                print(result.stdout.strip())

    # 6. Validate mkdocs.yml nav references (blocking)
    nav_errors = validate_mkdocs_nav()

    # 7. Validate fenced YAML workflow step indentation in docs (blocking)
    yaml_step_indentation_errors = validate_yaml_step_indentation(all_md)

    # 8. Validate nav card labels match page titles (blocking)
    nav_card_errors = validate_doc_nav_card_consistency()

    # 9. Validate changelog-style reference links are version-consistent (blocking)
    changelog_link_errors = validate_changelog_example_links(all_md)

    # 10. Validate unstable feature wording in .llm markdown (blocking)
    unstable_wording_errors = validate_unstable_feature_wording(all_md)

    # 11. Advisory: warn about absolute guarantee language in doc comments
    guarantee_warnings = warn_absolute_guarantee_language()
    if guarantee_warnings:
        print(
            "\nWarning: absolute guarantee language in doc comments "
            "(advisory only — not blocking):",
            file=sys.stderr,
        )
        for w in guarantee_warnings:
            print(w, file=sys.stderr)
        print(
            "\nPlease verify these guarantees are accurate. "
            "Words like 'always', 'never', 'guaranteed', and "
            "'unconditional' near delivery/event terms may "
            "over-promise to callers.",
            file=sys.stderr,
        )

    # 12. Report all collected errors together
    error_sections = [
        (
            version_sync_errors,
            "crate version synchronization checks failed:",
            "Fix Cargo.toml version parsing or read/write permissions in version-sync target files.",
        ),
        (
            line_count_errors,
            f"the following .llm/ files exceed {MAX_LINES} lines:",
            "Please split these files or reduce their content before committing.",
        ),
        (
            index_generation_errors,
            "skills index generation failed:",
            "Ensure .llm/skills/index.md is writable and parent directories exist.",
        ),
        (
            nav_errors,
            "mkdocs.yml nav references missing files:",
            "Every file in mkdocs.yml nav must exist in docs/. Either create the file or remove the nav entry.",
        ),
        (
            yaml_step_indentation_errors,
            "malformed fenced YAML step indentation:",
            "In fenced YAML examples, align `uses:`, `with:`, and `run:` with the same step item key alignment as `- name:`.",
        ),
        (
            nav_card_errors,
            "nav card labels do not match page titles:",
            "Update card labels in docs/index.md to match the H1 heading of each target page.",
        ),
        (
            changelog_link_errors,
            "changelog reference links are inconsistent:",
            "In changelog examples, [Unreleased] must compare from the latest released version and that release link must point to the same tag.",
        ),
        (
            unstable_wording_errors,
            "stale unstable-feature wording detected:",
            "Avoid release-specific claims like 'removed in Rust X.Y'. Prefer wording that stays accurate over time, such as 'removed from rustdoc'.",
        ),
    ]

    if any(errors for errors, _, _ in error_sections):
        for errors, title, guidance in error_sections:
            if not errors:
                continue
            print(f"\nPre-commit hook FAILED: {title}", file=sys.stderr)
            for error in errors:
                print(error, file=sys.stderr)
            print(f"\n{guidance}", file=sys.stderr)
        return 1

    # Report clean status
    counts = []
    for path in all_md:
        n = len(path.read_text(encoding="utf-8").splitlines())
        rel = path.relative_to(REPO_ROOT)
        counts.append(f"  {rel}: {n} lines")

    print("All .llm/ files are within the 300-line limit:")
    for c in counts:
        print(c)

    return 0


if __name__ == "__main__":
    sys.exit(main())
