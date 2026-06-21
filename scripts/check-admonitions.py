#!/usr/bin/env python3
"""Validate pymdownx admonition / details titles are well-formed.

MkDocs Material admonition (`!!!`) and collapsible-details (`???` / `???+`)
blocks take an optional title delimited by double quotes:

    !!! warning "Title"
    ???+ note "Collapsible title"

The title runs from the FIRST double quote to its matching close, so a literal
`"` *inside* the title (for example an embedded JSON envelope `{ "type": ... }`)
closes the title early and the rest of the line leaks into the rendered page as
raw text. `mkdocs build --strict` does NOT catch this — the block still
"builds", it just renders wrong — so this guard catches it at commit time and in
CI before it reaches the published site.

The rule is deliberately simple and robust: the text following an admonition /
details marker must contain exactly zero or two double quotes. Zero = no title;
two = a single well-formed `"..."` title. Any other count is an embedded or
unbalanced quote that will mis-render. Backticks (inline code such as
`!!! tip "the `?` operator"`) are fine — only double quotes delimit the title.

Fenced code blocks are skipped: an admonition shown *inside* a ```` ```markdown ````
example is documentation, not a real admonition. Backtick and tilde fences are
tracked independently and honor fence length per CommonMark — a ```` ``` ```` never
closes a ```` ```` ```` fence — so a fenced example that itself shows shorter fences
is handled correctly. (Indented 4-space code blocks are not modeled: a literal
fence marker inside one is treated as a fence. This is the safe default, since
admonition bodies legitimately indent their own fenced code.)

Usage:
    python3 scripts/check-admonitions.py [PATH ...]

With no PATH arguments it scans the MkDocs documentation root (docs/), which is
the only tree MkDocs renders. Pass explicit files or directories to scan others.
Exit code 0 = clean, 1 = malformed titles found.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# Directories never worth scanning (generated, vendored, or VCS internals).
SKIP_DIRS = {".git", "target", "site", "node_modules", "__pycache__", ".venv"}

# Default root: the MkDocs site. Only docs/ is rendered, so only docs/ can
# suffer the rendering bug.
DEFAULT_ROOTS = ("docs",)

# Block markers that may carry a `"..."` title.
ADMONITION_MARKERS = ("!!!", "???")


# A fenced-code-block delimiter: optional indent, a run of >= 3 backticks or
# tildes, then an optional info string. The run LENGTH matters (CommonMark): a
# closing fence uses the same character and is at least as long as the opener,
# with no info string of its own.
_FENCE_RE = re.compile(r"^[ \t]*(?P<run>`{3,}|~{3,})(?P<info>.*)$")


def admonition_quote_errors(text: str, path: str) -> list[str]:
    """Return human-readable errors for malformed admonition titles in `text`.

    Fence-aware: lines inside a fenced code block are ignored so documented
    *example* admonitions never trip the check.
    """
    errors: list[str] = []
    open_fence: tuple[str, int] | None = None  # (char, run length) when inside a fence
    for lineno, line in enumerate(text.splitlines(), start=1):
        fence = _FENCE_RE.match(line)
        if fence is not None:
            run = fence.group("run")
            char, length = run[0], len(run)
            if open_fence is None:
                open_fence = (char, length)  # open a fence
            elif (
                char == open_fence[0]
                and length >= open_fence[1]
                and not fence.group("info").strip()
            ):
                open_fence = None            # close: same char, >= opener length, no info
            continue                         # a fence delimiter is never an admonition
        if open_fence is not None:
            continue                         # inside a fence — skip everything

        stripped = line.strip()
        marker = next((m for m in ADMONITION_MARKERS if stripped.startswith(m)), None)
        if marker is None:
            continue
        rest = stripped[len(marker):]
        if marker == "???" and rest.startswith("+"):
            rest = rest[1:]                 # collapsible-expanded `???+`
        # A real opener is `marker <type> ["title"]` — the marker is followed by
        # whitespace. `!!!foo` or a bare marker is not an admonition.
        if not rest[:1].isspace():
            continue

        quotes = rest.count('"')
        if quotes not in (0, 2):
            errors.append(
                f"{path}:{lineno}: admonition/details title has {quotes} double "
                f"quotes (expected 0 or 2); an embedded or unbalanced quote "
                f"closes the title early and leaks raw text.\n    {line.rstrip()}"
            )
    return errors


def iter_markdown_files(roots: list[Path]) -> list[Path]:
    """Expand `roots` (files or directories) into a sorted list of .md files."""
    files: list[Path] = []
    seen: set[Path] = set()
    for root in roots:
        candidates: list[Path]
        if root.is_file():
            candidates = [root] if root.suffix == ".md" else []
        elif root.is_dir():
            candidates = sorted(root.rglob("*.md"))
        else:
            print(f"warning: path not found, skipping: {root}", file=sys.stderr)
            candidates = []
        for path in candidates:
            if any(part in SKIP_DIRS for part in path.parts):
                continue
            resolved = path.resolve()
            if resolved in seen:
                continue
            seen.add(resolved)
            files.append(path)
    return files


def resolve_roots(args: list[str]) -> list[Path]:
    if args:
        return [Path(a) for a in args]
    return [REPO_ROOT / r for r in DEFAULT_ROOTS]


def main(argv: list[str]) -> int:
    files = iter_markdown_files(resolve_roots(argv))
    all_errors: list[str] = []
    for path in files:
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as exc:
            print(f"warning: could not read {path}: {exc}", file=sys.stderr)
            continue
        try:
            display = str(path.relative_to(REPO_ROOT))
        except ValueError:
            display = str(path)
        all_errors.extend(admonition_quote_errors(text, display))

    if all_errors:
        print("Malformed MkDocs admonition/details titles found:\n", file=sys.stderr)
        for err in all_errors:
            print(f"  {err}", file=sys.stderr)
        print(
            "\nA double quote inside a \"...\"-delimited admonition title closes it "
            "early and leaks the rest as raw text. Rephrase with backticks "
            "(e.g. !!! note \"the `type` key\") or drop the inner quotes.",
            file=sys.stderr,
        )
        return 1

    print(f"OK: {len(files)} file(s) checked, all admonition/details titles well-formed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
