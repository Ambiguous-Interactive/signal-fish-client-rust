"""MkDocs hook: normalize rustdoc code-fence annotations for Pygments.

Rustdoc uses ``rust,ignore``, ``rust,no_run``, ``rust,compile_fail``, and
``rust,edition20XX`` as code-fence info strings.  These are meaningful to
``rustdoc`` and to the project's ``extract-rust-snippets.sh`` validator, but
Pygments (used by pymdownx.highlight) does not recognize them and falls back
to plain-text rendering — which also breaks the fence parser and corrupts
everything after the block.

This hook rewrites such info strings to plain ``rust`` so Pygments applies
correct syntax highlighting.  The source markdown files are **not** modified;
the transformation is applied only during the MkDocs build.

The rewrite walks the page with a fence state machine instead of a global
regex, so it is fence-aware in every direction that previously leaked:

- both fence characters (backticks and tildes) are understood;
- a fence line is only rewritten while *outside* any code block, so display
  fences (markdown showing markdown) pass through verbatim;
- info strings are matched exactly (``rust`` plus comma-separated word
  annotations, trailing whitespace allowed) — prose like ``rust,ignore is a
  rustdoc tag`` or a bare `` ``` `` fence is never touched, and trailing
  garbage after a valid info string is left for the rendering checks to
  flag rather than silently rewritten.
"""

from __future__ import annotations

import re

# An opening fence: up to three spaces of indent, then 3+ backticks or
# tildes, then an info string. CommonMark requires a closing fence to use
# the same character and at least the same length, and to carry no info.
_FENCE_RE = re.compile(r"^(?P<indent>[ ]{0,3})(?P<mark>`{3,}|~{3,})[ \t]*(?P<info>.*)$")

# A rustdoc info string: "rust" plus at least one comma-separated word
# annotation (``rust,ignore``, ``rust,no_run,edition2021``, ...).
_RUSTDOC_INFO_RE = re.compile(r"^rust(?:[ \t]*,[ \t]*\w+)+[ \t]*$")


def _rewrite_opening(line: str, match: re.Match[str]) -> str:
    """Return the opening-fence line with a rustdoc info string normalized."""
    info = match.group("info")
    if _RUSTDOC_INFO_RE.match(info):
        return f"{match.group('indent')}{match.group('mark')}rust"
    return line


def on_page_markdown(markdown: str, **kwargs) -> str:  # noqa: ANN003
    """Rewrite ``rust,<annotation>`` fence info strings to plain ``rust``."""
    out: list[str] = []
    open_mark: str | None = None  # fence character and length of the open block
    for line in markdown.split("\n"):
        match = _FENCE_RE.match(line)
        if open_mark is None:
            if match:
                mark = match.group("mark")
                open_mark = mark
                out.append(_rewrite_opening(line, match))
            else:
                out.append(line)
        else:
            out.append(line)
            if (
                match
                and match.group("mark")[0] == open_mark[0]
                and len(match.group("mark")) >= len(open_mark)
                and match.group("info").strip() == ""
            ):
                open_mark = None
    return "\n".join(out)
