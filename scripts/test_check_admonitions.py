#!/usr/bin/env python3
"""Self-tests for scripts/check-admonitions.py.

Plain-Python (no pytest dependency) so the pre-commit hook can run it anywhere
`python3` exists. Exit 0 = all pass, 1 = a self-test failed.

Run:
    python3 scripts/test_check_admonitions.py
"""

from __future__ import annotations

import importlib.util
import sys
import tempfile
from pathlib import Path

_SPEC = importlib.util.spec_from_file_location(
    "check_admonitions", Path(__file__).resolve().parent / "check-admonitions.py"
)
assert _SPEC is not None and _SPEC.loader is not None
check = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(check)

# (name, markdown, should_be_flagged)
CASES: list[tuple[str, str, bool]] = [
    # ── RED: embedded / unbalanced quotes that render incorrectly ──────
    (
        "embedded JSON envelope (the real Copilot bug)",
        '!!! warning "External tagging, not the `{ "type": ..., "data": ... }` envelope"',
        True,
    ),
    ("nested quoted phrase", '!!! note "He said "hi" today"', True),
    ("unbalanced single quote", '??? example "unterminated', True),
    ("collapsible-expanded embedded quote", '???+ tip "use the "x" flag"', True),
    ("indented admonition with embedded quote", '    !!! note "a "b" c"', True),
    # ── GREEN: well-formed titles ──────────────────────────────────────
    ("plain title", '!!! note "A plain title"', False),
    ("backticked code in title", '!!! tip "The `?` operator works naturally"', False),
    ("backticked envelope (the fixed form)", '!!! warning "not the `{ type, data }` envelope"', False),
    ("no title", "!!! note", False),
    ("collapsible no title", "???+ example", False),
    ("collapsible with backticked title", '???+ example "JSON — `Direct` variant"', False),
    ("apostrophe in title is fine", '!!! note "the client\'s view"', False),
    ("inline admonition (multi-token type)", '!!! note inline end "A title"', False),
    ("multi-class admonition", '!!! danger highlight "A title"', False),
    ("prose with quotes is not an admonition", 'Body text mentioning "quotes" inline.', False),
    ("bare marker is not an opener", "!!!", False),
    ("glued marker is not an opener", '!!!note "x" "y"', False),
    # ── CRLF portability: a trailing \r must not hide an embedded quote ──
    ("embedded quote with CRLF endings", '!!! warning "a "b" c"\r\n', True),
    ("well-formed title with CRLF endings", '!!! note "ok"\r\n', False),
    # ── GREEN: fence-awareness — example admonitions inside code fences ─
    (
        "admonition inside backtick fence is ignored",
        '```markdown\n!!! warning "the `{ "type": ... }` envelope"\n```',
        False,
    ),
    (
        "admonition inside tilde fence is ignored",
        '~~~markdown\n!!! warning "bad "quotes" here"\n~~~',
        False,
    ),
    (
        "tilde fence is not closed by backticks",
        '~~~\n```\n!!! warning "still inside the tilde fence "x""\n~~~',
        False,
    ),
    (
        "4-backtick wrapper holding a 3-backtick example is ignored",
        '````markdown\n```\n!!! warning "the `{ "type": ... }` envelope"\n```\n````',
        False,
    ),
    (
        "longer run closes a shorter fence; the example inside stays ignored",
        '```\n!!! note "broken "title" inside the fence"\n`````\nplain prose',
        False,
    ),
]


def run() -> int:
    failures = 0
    for name, text, should_flag in CASES:
        errs = check.admonition_quote_errors(text, "test.md")
        flagged = bool(errs)
        if flagged != should_flag:
            failures += 1
            print(f"FAIL: {name}: expected flagged={should_flag}, got {flagged}: {errs}")
        else:
            print(f"ok: {name}")

    # Regression: the real docs tree must be clean after the protocol.md fix.
    docs = check.REPO_ROOT / "docs"
    if docs.is_dir():
        repo_errs: list[str] = []
        for path in check.iter_markdown_files([docs]):
            text = path.read_text(encoding="utf-8", errors="replace")
            repo_errs.extend(check.admonition_quote_errors(text, str(path)))
        if repo_errs:
            failures += 1
            print("FAIL: docs/ must be clean after the fix, got:\n  " + "\n  ".join(repo_errs))
        else:
            print("ok: docs/ tree is clean")

    # A non-UTF8 .md must be skipped gracefully, not crash main()
    # (UnicodeDecodeError is not an OSError).
    with tempfile.TemporaryDirectory() as tmp:
        binmd = Path(tmp) / "bin.md"
        binmd.write_bytes(b"\xff\xfe not utf-8 \x00\x80")
        try:
            rc = check.main([str(binmd)])
        except Exception as exc:
            failures += 1
            print(f"FAIL: non-UTF8 file must not raise, got {exc!r}")
        else:
            if rc != 0:
                failures += 1
                print(f"FAIL: non-UTF8 file should be skipped (rc 0), got rc={rc}")
            else:
                print("ok: non-UTF8 file handled gracefully")

    if failures:
        print(f"\n{failures} self-test(s) FAILED.")
        return 1
    print(f"\nAll {len(CASES)} admonition self-tests passed (+ docs/ regression).")
    return 0


if __name__ == "__main__":
    raise SystemExit(run())
