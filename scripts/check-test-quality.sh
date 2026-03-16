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

# Returns success when the match position is likely inside a double-quoted
# string literal on the same line (best effort).
is_inside_string_literal() {
    local line="$1"
    local match_start="$2"
    local before_match unescaped quotes_only quote_count

    before_match="${line:0:match_start}"
    # Remove escaped quotes so they do not affect quote parity.
    unescaped="${before_match//\\\"/}"
    quotes_only="${unescaped//[!\"]/}"
    quote_count=${#quotes_only}

    [ $((quote_count % 2)) -ne 0 ]
}

echo -e "${YELLOW}=== Test quality check ===${NC}"
echo ""

# ── Check 1: Mutable references to temporaries ──────────────────────
# `&mut false`, `&mut true`, `&mut 0`, `&mut 1` create mutable references
# to temporaries. Any mutation through the reference is silently discarded.
echo -e "${YELLOW}Check 1: Scanning for mutable references to temporaries (&mut false, &mut true, etc.)...${NC}"

CHECK1_VIOLATIONS=0

# Pattern matches `&mut` followed by a boolean or small integer literal.
# The non-identifier boundary after the literal prevents matching things
# like `&mut true_count` or `&mut 100`.
PATTERN='&mut[[:space:]]+(false|true|0|1)([^A-Za-z0-9_]|$)'

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

        # Process every match on the line. Do not decide based on the first
        # occurrence only, or later real violations can be missed.
        remaining="$content"
        offset=0
        while [[ "$remaining" =~ $PATTERN ]]; do
            matched="${BASH_REMATCH[0]}"
            # Use the matched substring to find the first occurrence in the
            # remaining text so we can map match positions back to the full line.
            prefix="${remaining%%"$matched"*}"
            match_start=$((offset + ${#prefix}))
            advance=$(( ${#prefix} + ${#matched} ))

            if is_inside_string_literal "$content" "$match_start"; then
                remaining="${remaining:$advance}"
                offset=$((offset + advance))
                continue
            fi

            echo -e "${RED}VIOLATION:${NC} $file_and_line:$lineno: mutable reference to temporary"
            echo "  $stripped"
            echo "  \`&mut <literal>\` creates a reference to a temporary — mutations are silently discarded."
            echo "  Assign the value to a variable first: \`let mut val = false; f(&mut val);\`"
            CHECK1_VIOLATIONS=$((CHECK1_VIOLATIONS + 1))

            # Advance both the remaining slice and absolute offset to continue
            # scanning subsequent matches on the same source line.
            remaining="${remaining:$advance}"
            offset=$((offset + advance))
        done
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
