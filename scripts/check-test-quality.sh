#!/usr/bin/env bash
# check-test-quality.sh — Static analysis for common test quality anti-patterns
# in Rust source files.
#
# Checks:
#   1. Mutable references to temporaries: `&mut false`, `&mut true`,
#      `&mut 0`, `&mut 1`. The mutation is silently discarded because
#      the temporary is dropped at the end of the statement. This is
#      almost always a bug — the author intended to pass a mutable
#      reference to a *variable*, not a literal.
#
# Skips:
#   - Lines that are comments (// or /// after trimming)
#   - Lines where the match appears inside a string literal (best effort)
#
# Exit codes:
#   0 — no violations found
#   1 — one or more violations detected
#
# Usage:
#   bash scripts/check-test-quality.sh

set -euo pipefail

# ── Resolve paths relative to this script ────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

VIOLATIONS=0

echo -e "${YELLOW}=== Test quality check ===${NC}"
echo ""

# ── Check 1: Mutable references to temporaries ──────────────────────
# `&mut false`, `&mut true`, `&mut 0`, `&mut 1` create mutable references
# to temporaries. Any mutation through the reference is silently discarded.
echo -e "${YELLOW}Check 1: Scanning for mutable references to temporaries (&mut false, &mut true, etc.)...${NC}"

CHECK1_VIOLATIONS=0

# Pattern matches &mut followed by a boolean or small integer literal.
# The word-boundary after the literal prevents matching things like
# `&mut true_count` or `&mut 100`.
PATTERN='&mut (false|true|0|1)[^A-Za-z0-9_]|&mut (false|true|0|1)$'

MATCHES=$(grep -rnE "$PATTERN" src/ tests/ 2>/dev/null \
    | grep -E '\.rs:' \
    || true)

if [ -z "$MATCHES" ]; then
    echo -e "${GREEN}  Check 1: PASS — no mutable references to temporaries found.${NC}"
else
    while IFS= read -r match; do
        [ -z "$match" ] && continue

        file_and_line="${match%%:*}"
        rest="${match#*:}"
        lineno="${rest%%:*}"
        content="${rest#*:}"

        # Strip leading whitespace for display and comment detection
        stripped="${content#"${content%%[![:space:]]*}"}"

        # Skip comment lines (// or ///)
        case "$stripped" in
            //*) continue ;;
        esac

        # Skip lines where the pattern appears inside a string literal (best effort).
        # If the match is preceded by an odd number of unescaped quotes on the line,
        # it is likely inside a string. We use a simple heuristic: strip escaped
        # quotes (\") first, then count the remaining double-quote characters
        # before the first occurrence of &mut.
        before_match="${content%%&mut*}"
        # Remove escaped quotes so they don't throw off the count
        unescaped="${before_match//\\\"/}"
        # Note: strings ending with \\" (escaped backslash + closing quote)
        # may still be miscounted — an acceptable tradeoff for a best-effort heuristic.
        # Count unescaped double quotes (remove everything except quotes, then measure length)
        quotes_only="${unescaped//[!\"]/}"
        quote_count=${#quotes_only}
        if [ $((quote_count % 2)) -ne 0 ]; then
            continue
        fi

        echo -e "${RED}VIOLATION:${NC} $file_and_line:$lineno: mutable reference to temporary"
        echo "  $stripped"
        echo "  \`&mut <literal>\` creates a reference to a temporary — mutations are silently discarded."
        echo "  Assign the value to a variable first: \`let mut val = false; f(&mut val);\`"
        CHECK1_VIOLATIONS=$((CHECK1_VIOLATIONS + 1))
    done <<< "$MATCHES"

    if [ "$CHECK1_VIOLATIONS" -eq 0 ]; then
        echo -e "${GREEN}  Check 1: PASS — no mutable references to temporaries found.${NC}"
    fi
fi

VIOLATIONS=$((VIOLATIONS + CHECK1_VIOLATIONS))
echo ""

# ── Result ────────────────────────────────────────────────────────────
if [ "$VIOLATIONS" -gt 0 ]; then
    echo -e "${RED}FAILED: $VIOLATIONS test quality violation(s) found.${NC}"
    echo "Fix all violations before committing."
    exit 1
else
    echo -e "${GREEN}PASSED: No test quality issues found.${NC}"
    exit 0
fi
