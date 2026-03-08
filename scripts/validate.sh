#!/usr/bin/env bash
# validate.sh — Local pre-flight validation script for signal-fish-client
#
# Runs the mandatory cargo checks from CLAUDE.md plus additional config
# validations to catch issues before they reach CI.
#
# Usage:
#   bash scripts/validate.sh
#
# Exit codes:
#   0 — all checks passed
#   1 — one or more checks failed

set -euo pipefail

# Resolve repo root from script location
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

FAILED=0

section() {
    echo ""
    echo "========================================"
    echo "  $1"
    echo "========================================"
    echo ""
}

pass() {
    echo "[PASS] $1"
}

fail() {
    echo "[FAIL] $1"
    FAILED=1
}

skip() {
    echo "[SKIP] $1 (not installed)"
}

# ──────────────────────────────────────────────────────────────
# 1. Cargo fmt
# ──────────────────────────────────────────────────────────────
section "cargo fmt --check"
if cargo fmt --check; then
    pass "Formatting is correct"
else
    fail "cargo fmt found formatting issues — run 'cargo fmt' to fix"
fi

# ──────────────────────────────────────────────────────────────
# 2. Cargo clippy (all targets, all features, deny warnings)
# ──────────────────────────────────────────────────────────────
section "cargo clippy --all-targets --all-features -- -D warnings"
if cargo clippy --all-targets --all-features -- -D warnings; then
    pass "Clippy is clean"
else
    fail "Clippy reported warnings or errors"
fi

# ──────────────────────────────────────────────────────────────
# 3. Cargo test (all features)
# ──────────────────────────────────────────────────────────────
section "cargo test --all-features"
if cargo test --all-features; then
    pass "All tests passed"
else
    fail "Tests failed"
fi

# ──────────────────────────────────────────────────────────────
# 4. lychee.toml header format validation
# ──────────────────────────────────────────────────────────────
section "Lychee config validation"

LYCHEE_TOML="$REPO_ROOT/.lychee.toml"
if [ -f "$LYCHEE_TOML" ]; then
    # lychee TOML uses key=value syntax. A common mistake is using YAML-style
    # "key: value" for top-level assignments (not inside strings).
    # We check for lines that look like bare "key: value" assignments
    # (not inside quotes, not comments, not inside inline strings).
    # Correct TOML: key = value or key = "value"
    # Wrong:        key: value (YAML syntax)
    #
    # Check specifically for [header] section (map syntax) which lychee rejects.
    if grep -qE '^\[header\]' "$LYCHEE_TOML"; then
        fail ".lychee.toml uses [header] map syntax — lychee requires header = [\"key=value\"] array format"
    else
        pass ".lychee.toml does not use [header] map syntax"
    fi

    # Verify header field uses = assignment (not : assignment at top level)
    # This catches lines like "header: [...]" instead of "header = [...]"
    if grep -qE '^[a-z_]+[[:space:]]*:' "$LYCHEE_TOML"; then
        fail ".lychee.toml has YAML-style 'key: value' lines — TOML requires 'key = value'"
    else
        pass ".lychee.toml uses correct TOML key=value format"
    fi

    # Verify header entries inside the array use key=value (not key: value)
    # lychee v0.18+ rejects "key: value" and requires "key=value"
    if grep -E '^header[[:space:]]*=' "$LYCHEE_TOML" | grep -qE '"[^"]*[^=]*:[[:space:]]'; then
        fail ".lychee.toml header entries use 'key: value' — lychee requires 'key=value' (equals, not colon)"
    else
        pass ".lychee.toml header entries use correct key=value format"
    fi
else
    skip ".lychee.toml not found"
fi

# ──────────────────────────────────────────────────────────────
# 5. Markdown lint (if markdownlint-cli2 is available)
# ──────────────────────────────────────────────────────────────
section "Markdown lint"

if command -v markdownlint-cli2 >/dev/null 2>&1; then
    if markdownlint-cli2 "**/*.md"; then
        pass "Markdown lint passed"
    else
        fail "Markdown lint reported issues"
    fi
else
    skip "markdownlint-cli2"
fi

# ──────────────────────────────────────────────────────────────
# Summary
# ──────────────────────────────────────────────────────────────
section "Summary"

if [ "$FAILED" -eq 0 ]; then
    echo "All checks passed."
    exit 0
else
    echo "One or more checks failed. Review output above."
    exit 1
fi
