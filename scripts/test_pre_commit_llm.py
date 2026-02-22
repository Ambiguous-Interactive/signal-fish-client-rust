"""Tests for extract_first_paragraph, extract_title, and generate_index in pre-commit-llm.py."""

import importlib.util
import re
import sys
from pathlib import Path

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
