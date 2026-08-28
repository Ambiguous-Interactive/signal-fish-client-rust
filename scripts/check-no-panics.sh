#!/usr/bin/env bash
# check-no-panics.sh — Guard script for the hard-fail panic policy.
#
# Scans every workspace member's production sources (src/, examples/,
# crates/*/src, crates/*/examples, tools/*/src) for panic-prone patterns
# that should not appear in production or example code, and verifies test
# sources opt in explicitly via #![allow(...)] or #[allow(...)] attributes.
#
# Phase 1: grep-based scan of all member library/example sources (~1-2 seconds)
# Phase 2: verify test files have explicit opt-in attributes
#
# NOTE: A previous Phase 3 (running cargo clippy with panic-free lints)
# was removed because those lints are already configured as deny in
# Cargo.toml [lints.clippy], and the pre-commit hook already runs
# `cargo clippy --workspace --all-targets --all-features -- -D warnings` which
# catches all deny-level lints. Removing Phase 3 saves ~60-120 seconds
# of redundant compilation per hook run.
#
# Exit codes:
#   0 — no violations found
#   1 — forbidden patterns detected

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

# ── Forbidden patterns ────────────────────────────────────────────────
# These patterns are denied by Clippy lints in Cargo.toml. This script
# provides a defense-in-depth check that catches patterns even when
# Clippy is not run (e.g. during pre-commit checks or when Clippy is
# skipped). Markdown documentation code blocks are validated separately
# by scripts/extract-rust-snippets.sh.
PATTERNS=(
    '\.unwrap()'
    '\.expect('
    'panic!('
    'todo!('
    'unimplemented!('
    'unreachable!('
)

# ── Production source directories ─────────────────────────────────────
# Every workspace member's library/binary sources must be panic-free, not
# just the core crate. The globs pick up all members under the conventional
# paths so a future member cannot silently escape this gate.
PROD_DIRS=(src examples crates/*/src crates/*/examples tools/*/src)
TEST_DIRS=(tests crates/*/tests tools/*/tests)

echo -e "${YELLOW}=== Panic-free policy check ===${NC}"
echo ""

# ── Phase 1: Scan library and example code (must be panic-free) ──────
echo -e "${YELLOW}Phase 1: Scanning all workspace member sources for forbidden patterns...${NC}"

for dir in "${PROD_DIRS[@]}"; do
    if [ ! -d "$dir" ]; then
        continue
    fi

    for pattern in "${PATTERNS[@]}"; do
        # Find violations, filtering out:
        #   - Comment-only and doc-comment lines (`// …`, `/// …`). grep -rn
        #     prefixes every hit with `path:line:`, so the filter is
        #     prefix-aware: `^[^:]*:[0-9]*:` skips the location, then the
        #     content must start with optional whitespace and `//`.
        #   - Lines referencing the pattern inside a trailing comment. `//`
        #     only starts a comment at line start or after whitespace, so a
        #     colon-adjacent `//` (e.g. an "https://…" URL) is not treated as
        #     one. Residual hole: whitespace inside a string literal can
        #     still mask a violation later on the same line — clippy's deny
        #     lints remain the authoritative backstop for that class.
        matches=$(grep -rn --include='*.rs' "$pattern" "$dir" \
            | grep -v '^[^:]*:[0-9]*:[[:space:]]*//' \
            | grep -v '[[:space:]]//.*'"$pattern" \
            || true)

        if [ -z "$matches" ]; then
            continue
        fi

        # Filter out matches inside #[cfg(test)] modules.
        while IFS= read -r line; do
            line="${line//$'\r'/}"
            file=$(printf '%s\n' "$line" | cut -d: -f1)
            lineno=$(printf '%s\n' "$line" | cut -d: -f2)

            # Find the last #[cfg(...test...)] line number in the file.
            # This matches both simple `#[cfg(test)]` and compound forms
            # like `#[cfg(all(test, feature = "..."))]`.
            #
            # LIMITATION: This pattern also matches `#[cfg(not(test))]`,
            # which would incorrectly treat the code below it as "inside a
            # test module" when it is actually the opposite. A safety-net
            # test in tests/ci_config_tests.rs (module panic_script_cfg_handling,
            # no_production_source_uses_cfg_not_test) verifies that none of
            # the production roots scanned here uses `not(test)` in any cfg
            # attribute, so this false positive cannot occur in practice.
            cfg_test_line=$(grep -nE '#\[cfg\((.*[^[:alnum:]_])?test([^[:alnum:]_]|$)' "$file" 2>/dev/null \
                | tail -1 | cut -d: -f1 || true)

            if [ -n "$cfg_test_line" ] && [ "$lineno" -gt "$cfg_test_line" ]; then
                # Inside a #[cfg(..test..)] module — allowed.
                continue
            fi

            echo -e "${RED}VIOLATION:${NC} $line"
            VIOLATIONS=$((VIOLATIONS + 1))
        done <<< "$matches"
    done
