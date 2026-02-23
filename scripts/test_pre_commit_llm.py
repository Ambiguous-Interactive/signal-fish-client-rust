"""Tests for pre-commit-llm.py helper and validation functions."""

import importlib.util
import re
import sys
from pathlib import Path
from unittest.mock import patch

import pytest

# ---------------------------------------------------------------------------
# Import the module from the scripts directory using importlib so that we
# don't need the scripts directory on PYTHONPATH or an __init__.py file.
# ---------------------------------------------------------------------------

_SCRIPT_PATH = Path(__file__).resolve().parent / "pre-commit-llm.py"
_spec = importlib.util.spec_from_file_location("pre_commit_llm", _SCRIPT_PATH)
_mod = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = _mod
_spec.loader.exec_module(_mod)

extract_first_paragraph = _mod.extract_first_paragraph
extract_title = _mod.extract_title
generate_index = _mod.generate_index
validate_mkdocs_nav = _mod.validate_mkdocs_nav
validate_yaml_step_indentation = _mod.validate_yaml_step_indentation
validate_doc_nav_card_consistency = _mod.validate_doc_nav_card_consistency
validate_changelog_example_links = _mod.validate_changelog_example_links
validate_unstable_feature_wording = _mod.validate_unstable_feature_wording
read_cargo_package_version = _mod.read_cargo_package_version
sync_crate_version_references = _mod.sync_crate_version_references


# ===================================================================
# Tests for extract_first_paragraph
# ===================================================================


class TestExtractFirstParagraph:
    """Tests for the extract_first_paragraph helper."""

    def test_simple_paragraph_after_heading(self):
        """A plain paragraph immediately following an H1 heading is returned."""
        text = """\
# My Heading

This is the first paragraph."""
        assert extract_first_paragraph(text) == "This is the first paragraph."

    def test_paragraph_with_blank_lines_before_and_after(self):
        """Leading and trailing blank lines around the paragraph are ignored."""
        text = """\
# Title


This is the paragraph.


Some other text."""
        assert extract_first_paragraph(text) == "This is the paragraph."

    def test_code_fence_before_paragraph(self):
        """A fenced code block before the real paragraph is skipped entirely."""
        text = """\
# Heading

```
code line one
code line two
```

This is the actual paragraph."""
        assert extract_first_paragraph(text) == "This is the actual paragraph."

    def test_code_fence_with_language_specifier(self):
        """Code fences with language tags (```rust, ```toml) are recognized."""
        text = """\
# Heading

```rust
fn main() {}
```

Real paragraph here."""
        assert extract_first_paragraph(text) == "Real paragraph here."

    def test_code_fence_with_toml_specifier(self):
        """Another language specifier variant (```toml) is handled."""
        text = """\
# Config

```toml
[package]
name = "example"
```

Description of the config."""
        assert extract_first_paragraph(text) == "Description of the config."

    def test_multiple_code_fences_before_paragraph(self):
        """Multiple code fences before the paragraph are all skipped."""
        text = """\
# Heading

```
first block
```

```python
second block
```

The actual paragraph after two code blocks."""
        assert (
            extract_first_paragraph(text)
            == "The actual paragraph after two code blocks."
        )

    def test_code_fence_immediately_after_heading_no_paragraph_before(self):
        """When a code fence follows a heading with no paragraph in between,
        the paragraph after the code fence is returned."""
        text = """\
# Heading
```
some code
```
Paragraph after code fence."""
        assert extract_first_paragraph(text) == "Paragraph after code fence."

    def test_paragraph_followed_by_code_fence(self):
        """A paragraph that precedes a code fence stops at the fence boundary."""
        text = """\
# Title

First paragraph text.

```
code here
```"""
        assert extract_first_paragraph(text) == "First paragraph text."

    def test_paragraph_immediately_followed_by_code_fence(self):
        """If a code fence opens right after paragraph lines (no blank line),
        the paragraph ends and the fence content is excluded."""
        text = """\
# Title

Paragraph line.
```
code inside fence
```"""
        assert extract_first_paragraph(text) == "Paragraph line."

    def test_empty_text_returns_empty_string(self):
        """An empty input string returns an empty string."""
        assert extract_first_paragraph("") == ""

    def test_only_headings_returns_empty_string(self):
        """Input consisting solely of headings returns an empty string."""
        text = """\
# Heading One
## Heading Two
### Heading Three"""
        assert extract_first_paragraph(text) == ""

    def test_only_code_fences_returns_empty_string(self):
        """Input that contains only fenced code blocks returns an empty string."""
        text = """\
```
line inside fence
another line
```"""
        assert extract_first_paragraph(text) == ""

    def test_multi_line_paragraph(self):
        """Consecutive non-blank, non-heading lines are joined into one paragraph."""
        text = """\
# Heading

First line of paragraph.
Second line of paragraph.
Third line of paragraph.

Another paragraph that should not appear."""
        assert extract_first_paragraph(text) == (
            "First line of paragraph. "
            "Second line of paragraph. "
            "Third line of paragraph."
        )

    def test_code_fence_contents_never_leak_into_result(self):
        """The original bug: code fence contents must never appear in the result.

        This is the regression test for the bug that was fixed.  Before the fix,
        lines inside a code fence could be mistakenly treated as paragraph text.
        """
        text = """\
# Skill Title

```toml
[dependencies]
signal-fish-client = "0.1"
```

This is the real description of the skill."""
        result = extract_first_paragraph(text)
        # The result must not contain any code-fence content
        assert "[dependencies]" not in result
        assert "signal-fish-client" not in result
        assert result == "This is the real description of the skill."

    def test_code_fence_content_does_not_leak_without_trailing_paragraph(self):
        """Even when there is no paragraph after the fence, fence content must
        not leak."""
        text = """\
# Title

```
leaked = true
```"""
        result = extract_first_paragraph(text)
        assert "leaked" not in result
        assert result == ""

    def test_only_blank_lines_returns_empty(self):
        """Input with only whitespace/blank lines returns an empty string."""
        text = "\n\n   \n\n"
        assert extract_first_paragraph(text) == ""

    def test_no_heading_just_paragraph(self):
        """A paragraph without any preceding heading is still extracted."""
        text = "Just a paragraph with no heading."
        assert extract_first_paragraph(text) == "Just a paragraph with no heading."

    def test_heading_inside_code_fence_is_ignored(self):
        """Headings inside a code fence are not treated as real headings."""
        text = """\
```
# This is not a real heading
```

Actual paragraph."""
        assert extract_first_paragraph(text) == "Actual paragraph."

    def test_nested_backticks_in_code_fence(self):
        """Backtick sequences shorter than ``` inside a fence do not close it."""
        text = """\
# Title

```
some `inline` code
```

The paragraph."""
        assert extract_first_paragraph(text) == "The paragraph."


