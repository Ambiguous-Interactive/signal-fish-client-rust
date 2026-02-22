#!/usr/bin/env bash
# ci-validate.sh — Local CI validation script for signal-fish-client.
#
# Run this before pushing to catch the same issues that CI checks for.
# This is a lightweight alternative to scripts/check-all.sh that focuses
# on the core checks most likely to fail in CI.
#
# Usage:
#   bash scripts/ci-validate.sh
#
# Checks:
#   1. cargo fmt --check          (formatting)
#   2. cargo clippy                (linting)
#   3. cargo test                  (tests)
#   4. typos spell check           (optional — skipped if typos not installed)
#   5. .lychee.toml syntax         (TOML validity)
#   6. .markdownlint.json syntax   (JSON validity)
#   7. shellcheck on scripts/*.sh  (optional — skipped if shellcheck not installed)
#   8. markdownlint on *.md        (optional — skipped if markdownlint not installed)
#   9. mkdocs nav validation       (all nav-referenced files exist in docs/)
#
# Exit codes:
#   0 — all checks passed (or optional checks skipped)
#   1 — one or more checks failed

set -euo pipefail

# ── Resolve paths ────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# ── Color constants ──────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BOLD='\033[1m'
NC='\033[0m' # No Color

# ── State tracking ───────────────────────────────────────────────────
FAILURES=0
TOTAL_CHECKS=9
PASSED=0
SKIPPED=0

# ── Helper functions ─────────────────────────────────────────────────
section_header() {
    local step="$1"
    local title="$2"
    echo ""
    echo -e "${BOLD}${YELLOW}[$step/$TOTAL_CHECKS] $title${NC}"
    echo "────────────────────────────────────────────────────────────"
}

pass() {
    local label="$1"
    echo -e "${GREEN}PASS${NC}: $label"
    PASSED=$((PASSED + 1))
}

fail() {
    local label="$1"
    echo -e "${RED}FAIL${NC}: $label"
    FAILURES=$((FAILURES + 1))
}

skip() {
    local label="$1"
    local reason="$2"
    echo -e "${YELLOW}SKIP${NC}: $label — $reason"
    SKIPPED=$((SKIPPED + 1))
}

# ── Banner ───────────────────────────────────────────────────────────
echo -e "${BOLD}${YELLOW}=== signal-fish-client: CI validation ===${NC}"
echo "Running the same checks that CI enforces..."

# ── Check 1: cargo fmt ───────────────────────────────────────────────
section_header 1 "Formatting (cargo fmt --check)"

if ! command -v cargo &>/dev/null; then
    echo -e "${RED}ERROR: cargo is not installed. Install Rust via https://rustup.rs${NC}" >&2
    exit 1
fi

if cargo fmt --check 2>&1; then
    pass "Code formatting is correct"
else
    fail "Code formatting issues found. Run 'cargo fmt' to fix."
fi

# ── Check 2: cargo clippy ───────────────────────────────────────────
section_header 2 "Linting (cargo clippy --all-targets --all-features -- -D warnings)"

if cargo clippy --all-targets --all-features -- -D warnings 2>&1; then
    pass "No clippy warnings"
else
    fail "Clippy reported warnings or errors"
fi

# ── Check 3: cargo test ─────────────────────────────────────────────
section_header 3 "Tests (cargo test --all-features)"

if cargo test --all-features 2>&1; then
    pass "All tests passed"
else
    fail "One or more tests failed"
fi

# ── Check 4: Spell check (typos) ────────────────────────────────────
section_header 4 "Spell check (typos)"

if ! command -v typos &>/dev/null; then
    skip "Spell check" "typos is not installed (install: cargo install typos-cli)"
else
    if [ -f "$REPO_ROOT/.typos.toml" ]; then
        if typos --config "$REPO_ROOT/.typos.toml" 2>&1; then
            pass "No spelling errors found"
        else
            fail "Spelling errors detected. Run 'typos --config .typos.toml' to see details."
        fi
    else
        skip "Spell check" ".typos.toml config not found"
    fi