done

if [ "$VIOLATIONS" -eq 0 ]; then
    echo -e "${GREEN}Phase 1: PASS — no violations in any workspace member sources${NC}"
fi
echo ""

# ── Phase 2: Scan test files for missing opt-in ──────────────────────
# Test files under tests/ are allowed to use panic-prone patterns, but they
# MUST contain a #![allow(...)] or #[allow(...)] attribute that
# explicitly allows at least one panic-related Clippy lint (e.g.
# clippy::unwrap_used). Files without this opt-in are flagged.
echo -e "${YELLOW}Phase 2: Checking test sources for panic-free opt-in...${NC}"

TESTS_VIOLATIONS=0
for test_root in "${TEST_DIRS[@]}"; do
    if [ ! -d "$test_root" ]; then
        continue
    fi
    # Recursively find repository-owned .rs files while pruning nested Cargo
    # build output (for example tests/godot-web-smoke/target/).
    while IFS= read -r test_file; do
        test_file="${test_file//$'\r'/}"
        # Check if the file has any panic-prone patterns at all.
        has_patterns=false
        for pattern in "${PATTERNS[@]}"; do
            if grep -q "$pattern" "$test_file" 2>/dev/null; then
                has_patterns=true
                break
            fi
        done

        if [ "$has_patterns" = false ]; then
            continue
        fi

        # File has panic-prone patterns — verify it explicitly opts in to at
        # least one panic-related Clippy lint.  The check is split into two
        # grep passes so it handles multi-line #![allow( ... )] blocks, and
        # accepts both allow and expect opt-in forms.
        if ! grep -qE '#!?\[(allow|expect)\(' "$test_file" 2>/dev/null || \
           ! grep -qE 'clippy::(unwrap_used|expect_used|panic|todo|unimplemented|unreachable)' "$test_file" 2>/dev/null; then
            echo -e "${RED}VIOLATION:${NC} $test_file uses panic-prone patterns without allowing a panic-related lint (e.g. #![allow(clippy::unwrap_used)])"
            TESTS_VIOLATIONS=$((TESTS_VIOLATIONS + 1))
        fi
    done < <(find "$test_root" -type d -name target -prune -o -name '*.rs' -type f -print)
done

if [ "$TESTS_VIOLATIONS" -eq 0 ]; then
    echo -e "${GREEN}Phase 2: PASS — all test files have explicit opt-in${NC}"
else
    VIOLATIONS=$((VIOLATIONS + TESTS_VIOLATIONS))
fi
echo ""

# ── Result ────────────────────────────────────────────────────────────
if [ "$VIOLATIONS" -gt 0 ]; then
    echo -e "${RED}FAILED: $VIOLATIONS violation(s) found.${NC}"
    echo "Fix all panic-prone patterns before committing."
    exit 1
else
    echo -e "${GREEN}PASSED: No panic-prone patterns found.${NC}"
    exit 0
fi