# ===================================================================
# Tests for crate version synchronization helpers
# ===================================================================


class TestCrateVersionSync:
    """Tests for Cargo version parsing and version-reference synchronization."""

    def test_read_cargo_package_version(self, tmp_path, monkeypatch):
        """The package version is read from Cargo.toml [package]."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        (fake_root / "Cargo.toml").write_text(
            "[package]\n"
            'name = "signal-fish-client"\n'
            'version = "1.2.3"\n'
            "\n"
            "[dependencies]\n"
            'tokio = "1"\n',
            encoding="utf-8",
        )
        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        assert read_cargo_package_version() == "1.2.3"

    def test_sync_crate_version_references_updates_target_files(
        self,
        tmp_path,
        monkeypatch,
    ):
        """Known version references are updated to the Cargo.toml version."""
        fake_root = tmp_path / "repo"
        (fake_root / ".llm" / "skills").mkdir(parents=True)
        (fake_root / "docs").mkdir(parents=True)

        (fake_root / "README.md").write_text(
            'signal-fish-client = "0.1"\n'
            'signal-fish-client = { version = "0.1", default-features = false }\n',
            encoding="utf-8",
        )
        (fake_root / "docs" / "getting-started.md").write_text(
            'signal-fish-client = "0.1"\n'
            'signal-fish-client = { version = "0.1", default-features = false }\n'
            'signal-fish-client = { version = "0.1", default-features = false, features = ["transport-websocket"] }\n',
            encoding="utf-8",
        )
        (fake_root / "docs" / "index.md").write_text(
            'signal-fish-client = { version = "*", features = ["transport-websocket"] }\n',
            encoding="utf-8",
        )
        (fake_root / ".llm" / "context.md").write_text(
            "- **Version:** 0.1.0\n",
            encoding="utf-8",
        )
        (fake_root / ".llm" / "skills" / "crate-publishing.md").write_text(
            "# Crate Publishing\n\n"
            "```toml\n"
            '[package]\nname = "signal-fish-client"\nversion = "0.1.0"\n'
            "```\n\n"
            "# Bump version (0.1.0 -> 0.2.0)\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        errors, changed_files = sync_crate_version_references("1.2.3")

        assert errors == []
        assert {path.relative_to(fake_root).as_posix() for path in changed_files} == {
            "README.md",
            "docs/getting-started.md",
            "docs/index.md",
            ".llm/context.md",
            ".llm/skills/crate-publishing.md",
        }

        assert 'signal-fish-client = "1.2.3"' in (
            fake_root / "README.md"
        ).read_text(encoding="utf-8")
        assert 'version = "1.2.3"' in (
            fake_root / "docs" / "getting-started.md"
        ).read_text(encoding="utf-8")
        assert 'version = "1.2.3"' in (
            fake_root / "docs" / "index.md"
        ).read_text(encoding="utf-8")
        assert "- **Version:** 1.2.3" in (
            fake_root / ".llm" / "context.md"
        ).read_text(encoding="utf-8")
        publishing = (fake_root / ".llm" / "skills" / "crate-publishing.md").read_text(
            encoding="utf-8"
        )
        assert 'version = "1.2.3"' in publishing
        assert "# Bump version (0.1.0 -> 0.2.0)" in publishing

    def test_main_workflow_step_comments_are_contiguous(self):
        """Numbered workflow comments in main() remain contiguous after edits."""
        source = _SCRIPT_PATH.read_text(encoding="utf-8")
        main_match = re.search(
            r'def main\(\) -> int:\n(?P<body>.*?)(?:\n\nif __name__ == "__main__":)',
            source,
            flags=re.DOTALL,
        )
        assert main_match is not None, "Could not locate main() in pre-commit script."

        main_body = main_match.group("body")
        step_numbers = [
            int(match.group(1))
            for match in re.finditer(r"^\s*#\s+(\d+)\.\s", main_body, flags=re.MULTILINE)
        ]

        assert len(step_numbers) >= 10, (
            "Expected at least 10 numbered workflow comments in main(); "
            f"found {len(step_numbers)}."
        )
        assert step_numbers == list(range(1, len(step_numbers) + 1)), (
            "Numbered workflow comments in main() must be contiguous and start at 1. "
            f"Found: {step_numbers}."
        )

    def test_tilde_fence_is_skipped(self):
        """Tilde fences (~~~) are recognized and their content is skipped."""
        text = """\
