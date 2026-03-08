#!/usr/bin/env bash
# =============================================================================
# validate-docs.sh — Lightweight documentation validation for signal-fish-client
#
# Checks:
#   1. Spell check via typos (optional — skips if not installed)
#   2. mkdocs.yml nav validation — every .md file referenced in nav: exists
#      in the docs/ directory
#
# Usage:
#   bash scripts/validate-docs.sh
#
#
# Exit codes:
#   0 — all checks passed
#   1 — one or more checks failed
# =============================================================================

set -euo pipefail

# ── Resolve paths relative to this script ────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

FAILED=0

pass() {
    echo "[PASS] $1"
}

fail() {
    echo "[FAIL] $1"
    FAILED=1
}

skip() {
    echo "[SKIP] $1"
}

# ── 1. Spell check (typos) ──────────────────────────────────────────
echo ""
echo "── Spell check (typos) ──────────────────────────────────"
echo ""

if command -v typos &>/dev/null; then
    if [ -f "$REPO_ROOT/.typos.toml" ]; then
        if typos --config "$REPO_ROOT/.typos.toml"; then
            pass "typos spell check"
        else
            fail "typos found spelling errors (fix them or update .typos.toml)"
        fi
    else
        skip "typos — .typos.toml not found"
    fi
else
    skip "typos is not installed (install: cargo install typos-cli)"
fi

# ── 2. mkdocs.yml nav file validation ───────────────────────────────
echo ""
echo "── mkdocs.yml nav file validation ───────────────────────"
echo ""

MKDOCS_YML="$REPO_ROOT/mkdocs.yml"

if [ ! -f "$MKDOCS_YML" ]; then
    skip "mkdocs.yml not found — nothing to validate"
else
    NAV_ERRORS=0

    # Extract .md filenames from the nav: section of mkdocs.yml.
    # Lines in nav look like:  "  - Title: filename.md" or "  - filename.md"
    # We grep for any line containing a .md reference and extract the filename.
    while IFS= read -r md_file; do
        # Skip empty lines (shouldn't happen, but be safe)
        [ -z "$md_file" ] && continue

        if [ ! -f "$REPO_ROOT/docs/$md_file" ]; then
            echo "  MISSING: docs/$md_file (referenced in mkdocs.yml nav)"
            NAV_ERRORS=$((NAV_ERRORS + 1))
        fi
    done < <(grep -oE '[a-zA-Z0-9_/.-]+\.md' "$MKDOCS_YML" | grep -v '^#' | grep -v 'includes/' | sort -u)

    if [ "$NAV_ERRORS" -gt 0 ]; then
        fail "mkdocs.yml nav references $NAV_ERRORS missing file(s) in docs/"
    else
        pass "All files referenced in mkdocs.yml nav exist in docs/"
    fi
fi

# ── Summary ──────────────────────────────────────────────────────────
echo ""
echo "── Summary ────────────────────────────────────────────────"
echo ""

if [ "$FAILED" -eq 0 ]; then
    echo "All docs validation checks passed."
    exit 0
else
    echo "One or more docs validation checks failed. Review output above."
    exit 1
fi
