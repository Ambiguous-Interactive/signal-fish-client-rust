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
#   2 — the guard could not do its job (canonical sources missing)

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

# ── cfg(test) region tracking ─────────────────────────────────────────
# A match is excused only inside the brace-bounded body of a `mod` opened
# by a `#[cfg(...test...)]` attribute. Excusing "everything below the last
# test attribute" was fail-open: production code appended after a closed
# test module, or below a mid-file test-only item, escaped the gate
# entirely.
#
# The region scan is textual (brace counting includes braces inside string
# literals and comments); a miscount almost always fails toward flagging,
# and clippy's deny lints remain the authoritative backstop for compiled
# code. A module body whose closing brace never appears extends the region
# to end-of-file, which only arises in files that do not compile. Any
# cfg attribute containing `test` may open a region — including the
# `not(test)` form; the pinned ci_config_tests.rs sweep
# (no_production_source_uses_cfg_not_test) keeps that form out of the
# production roots scanned here.
# shellcheck disable=SC2016  # intentional: the awk program must not expand
CFG_REGION_AWK='
{
    lines[NR] = $0
    # Only a line that STARTS with a bracketed attribute can open a region;
    # a `test` mention inside a comment or string literal must not.
    if ($0 ~ /^[[:space:]]*#[[][^]]*test[^]]*]/) istest[NR] = 1
}
END {
    i = 1
    while (i <= NR) {
        if (istest[i]) {
            # Scan forward from the attribute to the first code item. The
            # attribute (including multi-line #[allow( ... )] arguments),
            # comments, and blanks are skipped; the first item decides:
            # `mod` opens a region, any other item closes the search.
            modline = 0
            for (k = i; k <= NR; k++) {
                code = lines[k]
                sub(/^[[:space:]]+/, "", code)
                n = split(code, tok, /[[:space:]()]+/)
                kw = ""
                for (t = 1; t <= n && t <= 4; t++) {
                    if (tok[t] == "pub" || tok[t] == "crate" || tok[t] == "async" || tok[t] == "unsafe") continue
                    kw = tok[t]; break
                }
                if (kw == "mod") { modline = k; break }
                if (kw == "fn" || kw == "struct" || kw == "enum" || kw == "impl" || kw == "use" || kw == "const" || kw == "static" || kw == "type" || kw == "trait") break
            }
            if (modline) {
                mline = lines[modline]
                if (mline ~ /;/ && mline !~ /\{/) {
                    # Declaration-only module (`mod x;`) — no body to excuse.
                    i = modline + 1
                    continue
                }
                depth = 0; opened = 0; endline = modline; lookahead = 0
                for (k = modline; k <= NR; k++) {
                    s = lines[k]
                    o = gsub(/{/, "{", s); c = gsub(/}/, "}", s)
                    depth += o - c
                    endline = k
                    if (o > 0) opened = 1
                    if (opened && depth <= 0) break
                    if (!opened) {
                        # Tolerate a brace on the following line; past that,
                        # this is not a module body (stop before production
                        # braces can be mistaken for one).
                        lookahead++
                        if (lookahead > 1) break
                    }
                }
                if (opened) { print modline, endline; i = endline + 1 }
                else { i = modline + 1 }
                continue
            }
        }
        i++
    }
}'

REGION_CACHE="$(mktemp -d "${TMPDIR:-/tmp}/sf-no-panics-regions.XXXXXX")"
trap 'rm -rf "$REGION_CACHE"' EXIT

in_test_region() {
    # $1 = file, $2 = line number; returns 0 when the line lies inside the
    # brace-bounded body of a cfg(test)-opened mod block.
    local file="$1" lineno="$2"
    local cache
    cache="$REGION_CACHE/$(printf '%s' "$file" | tr '/' '_')"
    if [ ! -f "$cache" ]; then
        awk "$CFG_REGION_AWK" "$file" >"$cache" 2>/dev/null || : >"$cache"
    fi
    local start end
    while read -r start end; do
        [ -n "$start" ] || continue
        if [ "$lineno" -ge "$start" ] && [ "$lineno" -le "$end" ]; then
            return 0
        fi
    done <"$cache"
    return 1
}

echo -e "${YELLOW}=== Panic-free policy check ===${NC}"
echo ""

# ── Phase 1: Scan library and example code (must be panic-free) ──────
echo -e "${YELLOW}Phase 1: Scanning all workspace member sources for forbidden patterns...${NC}"

# The core crate's sources are the canonical scan root; scanning nothing
# must fail the guard instead of reporting a vacuous pass.
if [ ! -d src ]; then
    echo -e "${RED}FATAL: src/ is missing — the core crate's sources were not scanned.${NC}" >&2
    exit 2
fi

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

        # Filter out matches inside cfg(test)-opened mod blocks.
        while IFS= read -r line; do
            line="${line//$'\r'/}"
            file=$(printf '%s\n' "$line" | cut -d: -f1)
            lineno=$(printf '%s\n' "$line" | cut -d: -f2)

            if in_test_region "$file" "$lineno"; then
                # Inside a #[cfg(..test..)] module body — allowed.
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