fi

# ── Check 5: .lychee.toml validity ──────────────────────────────────
section_header 5 "Config validation (.lychee.toml — TOML syntax)"

LYCHEE_TOML="$REPO_ROOT/.lychee.toml"
if [ ! -f "$LYCHEE_TOML" ]; then
    skip ".lychee.toml check" "file not found"
else
    # Try Python first (most likely available), then fall back to taplo or cargo toml-cli
    TOML_VALIDATED=false

    if command -v python3 &>/dev/null; then
        # Python 3.11+ has tomllib in the standard library
        # Exit 0 = valid TOML, exit 2 = no TOML parser available, exit 1 = invalid TOML
        TOML_EXIT=0
        python3 -c "
import sys
try:
    import tomllib
except ImportError:
    try:
        import tomli as tomllib
    except ImportError:
        sys.exit(2)
with open('$LYCHEE_TOML', 'rb') as f:
    tomllib.load(f)
" 2>/dev/null || TOML_EXIT=$?

        if [ "$TOML_EXIT" -eq 0 ]; then
            pass ".lychee.toml is valid TOML"
            TOML_VALIDATED=true
        elif [ "$TOML_EXIT" -eq 2 ]; then
            # Neither tomllib nor tomli available, try other methods
            :
        else
            fail ".lychee.toml contains invalid TOML syntax"
            TOML_VALIDATED=true
        fi
    fi

    if [ "$TOML_VALIDATED" = false ] && command -v taplo &>/dev/null; then
        if taplo check "$LYCHEE_TOML" 2>&1; then
            pass ".lychee.toml is valid TOML"
            TOML_VALIDATED=true
        else
            fail ".lychee.toml contains invalid TOML syntax"
            TOML_VALIDATED=true
        fi
    fi

    if [ "$TOML_VALIDATED" = false ]; then
        skip ".lychee.toml TOML check" "no TOML validator available (need python3 with tomllib/tomli, or taplo)"
    fi
fi

# ── Check 6: .markdownlint.json validity ────────────────────────────
section_header 6 "Config validation (.markdownlint.json — JSON syntax)"

MDLINT_JSON="$REPO_ROOT/.markdownlint.json"
if [ ! -f "$MDLINT_JSON" ]; then
    skip ".markdownlint.json check" "file not found"
else
    JSON_VALIDATED=false

    if command -v python3 &>/dev/null; then
        if python3 -c "import json; json.load(open('$MDLINT_JSON'))" 2>/dev/null; then
            pass ".markdownlint.json is valid JSON"
            JSON_VALIDATED=true
        else
            fail ".markdownlint.json contains invalid JSON syntax"
            JSON_VALIDATED=true
        fi
    fi

    if [ "$JSON_VALIDATED" = false ] && command -v jq &>/dev/null; then
        if jq empty "$MDLINT_JSON" 2>/dev/null; then
            pass ".markdownlint.json is valid JSON"
            JSON_VALIDATED=true
        else
            fail ".markdownlint.json contains invalid JSON syntax"
            JSON_VALIDATED=true
        fi
    fi

    if [ "$JSON_VALIDATED" = false ] && command -v node &>/dev/null; then
        if node -e "JSON.parse(require('fs').readFileSync('$MDLINT_JSON', 'utf8'))" 2>/dev/null; then
            pass ".markdownlint.json is valid JSON"
            JSON_VALIDATED=true
        else
            fail ".markdownlint.json contains invalid JSON syntax"
            JSON_VALIDATED=true
        fi
    fi

    if [ "$JSON_VALIDATED" = false ]; then
        skip ".markdownlint.json JSON check" "no JSON validator available (need python3, jq, or node)"
    fi
fi

# ── Check 7: shellcheck on scripts/*.sh ────────────────────────────
section_header 7 "Shell script lint (shellcheck scripts/*.sh)"

if ! command -v shellcheck &>/dev/null; then
    skip "shellcheck" "shellcheck is not installed (install: apt install shellcheck)"
