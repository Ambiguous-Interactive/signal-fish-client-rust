"""Tests for extract_first_paragraph and extract_title in pre-commit-llm.py."""

import importlib.util
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
