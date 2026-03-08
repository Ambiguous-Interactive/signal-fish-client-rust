"""MkDocs hook: normalize rustdoc code-fence annotations for Pygments.

Rustdoc uses ``rust,ignore``, ``rust,no_run``, ``rust,compile_fail``, and
``rust,edition20XX`` as code-fence language tags.  These are meaningful to
``rustdoc`` and to the project's ``extract-rust-snippets.sh`` validator, but
Pygments (used by pymdownx.highlight) does not recognize them and falls back
to plain-text rendering — which also breaks the fence parser and corrupts
everything after the block.

This hook strips the ``,<annotation>`` suffix so Pygments receives ``rust``
and applies correct syntax highlighting.  The source markdown files are
**not** modified; the transformation is applied only during the MkDocs build.
"""

from __future__ import annotations

import re

# Matches ```rust,<annotation> at the start of a line.
# Captures the leading whitespace + backticks + "rust" and discards the rest.
_RUSTDOC_FENCE_RE = re.compile(
    r"^(?P<fence>\s*`{3,})\s*rust\s*(?:,\s*\w+)+",
    re.MULTILINE,
)


def on_page_markdown(markdown: str, **kwargs) -> str:  # noqa: ANN003
    """Replace ``rust,<annotation>`` fences with plain ``rust``."""
    return _RUSTDOC_FENCE_RE.sub(r"\g<fence>rust", markdown)