# Heading

~~~
code inside tilde fence
~~~

Paragraph after tilde fence."""
        assert extract_first_paragraph(text) == "Paragraph after tilde fence."

    def test_tilde_fence_with_language_specifier(self):
        """Tilde fences with a language tag (~~~python) are also handled."""
        text = """\
# Heading

~~~python
print("hello")
~~~

The real paragraph."""
        assert extract_first_paragraph(text) == "The real paragraph."

    def test_tilde_fence_content_does_not_leak(self):
        """Content inside tilde fences must never appear in the result."""
        text = """\
# Title

~~~toml
[dependencies]
signal-fish-client = "0.1"
~~~

This is the actual description."""
        result = extract_first_paragraph(text)
        assert "[dependencies]" not in result
        assert result == "This is the actual description."

    def test_mixed_fence_types_do_not_cross_close(self):
        """Backtick fence cannot be closed by tilde fence and vice versa."""
        text = """\
# Title

```
code inside backtick fence
~~~
This tilde should not close the backtick fence.
```

Actual paragraph.
"""
        assert extract_first_paragraph(text) == "Actual paragraph."

    def test_unclosed_code_fence_skips_remaining_lines(self):
        """If a code fence is opened but never closed, all remaining lines are
        skipped. If no paragraph was found before the fence, empty string is
        returned."""
        text = """\
# Title

```
this fence is never closed
more lines inside
still inside"""
        assert extract_first_paragraph(text) == ""

    def test_unclosed_code_fence_returns_paragraph_before_fence(self):
        """If a paragraph was collected before an unclosed fence, only that
        paragraph is returned and no fence content leaks."""
        text = """\
# Title

Paragraph before the fence.

```
unclosed fence content
more content"""
        result = extract_first_paragraph(text)
        assert result == "Paragraph before the fence."
        assert "unclosed" not in result


# ===================================================================
# Tests for extract_title
# ===================================================================


class TestExtractTitle:
    """Tests for the extract_title helper."""

    def test_simple_h1_heading(self):
        """A straightforward H1 heading is extracted."""
        text = """\
# My Title