elif ! compgen -G "$REPO_ROOT/scripts/*.sh" > /dev/null; then
    skip "shellcheck" "no .sh files found in scripts/"
else
    if shellcheck "$REPO_ROOT"/scripts/*.sh 2>&1; then
        pass "All shell scripts pass shellcheck"
    else
        fail "shellcheck reported issues in scripts/*.sh"
    fi
fi

# ── Check 8: markdownlint on *.md ──────────────────────────────────
section_header 8 "Markdown lint (markdownlint **/*.md)"

if ! command -v markdownlint-cli2 &>/dev/null && ! command -v markdownlint &>/dev/null; then
    skip "markdownlint" "markdownlint is not installed (install: npm install -g markdownlint-cli2)"
else
    MDL_CMD=""
    if command -v markdownlint-cli2 &>/dev/null; then
        MDL_CMD="markdownlint-cli2"
    else
        MDL_CMD="markdownlint"
    fi
    if $MDL_CMD "**/*.md" 2>&1; then
        pass "All Markdown files pass markdownlint"
    else
        fail "markdownlint reported issues in *.md files"
    fi
fi

# ── Check 9: mkdocs nav validation ──────────────────────────────────
section_header 9 "MkDocs nav validation (docs/ file references)"

MKDOCS_YML="$REPO_ROOT/mkdocs.yml"
DOCS_DIR="$REPO_ROOT/docs"

if [ ! -f "$MKDOCS_YML" ]; then
    skip "MkDocs nav check" "mkdocs.yml not found"
elif [ ! -d "$DOCS_DIR" ]; then
    skip "MkDocs nav check" "docs/ directory not found"
else
    MKDOCS_NAV_OK=true
    # Extract file references from the nav section of mkdocs.yml
    # Nav entries look like: `  - Label: filename.md`
    IN_NAV=false
    while IFS= read -r line; do
        trimmed="${line#"${line%%[![:space:]]*}"}"
        trimmed="${trimmed%"${trimmed##*[![:space:]]}"}"
        if [ "$trimmed" = "nav:" ]; then
            IN_NAV=true
            continue
        fi
        if $IN_NAV && [ -n "$line" ] && [[ ! "$line" =~ ^[[:space:]] ]] && [[ ! "$line" =~ ^# ]]; then
            break
        fi
        if ! $IN_NAV; then
            continue
        fi
        # Match labeled entries like: `  - Label: something.md`
        # and bare entries like: `  - something.md`
        FILE_REF=""
        if [[ "$trimmed" =~ ^-\ .+:\ (.+\.md)$ ]]; then
            FILE_REF="${BASH_REMATCH[1]}"
        elif [[ "$trimmed" =~ ^-\ (.+\.md)$ ]]; then
            FILE_REF="${BASH_REMATCH[1]}"
        fi
        if [ -n "$FILE_REF" ] && [ ! -f "$DOCS_DIR/$FILE_REF" ]; then
            echo -e "${RED}MISSING${NC}: mkdocs.yml nav references '$FILE_REF' but docs/$FILE_REF does not exist"
            MKDOCS_NAV_OK=false
        fi
    done < "$MKDOCS_YML"

    if $MKDOCS_NAV_OK; then
        pass "All mkdocs.yml nav file references exist in docs/"
    else
        fail "mkdocs.yml nav references files that do not exist in docs/"
    fi
fi

# ── Summary ──────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}${YELLOW}=== Summary ===${NC}"
echo -e "  ${GREEN}Passed${NC}:  $PASSED"
echo -e "  ${RED}Failed${NC}:  $FAILURES"
echo -e "  ${YELLOW}Skipped${NC}: $SKIPPED"
echo ""

if [ "$FAILURES" -gt 0 ]; then
    echo -e "${RED}CI validation FAILED: $FAILURES check(s) failed.${NC}"
    echo "Fix the issues above before pushing."
    exit 1
else
    echo -e "${GREEN}CI validation PASSED: All checks passed.${NC}"
    echo "You are clear to push."
    exit 0
fi
