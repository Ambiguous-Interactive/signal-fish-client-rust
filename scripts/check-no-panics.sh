#!/usr/bin/env bash
# check-no-panics.sh — Guard script for the hard-fail panic policy.
#
# Scans src/, examples/, and tests/ for panic-prone patterns that should
# not appear in production or example code. Test code is allowed to use
# these patterns when explicitly opted in via #![allow(...)] or
# #[allow(...)] attributes.
#
# Exit codes:
#   0 — no violations found
#   1 — forbidden patterns detected

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

VIOLATIONS=0

# ── Forbidden patterns ────────────────────────────────────────────────
# These patterns are denied by Clippy lints in Cargo.toml. This script
# provides a defense-in-depth check that catches patterns even when
# Clippy is not run (e.g. in documentation code blocks).
PATTERNS=(
    '\.unwrap()'
    '\.expect('
    'panic!('
    'todo!('
    'unimplemented!('
)

echo -e "${YELLOW}=== Panic-free policy check ===${NC}"
echo ""

# ── Phase 1: Scan library and example code (must be panic-free) ──────
echo -e "${YELLOW}Phase 1: Scanning src/ and examples/ for forbidden patterns...${NC}"

for dir in src examples; do
    if [ ! -d "$dir" ]; then
        continue
    fi

    for pattern in "${PATTERNS[@]}"; do
        # Find violations, filtering out:
        #   - Comment-only lines (// ...)
        #   - Lines referencing the pattern inside comments
        matches=$(grep -rn --include='*.rs' "$pattern" "$dir" \
            | grep -v '^[[:space:]]*//' \
            | grep -v '//.*'"$pattern" \
            || true)

        if [ -z "$matches" ]; then
            continue
        fi

        # Filter out matches inside #[cfg(test)] modules.
        while IFS= read -r line; do
            file=$(echo "$line" | cut -d: -f1)
            lineno=$(echo "$line" | cut -d: -f2)

            # Find the last #[cfg(test)] line number in the file.
            cfg_test_line=$(grep -n '#\[cfg(test)\]' "$file" 2>/dev/null \
                | tail -1 | cut -d: -f1 || true)

            if [ -n "$cfg_test_line" ] && [ "$lineno" -gt "$cfg_test_line" ]; then
                # Inside a #[cfg(test)] module — allowed.
                continue
            fi

            echo -e "${RED}VIOLATION:${NC} $line"
            VIOLATIONS=$((VIOLATIONS + 1))
        done <<< "$matches"
    done
done

if [ "$VIOLATIONS" -eq 0 ]; then
    echo -e "${GREEN}Phase 1: PASS — no violations in src/ or examples/${NC}"
fi
echo ""

# ── Phase 2: Scan test files for missing opt-in ──────────────────────
# Test files in tests/ are allowed to use panic-prone patterns, but they
# MUST have a #![allow(...)] at the file top or a module-level #[allow]
# to explicitly opt in. Files without the opt-in are flagged.
echo -e "${YELLOW}Phase 2: Checking tests/ for panic-free opt-in...${NC}"

TESTS_VIOLATIONS=0
if [ -d "tests" ]; then
    # Recursively find all .rs files under tests/ (covers tests/common/,
    # tests/helpers/, or any future subdirectories).
    while IFS= read -r test_file; do
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

        # File has panic-prone patterns — verify it has an opt-in allow.
        if ! grep -q '#!\[allow(' "$test_file" 2>/dev/null; then
            echo -e "${RED}VIOLATION:${NC} $test_file uses panic-prone patterns without #![allow(...)]"
            TESTS_VIOLATIONS=$((TESTS_VIOLATIONS + 1))
        fi
    done < <(find tests -name '*.rs' -type f)
fi

if [ "$TESTS_VIOLATIONS" -eq 0 ]; then
    echo -e "${GREEN}Phase 2: PASS — all test files have explicit opt-in${NC}"
else
    VIOLATIONS=$((VIOLATIONS + TESTS_VIOLATIONS))
fi
echo ""

# ── Phase 3: Run Clippy with hard-fail lints ─────────────────────────
# The deny-level lints are configured in Cargo.toml [lints.clippy].
# We pass them again here as defense-in-depth (ensures enforcement even
# if someone removes the [lints.clippy] section from Cargo.toml).
echo -e "${YELLOW}Phase 3: Running Clippy with panic-free lints...${NC}"
if cargo clippy --all-targets --all-features -- \
    -D clippy::unwrap_used \
    -D clippy::expect_used \
    -D clippy::panic \
    -D clippy::todo \
    -D clippy::unimplemented \
    -D clippy::indexing_slicing \
    2>&1; then
    echo -e "${GREEN}Phase 3: PASS${NC}"
else
    echo -e "${RED}Phase 3: FAIL${NC}"
    VIOLATIONS=$((VIOLATIONS + 1))
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