Some paragraph."""
        assert extract_title(text) == "My Title"

    def test_no_heading_returns_untitled(self):
        """When there is no H1 heading, '(Untitled)' is returned."""
        text = "Just some text without any heading."
        assert extract_title(text) == "(Untitled)"

    def test_h2_heading_is_not_h1(self):
        """An H2 heading (##) does not count as an H1."""
        text = """\
## Not An H1

Paragraph."""
        assert extract_title(text) == "(Untitled)"

    def test_multiple_headings_returns_first_h1(self):
        """When multiple H1 headings exist, the first one is returned."""
        text = """\
# First Title

Some text.

# Second Title

More text."""
        assert extract_title(text) == "First Title"

    def test_h1_with_extra_whitespace(self):
        """Leading/trailing whitespace around the title text is stripped."""
        text = "#   Spaced Out Title   "
        assert extract_title(text) == "Spaced Out Title"

    def test_empty_text_returns_untitled(self):
        """An empty string input returns '(Untitled)'."""
        assert extract_title("") == "(Untitled)"

    def test_h3_and_h4_are_not_h1(self):
        """H3 and H4 headings are not treated as H1."""
        text = """\
### H3 Heading
#### H4 Heading"""
        assert extract_title(text) == "(Untitled)"

    def test_h1_inside_code_fence_is_ignored(self):
        """H1 heading inside a code fence is not treated as the title."""
        text = """\
```markdown
# Fake Title
```

# Real Title
"""
        assert extract_title(text) == "Real Title"

    def test_h1_after_other_content(self):
        """An H1 heading that appears after other content is still found."""
        text = """\
Some introductory text.

# The Real Title

More content."""
        assert extract_title(text) == "The Real Title"


# ===================================================================
# Tests for generate_index
# ===================================================================

# Regex that matches underscore-delimited emphasis per Markdown:
# A '_' not preceded by a word character, then non-empty non-underscore
# content (without newlines), then '_' not followed by a word character.
_UNDERSCORE_EMPHASIS_RE = re.compile(r"(?<!\w)_[^_\n]+_(?!\w)")


def _make_skill_file(directory: Path, name: str, content: str) -> Path:
    """Write a mock skill markdown file and return its Path."""
    path = directory / name
    path.write_text(content, encoding="utf-8")
    return path


@pytest.fixture()
def skills_dir(tmp_path, monkeypatch):
    """Create a temporary skills directory and monkeypatch SKILLS_DIR."""
    d = tmp_path / "skills"
    d.mkdir()
    monkeypatch.setattr(_mod, "SKILLS_DIR", d)
    return d


class TestGenerateIndex:
    """Tests for the generate_index function."""

    # -- (a) Footer uses asterisk emphasis, not underscore emphasis ----------

    def test_footer_uses_asterisk_emphasis(self, skills_dir):
        """The footer line uses *...* (asterisk) emphasis, not _..._ (underscore)."""
        skill = _make_skill_file(
            skills_dir,
            "example.md",
            "# Example Skill\n\nA short description.\n",
        )
        output = generate_index([skill])
        footer_lines = [
            line for line in output.splitlines() if "Generated by" in line
        ]
        assert len(footer_lines) == 1, "Expected exactly one footer line"
        footer = footer_lines[0]
        # Must use asterisk emphasis (not underscore)
        assert footer.startswith("*") and footer.endswith("*"), (
            f"Footer must use asterisk emphasis (*...*), got: {footer!r}"
        )

    # -- (b) No underscore emphasis anywhere in the output -------------------

    def test_no_underscore_emphasis_anywhere(self, skills_dir):
        """The entire generated output must be free of underscore emphasis."""
        skill = _make_skill_file(
            skills_dir,
            "example.md",
            "# Example Skill\n\nA short description.\n",
        )
        output = generate_index([skill])
        matches = _UNDERSCORE_EMPHASIS_RE.findall(output)
        assert matches == [], (
            f"Found underscore emphasis in generated index: {matches}"
        )

    # -- (c) Index starts with H1 -------------------------------------------

    def test_index_starts_with_h1(self, skills_dir):
        """The generated index must start with '# Skills Index'."""
        skill = _make_skill_file(
            skills_dir,
            "example.md",
            "# Example Skill\n\nDescription.\n",
        )
        output = generate_index([skill])
        first_line = output.splitlines()[0]
        assert first_line == "# Skills Index"

    # -- (d) Index contains auto-generated notice ---------------------------

    def test_index_contains_auto_generated_notice(self, skills_dir):
        """The generated output must contain the AUTO-GENERATED notice."""
        skill = _make_skill_file(
            skills_dir,
            "example.md",
            "# Example Skill\n\nDescription.\n",
        )
        output = generate_index([skill])
        assert "AUTO-GENERATED" in output

    # -- (e) Index contains skill entries with links -------------------------

    def test_index_contains_skill_entries(self, skills_dir):
        """Given mock skill files, the generated index contains entries with links."""
        skill_a = _make_skill_file(
            skills_dir,
            "alpha.md",
            "# Alpha Skill\n\nAlpha does alpha things.\n",
        )
        skill_b = _make_skill_file(
            skills_dir,
            "beta.md",
            "# Beta Skill\n\nBeta does beta things.\n",
        )
        output = generate_index([skill_a, skill_b])
        # Each skill should appear as an H3 link
        assert "### [Alpha Skill](alpha.md)" in output
        assert "### [Beta Skill](beta.md)" in output
        # Descriptions should appear
        assert "Alpha does alpha things." in output
        assert "Beta does beta things." in output

    @pytest.mark.parametrize(
        "filename, title, description",
        [
            ("async-rust.md", "Async Rust Patterns", "Patterns for async Rust."),
            ("error-handling.md", "Error Handling", "How to handle errors."),
            ("testing.md", "Testing Guide", "Guide for writing tests."),
        ],
    )
    def test_index_contains_parameterized_skill_entries(
        self, skills_dir, filename, title, description
    ):
        """Parameterized: each skill file produces a matching entry."""
        skill = _make_skill_file(
            skills_dir,
            filename,
            f"# {title}\n\n{description}\n",
        )
        output = generate_index([skill])
        assert f"### [{title}]({filename})" in output
        assert description in output

    # -- (f) markdownlint MD049 compliance -----------------------------------

    def test_markdownlint_md049_compliance(self, skills_dir):
        """No line in the generated output uses underscore-delimited emphasis
        (MD049 violation).  This is the most critical compliance test."""
        skill = _make_skill_file(
            skills_dir,
            "example.md",
            "# Example Skill\n\nA short description.\n",
        )
        output = generate_index([skill])
        for lineno, line in enumerate(output.splitlines(), start=1):
            match = _UNDERSCORE_EMPHASIS_RE.search(line)
            assert match is None, (
                f"MD049 violation on line {lineno}: underscore emphasis "
                f"found {match.group()!r} in line: {line!r}"
            )

    @pytest.mark.parametrize(
        "line",
        [
            "*Generated by `scripts/pre-commit-llm.py`. "
            "Run `bash scripts/install-hooks.sh` to install the pre-commit hook.*",
            "*Some other emphasized text.*",
        ],
        ids=["footer", "generic-emphasis"],
    )
    def test_md049_asterisk_emphasis_is_allowed(self, line):
        """Asterisk emphasis is valid and must not trigger MD049 detection."""
        assert _UNDERSCORE_EMPHASIS_RE.search(line) is None, (
            f"False positive: asterisk emphasis incorrectly matched: {line!r}"
        )

    @pytest.mark.parametrize(
        "line",
        [
            "_Generated by some tool._",
            "_italic text_",
            "Some _emphasized_ word.",
        ],
        ids=["full-line", "simple", "inline"],
    )
    def test_md049_underscore_emphasis_is_detected(self, line):
        """The underscore emphasis regex correctly detects violations."""
        assert _UNDERSCORE_EMPHASIS_RE.search(line) is not None, (
            f"Expected underscore emphasis to be detected in: {line!r}"
        )

    def test_md049_compliance_with_multiple_skills(self, skills_dir):
        """MD049 compliance holds even with many skill files producing a large index."""
        for i in range(10):
            _make_skill_file(
                skills_dir,
                f"skill-{i:02d}.md",
                f"# Skill Number {i}\n\nDescription for skill {i}.\n",
            )
        skill_files = sorted(skills_dir.glob("*.md"))
        output = generate_index(skill_files)
        for lineno, line in enumerate(output.splitlines(), start=1):
            match = _UNDERSCORE_EMPHASIS_RE.search(line)
            assert match is None, (
                f"MD049 violation on line {lineno}: {match.group()!r} "
                f"in: {line!r}"
            )

    def test_empty_skill_list_produces_valid_index(self, skills_dir):
        """Even with no skill files, the index structure is valid."""
        output = generate_index([])
        assert output.splitlines()[0] == "# Skills Index"
        assert "AUTO-GENERATED" in output
        # No underscore emphasis should appear
        assert _UNDERSCORE_EMPHASIS_RE.search(output) is None

    def test_skill_with_long_description_is_truncated(self, skills_dir):
        """Descriptions longer than 120 characters are truncated with '...'."""
        long_desc = "A" * 200
        skill = _make_skill_file(
            skills_dir,
            "verbose.md",
            f"# Verbose Skill\n\n{long_desc}\n",
        )
        output = generate_index([skill])
        # The description in the output should be truncated to 120 chars
        # (117 chars + "...")
        assert long_desc not in output
        assert "A" * 117 + "..." in output


# ===================================================================
# Tests for validate_mkdocs_nav
# ===================================================================


class TestValidateMkdocsNav:
    """Tests for the validate_mkdocs_nav function and its I/O error handling."""

    def test_unreadable_mkdocs_yml(self, tmp_path, monkeypatch):
        """When read_text raises OSError, validate_mkdocs_nav returns an error message."""
        # Set up a fake repo root with mkdocs.yml and docs/ present
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        mkdocs_yml = fake_root / "mkdocs.yml"
        mkdocs_yml.write_text("nav:\n  - Home: index.md\n", encoding="utf-8")
        docs_dir = fake_root / "docs"
        docs_dir.mkdir()

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        # Mock read_text to raise OSError when called on the mkdocs.yml path
        original_read_text = Path.read_text

        def mock_read_text(self, *args, **kwargs):
            if self.name == "mkdocs.yml":
                raise OSError("Permission denied")
            return original_read_text(self, *args, **kwargs)

        with patch.object(Path, "read_text", mock_read_text):
            errors = validate_mkdocs_nav()

        assert len(errors) == 1
        assert "Could not read" in errors[0]
        assert "Permission denied" in errors[0]

    def test_is_file_oserror_continues(self, tmp_path, monkeypatch):
        """When is_file raises OSError for one entry, the error is reported
        but validation continues checking subsequent entries."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        docs_dir = fake_root / "docs"
        docs_dir.mkdir()

        # Create mkdocs.yml with two nav entries
        mkdocs_yml = fake_root / "mkdocs.yml"
        mkdocs_yml.write_text(
            "nav:\n"
            "  - First: first.md\n"
            "  - Second: second.md\n",
            encoding="utf-8",
        )

        # Neither file exists, but first.md will raise OSError on is_file
        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        original_is_file = Path.is_file

        def mock_is_file(self):
            if self.name == "first.md":
                raise OSError("I/O error on first.md")
            return original_is_file(self)

        with patch.object(Path, "is_file", mock_is_file):
            errors = validate_mkdocs_nav()

        # Should have two errors: one OSError for first.md, one missing for second.md
        assert len(errors) == 2

        # First error: the OSError catch
        assert "Could not check" in errors[0]
        assert "I/O error on first.md" in errors[0]

        # Second error: the normal missing-file error for second.md
        assert "second.md" in errors[1]
        assert "does not exist" in errors[1]

    def test_valid_nav_no_errors(self, tmp_path, monkeypatch):
        """When all nav-referenced files exist, no errors are returned."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        docs_dir = fake_root / "docs"
        docs_dir.mkdir()

        # Create the referenced files
        (docs_dir / "index.md").write_text("# Home\n", encoding="utf-8")
        (docs_dir / "guide.md").write_text("# Guide\n", encoding="utf-8")

        mkdocs_yml = fake_root / "mkdocs.yml"
        mkdocs_yml.write_text(
            "nav:\n"
            "  - Home: index.md\n"
            "  - Guide: guide.md\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        errors = validate_mkdocs_nav()
        assert errors == []

    def test_no_mkdocs_yml_returns_empty(self, tmp_path, monkeypatch):
        """When REPO_ROOT has no mkdocs.yml, validate_mkdocs_nav returns an empty list."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        errors = validate_mkdocs_nav()
        assert errors == []

    def test_no_docs_dir_returns_empty(self, tmp_path, monkeypatch):
        """When REPO_ROOT has mkdocs.yml but no docs/ directory, returns empty list."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()

        mkdocs_yml = fake_root / "mkdocs.yml"
        mkdocs_yml.write_text(
            "nav:\n"
            "  - Home: index.md\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        errors = validate_mkdocs_nav()
        assert errors == []

    def test_bare_entry_without_label(self, tmp_path, monkeypatch):
        """A bare nav entry like `- index.md` (no label) produces no errors
        when the file exists."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        docs_dir = fake_root / "docs"
        docs_dir.mkdir()

        # Create the referenced file
        (docs_dir / "index.md").write_text("# Home\n", encoding="utf-8")

        mkdocs_yml = fake_root / "mkdocs.yml"
        mkdocs_yml.write_text(
            "nav:\n"
            "  - index.md\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        errors = validate_mkdocs_nav()
        assert errors == []

    def test_missing_nav_file_reports_error(self, tmp_path, monkeypatch):
        """A nav entry pointing to a non-existent file produces an error."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        docs_dir = fake_root / "docs"
        docs_dir.mkdir()

        mkdocs_yml = fake_root / "mkdocs.yml"
        mkdocs_yml.write_text(
            "nav:\n"
            "  - Missing: does-not-exist.md\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        errors = validate_mkdocs_nav()
        assert len(errors) == 1
        assert "does-not-exist.md" in errors[0]
        assert "does not exist" in errors[0]


# ===================================================================
# Tests for validate_yaml_step_indentation
# ===================================================================


class TestValidateYamlStepIndentation:
    """Tests for fenced YAML step indentation validation."""

    def test_valid_yaml_step_indentation_passes(self, tmp_path, monkeypatch):
        """Correctly aligned step keys in fenced YAML should not error."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        llm_dir = fake_root / ".llm"
        llm_dir.mkdir()
        doc = llm_dir / "example.md"
        doc.write_text(
            "# Example\n\n"
            "```yaml\n"
            "- name: Test on MSRV\n"
            "  uses: dtolnay/rust-toolchain@stable\n"
            "  with:\n"
            "    toolchain: 1.85.0\n"
            "- run: cargo test --all-features\n"
            "```\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        errors = validate_yaml_step_indentation([doc])
        assert errors == []

    def test_over_indented_uses_is_reported(self, tmp_path, monkeypatch):
        """Over-indented uses/with keys in fenced YAML should be blocked."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        llm_dir = fake_root / ".llm"
        llm_dir.mkdir()
        doc = llm_dir / "bad-example.md"
        doc.write_text(
            "# Bad Example\n\n"
            "```yaml\n"
            "- name: Test on MSRV\n"
            "    uses: dtolnay/rust-toolchain@stable\n"
            "    with:\n"
            "      toolchain: 1.85.0\n"
            "- run: cargo test --all-features\n"
            "```\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        errors = validate_yaml_step_indentation([doc])
        assert len(errors) == 2
        assert "bad-example.md" in errors[0]
        assert "`uses:` is over-indented" in errors[0]
        assert "`with:` is over-indented" in errors[1]

    def test_under_indented_step_keys_are_reported(self, tmp_path, monkeypatch):
        """Under-indented uses/with/run keys in a step should be blocked."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        llm_dir = fake_root / ".llm"
        llm_dir.mkdir()
        doc = llm_dir / "under-indented.md"
        doc.write_text(
            "# Under-indented Example\n\n"
            "```yaml\n"
            "- name: Test on MSRV\n"
            " uses: dtolnay/rust-toolchain@stable\n"
            " with:\n"
            "   toolchain: 1.85.0\n"
            " run: cargo test --all-features\n"
            "```\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        errors = validate_yaml_step_indentation([doc])
        assert len(errors) == 3
        assert "under-indented.md" in errors[0]
        assert "`uses:` is under-indented" in errors[0]
        assert "`with:` is under-indented" in errors[1]
        assert "`run:` is under-indented" in errors[2]

    def test_nested_with_name_mapping_passes(self, tmp_path, monkeypatch):
        """A plain nested `name:` under `with` should not be treated as a step key."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        llm_dir = fake_root / ".llm"
        llm_dir.mkdir()
        doc = llm_dir / "nested-with-name.md"
        doc.write_text(
            "# Nested with name\n\n"
            "```yaml\n"
            "- name: Build\n"
            "  uses: actions/cache@v4\n"
            "  with:\n"
            "    name: rust-cache\n"
            "    path: target\n"
            "- run: cargo test --all-features\n"
            "```\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        errors = validate_yaml_step_indentation([doc])
        assert errors == []

    def test_nested_list_under_with_does_not_reset_step_alignment(
        self,
        tmp_path,
        monkeypatch,
    ):
        """Nested list items under `with` should not change sibling step-key alignment."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        llm_dir = fake_root / ".llm"
        llm_dir.mkdir()
        doc = llm_dir / "nested-list-with.md"
        doc.write_text(
            "# Nested list under with\n\n"
            "```yaml\n"
            "- name: Build\n"
            "  uses: actions/example@v1\n"
            "  with:\n"
            "    include:\n"
            "      - linux\n"
            "      - macos\n"
            "  run: echo done\n"
            "```\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        errors = validate_yaml_step_indentation([doc])
        assert errors == []

    @pytest.mark.parametrize(
        "fence_open,fence_close",
        [
            ("~~~yaml", "~~~"),
            ("~~~YML", "~~~"),
        ],
    )
    def test_tilde_yaml_fence_is_validated(
        self,
        tmp_path,
        monkeypatch,
        fence_open,
        fence_close,
    ):
        """YAML in tilde fences should be validated the same as backticks."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        llm_dir = fake_root / ".llm"
        llm_dir.mkdir()
        doc = llm_dir / "tilde-yaml.md"
        doc.write_text(
            "# Tilde YAML\n\n"
            f"{fence_open}\n"
            "- name: Test on MSRV\n"
            "    uses: dtolnay/rust-toolchain@stable\n"
            f"{fence_close}\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        errors = validate_yaml_step_indentation([doc])
        assert len(errors) == 1
        assert "`uses:` is over-indented" in errors[0]

    @pytest.mark.parametrize(
        "fence_open",
        [
            "```yaml",
            "```YAML",
            "``` yml",
            "```   Yaml   ",
            "~~~ yaml",
        ],
    )
    def test_fence_language_case_and_spacing_variants_are_supported(
        self,
        tmp_path,
        monkeypatch,
        fence_open,
    ):
        """Fence language parsing should honor supported case/spacing variants."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        llm_dir = fake_root / ".llm"
        llm_dir.mkdir()
        doc = llm_dir / "fence-variants.md"
        close = "~~~" if fence_open.startswith("~~~") else "```"
        doc.write_text(
            "# Fence Variants\n\n"
            f"{fence_open}\n"
            "- name: Build\n"
            "    run: cargo test --all-features\n"
            f"{close}\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        errors = validate_yaml_step_indentation([doc])
        assert len(errors) == 1
        assert "`run:` is over-indented" in errors[0]

    def test_non_yaml_fence_is_ignored(self, tmp_path, monkeypatch):
        """Workflow-like snippets in non-yaml fences should not be checked."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        llm_dir = fake_root / ".llm"
        llm_dir.mkdir()
        doc = llm_dir / "shell-example.md"
        doc.write_text(
            "# Shell Example\n\n"
            "```bash\n"
            "- name: Not YAML\n"
            "    uses: dtolnay/rust-toolchain@stable\n"
            "```\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        errors = validate_yaml_step_indentation([doc])
        assert errors == []


# ===================================================================
# Tests for validate_doc_nav_card_consistency
# ===================================================================


class TestValidateDocNavCardConsistency:
    """Tests for the validate_doc_nav_card_consistency function."""

    def test_matching_labels_no_errors(self, tmp_path, monkeypatch):
        """When card labels match the target file H1 headings, no errors are returned."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        docs_dir = fake_root / "docs"
        docs_dir.mkdir()

        # Create a target page with an H1 heading
        (docs_dir / "getting-started.md").write_text(
            "# Getting Started\n\nSome content.\n",
            encoding="utf-8",
        )

        # Create docs/index.md with a matching nav card
        (docs_dir / "index.md").write_text(
            "# Home\n\n"
            "[:octicons-arrow-right-24: Getting Started](getting-started.md)\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        errors = validate_doc_nav_card_consistency()
        assert errors == []

    def test_mismatched_labels_reports_error(self, tmp_path, monkeypatch):
        """When a card label doesn't match the target H1, an error is reported."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        docs_dir = fake_root / "docs"
        docs_dir.mkdir()

        (docs_dir / "client.md").write_text(
            "# Client API\n\nReference docs.\n",
            encoding="utf-8",
        )

        (docs_dir / "index.md").write_text(
            "# Home\n\n"
            "[:octicons-arrow-right-24: Wrong Label](client.md)\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        errors = validate_doc_nav_card_consistency()
        assert len(errors) == 1
        assert 'Card label "Wrong Label"' in errors[0]
        assert '"Client API"' in errors[0]
        assert "client.md" in errors[0]

    def test_missing_target_file_reports_error(self, tmp_path, monkeypatch):
        """When the target .md file doesn't exist, an error is reported without crashing."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        docs_dir = fake_root / "docs"
        docs_dir.mkdir()

        (docs_dir / "index.md").write_text(
            "# Home\n\n"
            "[:octicons-arrow-right-24: Nonexistent](nonexistent.md)\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        errors = validate_doc_nav_card_consistency()
        assert len(errors) == 1
        assert "nonexistent.md" in errors[0]

    def test_external_url_cards_are_skipped(self, tmp_path, monkeypatch):
        """Cards pointing to external http:// URLs are silently skipped."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        docs_dir = fake_root / "docs"
        docs_dir.mkdir()

        # The regex only matches links ending in .md, so an http URL ending
        # in .md would be the edge case to verify.  The function also has an
        # explicit startswith("http") guard.
        (docs_dir / "index.md").write_text(
            "# Home\n\n"
            "[:octicons-arrow-right-24: docs.rs](https://docs.rs/signal-fish-client)\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        errors = validate_doc_nav_card_consistency()
        assert errors == []

    def test_missing_h1_heading_in_target(self, tmp_path, monkeypatch):
        """When the target file has no H1 heading, an error about the missing heading is reported."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        docs_dir = fake_root / "docs"
        docs_dir.mkdir()

        (docs_dir / "transport.md").write_text(
            "Some content without a heading.\n",
            encoding="utf-8",
        )

        (docs_dir / "index.md").write_text(
            "# Home\n\n"
            "[:octicons-arrow-right-24: Transport](transport.md)\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        errors = validate_doc_nav_card_consistency()
        assert len(errors) == 1
        assert "no H1 heading" in errors[0]
        assert "transport.md" in errors[0]

    def test_missing_docs_index_returns_empty(self, tmp_path, monkeypatch):
        """When docs/index.md doesn't exist, an empty list is returned."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        docs_dir = fake_root / "docs"
        docs_dir.mkdir()
        # No index.md created

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)

        errors = validate_doc_nav_card_consistency()
        assert errors == []


# ===================================================================
# Tests for validate_changelog_example_links
# ===================================================================


class TestValidateChangelogExampleLinks:
    """Tests for changelog example reference link consistency validation."""

    def test_consistent_unreleased_and_latest_release_links_pass(
        self,
        tmp_path,
        monkeypatch,
    ):
        """When links are aligned to latest version, no errors are returned."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        llm_dir = fake_root / ".llm"
        llm_dir.mkdir()
        doc = llm_dir / "skill.md"
        doc.write_text(
            "# Example\n\n"
            "## [Unreleased]\n\n"
            "## [0.2.0] - 2024-01-15\n\n"
            "[Unreleased]: https://github.com/example/project/compare/v0.2.0...HEAD\n"
            "[0.2.0]: https://github.com/example/project/releases/tag/v0.2.0\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)
        errors = validate_changelog_example_links([doc])
        assert errors == []

    def test_unreleased_compare_from_older_version_is_reported(
        self,
        tmp_path,
        monkeypatch,
    ):
        """Unreleased compare must start from the latest linked version."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        llm_dir = fake_root / ".llm"
        llm_dir.mkdir()
        doc = llm_dir / "skill.md"
        doc.write_text(
            "# Example\n\n"
            "[Unreleased]: https://github.com/example/project/compare/v0.1.0...HEAD\n"
            "[0.1.0]: https://github.com/example/project/releases/tag/v0.1.0\n"
            "[0.2.0]: https://github.com/example/project/releases/tag/v0.2.0\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)
        errors = validate_changelog_example_links([doc])
        assert len(errors) == 1
        assert "compares from v0.1.0" in errors[0]
        assert "latest linked version is 0.2.0" in errors[0]

    def test_latest_release_link_with_wrong_tag_is_reported(
        self,
        tmp_path,
        monkeypatch,
    ):
        """Latest version link must point to its own tag."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        llm_dir = fake_root / ".llm"
        llm_dir.mkdir()
        doc = llm_dir / "skill.md"
        doc.write_text(
            "# Example\n\n"
            "[Unreleased]: https://github.com/example/project/compare/v0.2.0...HEAD\n"
            "[0.2.0]: https://github.com/example/project/releases/tag/v0.1.0\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)
        errors = validate_changelog_example_links([doc])
        assert len(errors) == 1
        assert "[0.2.0] link points to v0.1.0" in errors[0]

    def test_non_changelog_files_are_ignored(self, tmp_path, monkeypatch):
        """Files without an Unreleased link are ignored."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        llm_dir = fake_root / ".llm"
        llm_dir.mkdir()
        doc = llm_dir / "skill.md"
        doc.write_text("# Example\n\nNo changelog links here.\n", encoding="utf-8")

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)
        errors = validate_changelog_example_links([doc])
        assert errors == []

    def test_relative_path_input_does_not_crash(self, tmp_path, monkeypatch):
        """Validator handles relative path inputs without raising ValueError."""
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        llm_dir = fake_root / ".llm"
        llm_dir.mkdir()
        doc = llm_dir / "skill.md"
        doc.write_text(
            "[Unreleased]: https://github.com/example/project/compare/v0.2.0...HEAD\n"
            "[0.2.0]: https://github.com/example/project/releases/tag/v0.1.0\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)
        with monkeypatch.context() as mp:
            mp.chdir(fake_root)
            errors = validate_changelog_example_links([Path(".llm/skill.md")])
        assert len(errors) == 1
        assert ".llm/skill.md" in errors[0]


# ===================================================================
# Tests for validate_unstable_feature_wording
# ===================================================================


class TestValidateUnstableFeatureWording:
    """Tests for stale release-specific unstable feature wording checks."""

    def test_doc_auto_cfg_with_version_specific_wording_fails(
        self,
        tmp_path,
        monkeypatch,
    ):
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        llm_dir = fake_root / ".llm"
        llm_dir.mkdir()
        skill = llm_dir / "skill.md"
        skill.write_text(
            "# Skill\n\n"
            "`doc_auto_cfg` was removed in Rust 1.92.\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)
        errors = validate_unstable_feature_wording([skill])
        assert len(errors) == 1
        assert "removed in Rust" in errors[0]

    def test_doc_auto_cfg_with_stable_wording_passes(self, tmp_path, monkeypatch):
        fake_root = tmp_path / "repo"
        fake_root.mkdir()
        llm_dir = fake_root / ".llm"
        llm_dir.mkdir()
        skill = llm_dir / "skill.md"
        skill.write_text(
            "# Skill\n\n"
            "`doc_auto_cfg` was removed from rustdoc.\n",
            encoding="utf-8",
        )

        monkeypatch.setattr(_mod, "REPO_ROOT", fake_root)
        errors = validate_unstable_feature_wording([skill])
        assert errors == []
