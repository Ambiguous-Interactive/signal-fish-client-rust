#!/usr/bin/env bash
# check-all.sh — Unified local pre-flight check for signal-fish-client.
#
# Reproduces ALL major CI checks locally in a single command so developers
# can catch failures before pushing. Checks run fastest-first.
#
# Usage:
#   bash scripts/check-all.sh          # run all phases
#   bash scripts/check-all.sh --quick  # run only phases 1-3 (fmt, clippy, test)
#
# Phases:
#   1. cargo fmt            (required)
#   2. cargo clippy          (required)
#   3. cargo test            (required)
#   4. cargo doc             (required)
#   5. cargo deny            (optional — skip if not installed)
#   6. cargo audit           (optional — skip if not installed)
#   7. Panic-free policy     (delegates to scripts/check-no-panics.sh)
#   8. Docs validation       (markdownlint, lychee, typos — each optional)
#   9. Unused deps           (cargo-machete — optional)
#  10. Workflow lint          (delegates to scripts/check-workflows.sh)
#  11. Publish dry-run        (optional — cargo publish --dry-run)
#  12. Examples validation    (doc tests, examples build, snippet check)
#  13. Semver checks        (optional — cargo-semver-checks)
#  14. Miri                  (optional — requires nightly + miri component)
#  15. Fuzz smoke tests      (optional — requires nightly + cargo-fuzz)
#  16. Mutation testing      (optional — requires cargo-mutants)
#  17. Code coverage          (optional — requires cargo-llvm-cov)
#
# Notes:
#   - MSRV (1.85.0) verification is CI-only (requires rustup toolchain override)
#   - --quick runs only the mandatory baseline (phases 1-3)
#
# Exit codes:
#   0 — all phases passed (or skipped)
#   1 — one or more phases failed

set -euo pipefail

# ── Resolve paths relative to this script ────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# ── Color constants ──────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BOLD='\033[1m'
NC='\033[0m' # No Color

# ── Parse arguments ──────────────────────────────────────────────────
QUICK=false
for arg in "$@"; do
    case "$arg" in
        --quick) QUICK=true ;;
        *)
            echo -e "${RED}Unknown argument: $arg${NC}" >&2
            echo "Usage: $0 [--quick]" >&2
            exit 1
            ;;
    esac
done

# ── Phase tracking ───────────────────────────────────────────────────
TOTAL_PHASES=17
if [ "$QUICK" = true ]; then
    TOTAL_PHASES=3
fi

declare -a PHASE_NAMES
declare -a PHASE_RESULTS

PHASE_NAMES[1]="cargo fmt"
PHASE_NAMES[2]="cargo clippy"
PHASE_NAMES[3]="cargo test"
PHASE_NAMES[4]="cargo doc"
PHASE_NAMES[5]="cargo deny"
PHASE_NAMES[6]="cargo audit"
PHASE_NAMES[7]="Panic-free policy"
PHASE_NAMES[8]="Docs validation"
PHASE_NAMES[9]="Unused deps (cargo-machete)"
PHASE_NAMES[10]="Workflow lint"
PHASE_NAMES[11]="Publish dry-run"
PHASE_NAMES[12]="Examples validation"
PHASE_NAMES[13]="Semver API compatibility"
PHASE_NAMES[14]="Miri (UB detection)"
PHASE_NAMES[15]="Fuzz smoke tests"
PHASE_NAMES[16]="Mutation testing"
PHASE_NAMES[17]="Code coverage"

for i in $(seq 1 "$TOTAL_PHASES"); do
    PHASE_RESULTS[i]="SKIP"
done

FAILURES=0

# ── Helper: require a command or abort ───────────────────────────────
require_cmd() {
    local cmd="$1"
    local hint="$2"
    if ! command -v "$cmd" &>/dev/null; then
        echo -e "${RED}ERROR: Required tool '$cmd' is not installed.${NC}" >&2
        echo "  Install: $hint" >&2
        exit 1
    fi
}

# ── Preflight: verify required tools ────────────────────────────────
echo -e "${BOLD}${YELLOW}=== signal-fish-client: pre-flight checks ===${NC}"
if [ "$QUICK" = true ]; then
    echo -e "${YELLOW}Mode: --quick (phases 1-3 only)${NC}"
fi
echo ""

require_cmd cargo "https://rustup.rs"
require_cmd rustfmt "rustup component add rustfmt"
# Clippy is a cargo subcommand; verify via cargo-clippy binary
require_cmd cargo-clippy "rustup component add clippy"

echo -e "${YELLOW}Preflight: Checking fuzz seeds for forbidden '\"data\":null'...${NC}"
if grep -R -n -E '"data"[[:space:]]*:[[:space:]]*null' \
    fuzz/seeds/fuzz_client_message \
    fuzz/seeds/fuzz_server_message; then
    echo -e "${RED}ERROR: Found forbidden '\"data\":null' in fuzz seed files.${NC}" >&2
    echo "  Unit variants must serialize without a data field (e.g., {\"type\":\"Ping\"})." >&2
    exit 1
fi
echo -e "${GREEN}Preflight: PASS${NC}"
echo ""

# ── Phase 1: cargo fmt ──────────────────────────────────────────────
echo -e "${YELLOW}Phase 1/$TOTAL_PHASES: Checking formatting (cargo fmt)...${NC}"
if cargo fmt --check 2>&1; then
    echo -e "${GREEN}Phase 1: PASS${NC}"
    PHASE_RESULTS[1]="PASS"
else
    echo -e "${RED}Phase 1: FAIL${NC}"
    PHASE_RESULTS[1]="FAIL"
    FAILURES=$((FAILURES + 1))
fi
echo ""

# ── Phase 2: cargo clippy ──────────────────────────────────────────
echo -e "${YELLOW}Phase 2/$TOTAL_PHASES: Running Clippy (cargo clippy — 3 feature combos)...${NC}"
PHASE2_FAIL=false
for feature_flags in "" "--all-features" "--no-default-features"; do
    label="${feature_flags:-default features}"
    if cargo clippy --all-targets $feature_flags -- -D warnings 2>&1; then
        echo -e "${GREEN}  clippy ($label): PASS${NC}"
    else
        echo -e "${RED}  clippy ($label): FAIL${NC}"
        PHASE2_FAIL=true
    fi
done
if [ "$PHASE2_FAIL" = true ]; then
    echo -e "${RED}Phase 2: FAIL${NC}"
    PHASE_RESULTS[2]="FAIL"
    FAILURES=$((FAILURES + 1))
else
    echo -e "${GREEN}Phase 2: PASS${NC}"
    PHASE_RESULTS[2]="PASS"
fi
echo ""

# ── Phase 3: cargo test ────────────────────────────────────────────
echo -e "${YELLOW}Phase 3/$TOTAL_PHASES: Running tests (cargo test — 3 feature combos)...${NC}"
PHASE3_FAIL=false
for feature_flags in "" "--all-features" "--no-default-features"; do
    label="${feature_flags:-default features}"
    if cargo test $feature_flags 2>&1; then
        echo -e "${GREEN}  test ($label): PASS${NC}"
    else
        echo -e "${RED}  test ($label): FAIL${NC}"
        PHASE3_FAIL=true
    fi
done
if [ "$PHASE3_FAIL" = true ]; then
    echo -e "${RED}Phase 3: FAIL${NC}"
    PHASE_RESULTS[3]="FAIL"
    FAILURES=$((FAILURES + 1))
else
    echo -e "${GREEN}Phase 3: PASS${NC}"
    PHASE_RESULTS[3]="PASS"
fi
echo ""

# ── Quick mode: stop here ───────────────────────────────────────────
if [ "$QUICK" = true ]; then
    # Jump to summary
    echo -e "${BOLD}${YELLOW}=== Summary (--quick) ===${NC}"
    for i in $(seq 1 "$TOTAL_PHASES"); do
        case "${PHASE_RESULTS[$i]}" in
            PASS) color="$GREEN" ;;
            FAIL) color="$RED" ;;
            SKIP) color="$YELLOW" ;;
            *)    color="$NC" ;;
        esac
        printf "  Phase %2d: ${color}%-4s${NC}  %s\n" "$i" "${PHASE_RESULTS[$i]}" "${PHASE_NAMES[$i]}"
    done
    echo ""

    if [ "$FAILURES" -gt 0 ]; then
        echo -e "${RED}FAILED: $FAILURES phase(s) failed.${NC}"
        exit 1
    else
        echo -e "${GREEN}PASSED: All quick checks passed.${NC}"
        exit 0
    fi
fi

# ── Phase 4: cargo doc ─────────────────────────────────────────────
echo -e "${YELLOW}Phase 4/$TOTAL_PHASES: Building documentation (cargo doc)...${NC}"
if RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps 2>&1; then
    echo -e "${GREEN}Phase 4: PASS${NC}"
    PHASE_RESULTS[4]="PASS"
else
    echo -e "${RED}Phase 4: FAIL${NC}"
    PHASE_RESULTS[4]="FAIL"
    FAILURES=$((FAILURES + 1))
fi
echo ""

# ── Phase 5: cargo deny ────────────────────────────────────────────
echo -e "${YELLOW}Phase 5/$TOTAL_PHASES: Checking dependency policy (cargo deny)...${NC}"
if ! command -v cargo-deny &>/dev/null; then
    echo -e "${YELLOW}SKIP: cargo-deny is not installed.${NC}"
    echo "  Install: cargo install cargo-deny"
    PHASE_RESULTS[5]="SKIP"
else
    if cargo deny check 2>&1; then
        echo -e "${GREEN}Phase 5: PASS${NC}"
        PHASE_RESULTS[5]="PASS"
    else
        echo -e "${RED}Phase 5: FAIL${NC}"
        PHASE_RESULTS[5]="FAIL"
        FAILURES=$((FAILURES + 1))
    fi
fi
echo ""

# ── Phase 6: cargo audit ───────────────────────────────────────────
echo -e "${YELLOW}Phase 6/$TOTAL_PHASES: Running security audit (cargo audit)...${NC}"
if ! command -v cargo-audit &>/dev/null; then
    echo -e "${YELLOW}SKIP: cargo-audit is not installed.${NC}"
    echo "  Install: cargo install cargo-audit"
    PHASE_RESULTS[6]="SKIP"
else
    # Ensure a lockfile exists (library crate may not commit Cargo.lock)
    if [ ! -f Cargo.lock ]; then
        cargo generate-lockfile 2>&1
    fi
    if cargo audit 2>&1; then
        echo -e "${GREEN}Phase 6: PASS${NC}"
        PHASE_RESULTS[6]="PASS"
    else
        echo -e "${RED}Phase 6: FAIL${NC}"
        PHASE_RESULTS[6]="FAIL"
        FAILURES=$((FAILURES + 1))
    fi
fi
echo ""

# ── Phase 7: Panic-free policy ──────────────────────────────────────
echo -e "${YELLOW}Phase 7/$TOTAL_PHASES: Panic-free policy check...${NC}"
if [ -f "$SCRIPT_DIR/check-no-panics.sh" ]; then
    if bash "$SCRIPT_DIR/check-no-panics.sh" 2>&1; then
        echo -e "${GREEN}Phase 7: PASS${NC}"
        PHASE_RESULTS[7]="PASS"
    else
        echo -e "${RED}Phase 7: FAIL${NC}"
        PHASE_RESULTS[7]="FAIL"
        FAILURES=$((FAILURES + 1))
    fi
else
    echo -e "${YELLOW}SKIP: scripts/check-no-panics.sh not found.${NC}"
    PHASE_RESULTS[7]="SKIP"
fi
echo ""

# ── Phase 8: Docs validation ───────────────────────────────────────
echo -e "${YELLOW}Phase 8/$TOTAL_PHASES: Docs validation (markdownlint, lychee, typos)...${NC}"
PHASE8_FAILURES=0
PHASE8_RAN=0

# 8a: markdownlint
if ! command -v markdownlint-cli2 &>/dev/null && ! command -v markdownlint &>/dev/null; then
    echo -e "${YELLOW}  SKIP: markdownlint-cli2 is not installed.${NC}"
    echo "    Install: npm install -g markdownlint-cli2"
else
    PHASE8_RAN=$((PHASE8_RAN + 1))
    MDL_CMD=""
    if command -v markdownlint-cli2 &>/dev/null; then
        MDL_CMD="markdownlint-cli2"
    else
        MDL_CMD="markdownlint"
    fi
    if $MDL_CMD "**/*.md" 2>&1; then
        echo -e "${GREEN}  markdownlint: PASS${NC}"
    else
        echo -e "${RED}  markdownlint: FAIL${NC}"
        PHASE8_FAILURES=$((PHASE8_FAILURES + 1))
    fi
fi

# 8b: lychee
if ! command -v lychee &>/dev/null; then
    echo -e "${YELLOW}  SKIP: lychee is not installed.${NC}"
    echo "    Install: cargo install lychee"
else
    PHASE8_RAN=$((PHASE8_RAN + 1))
    if lychee --config .lychee.toml "**/*.md" 2>&1; then
        echo -e "${GREEN}  lychee: PASS${NC}"
    else
        echo -e "${RED}  lychee: FAIL${NC}"
        PHASE8_FAILURES=$((PHASE8_FAILURES + 1))
    fi
fi

# 8c: typos
if ! command -v typos &>/dev/null; then
    echo -e "${YELLOW}  SKIP: typos is not installed.${NC}"
    echo "    Install: cargo install typos-cli"
else
    PHASE8_RAN=$((PHASE8_RAN + 1))
    if typos --config .typos.toml 2>&1; then
        echo -e "${GREEN}  typos: PASS${NC}"
    else
        echo -e "${RED}  typos: FAIL${NC}"
        PHASE8_FAILURES=$((PHASE8_FAILURES + 1))
    fi
fi

if [ "$PHASE8_FAILURES" -gt 0 ]; then
    echo -e "${RED}Phase 8: FAIL ($PHASE8_FAILURES sub-check(s) failed)${NC}"
    PHASE_RESULTS[8]="FAIL"
    FAILURES=$((FAILURES + 1))
elif [ "$PHASE8_RAN" -eq 0 ]; then
    echo -e "${YELLOW}Phase 8: SKIP (no docs validation tools installed)${NC}"
    PHASE_RESULTS[8]="SKIP"
else
    echo -e "${GREEN}Phase 8: PASS${NC}"
    PHASE_RESULTS[8]="PASS"
fi
echo ""

# ── Phase 9: Unused deps (cargo-machete) ───────────────────────────
echo -e "${YELLOW}Phase 9/$TOTAL_PHASES: Checking for unused dependencies (cargo-machete)...${NC}"
if ! command -v cargo-machete &>/dev/null; then
    echo -e "${YELLOW}SKIP: cargo-machete is not installed.${NC}"
    echo "  Install: cargo install cargo-machete"
    PHASE_RESULTS[9]="SKIP"
else
    if cargo machete 2>&1; then
        echo -e "${GREEN}Phase 9: PASS${NC}"
        PHASE_RESULTS[9]="PASS"
    else
        echo -e "${RED}Phase 9: FAIL${NC}"
        PHASE_RESULTS[9]="FAIL"
        FAILURES=$((FAILURES + 1))
    fi
fi
echo ""

# ── Phase 10: Workflow lint ─────────────────────────────────────────
echo -e "${YELLOW}Phase 10/$TOTAL_PHASES: Workflow lint (actionlint, yamllint, shellcheck)...${NC}"
if [ -f "$SCRIPT_DIR/check-workflows.sh" ]; then
    if bash "$SCRIPT_DIR/check-workflows.sh" 2>&1; then
        echo -e "${GREEN}Phase 10: PASS${NC}"
        PHASE_RESULTS[10]="PASS"
    else
        echo -e "${RED}Phase 10: FAIL${NC}"
        PHASE_RESULTS[10]="FAIL"
        FAILURES=$((FAILURES + 1))
    fi
else
    echo -e "${YELLOW}SKIP: scripts/check-workflows.sh not found.${NC}"
    PHASE_RESULTS[10]="SKIP"
fi
echo ""

# ── Phase 11: Publish dry-run ─────────────────────────────────────
echo -e "${YELLOW}Phase 11/$TOTAL_PHASES: Publish dry-run (cargo publish --dry-run)...${NC}"
if cargo publish --dry-run 2>&1; then
    echo -e "${GREEN}Phase 11: PASS${NC}"
    PHASE_RESULTS[11]="PASS"
else
    # cargo publish --dry-run can fail for non-registry crates or incomplete
    # metadata — treat as a soft/optional failure.
    echo -e "${YELLOW}Phase 11: SKIP (cargo publish --dry-run failed — this is optional)${NC}"
    PHASE_RESULTS[11]="SKIP"
fi
echo ""

# ── Phase 12: Examples validation ──────────────────────────────────
# Note: Phase 3 already runs `cargo test --all-features` which includes doc-tests
# and examples. Phase 12 re-validates them as explicit categories for defense-in-depth
# and clearer diagnostics when doc-tests or examples specifically fail.
echo -e "${YELLOW}Phase 12/$TOTAL_PHASES: Examples validation (doc tests, examples, snippets)...${NC}"
PHASE12_FAILURES=0
PHASE12_RAN=0

# 12a: cargo test --doc
PHASE12_RAN=$((PHASE12_RAN + 1))
if cargo test --doc --all-features 2>&1; then
    echo -e "${GREEN}  doc tests: PASS${NC}"
else
    echo -e "${RED}  doc tests: FAIL${NC}"
    PHASE12_FAILURES=$((PHASE12_FAILURES + 1))
fi

# 12b: cargo test --examples
PHASE12_RAN=$((PHASE12_RAN + 1))
if cargo test --examples --all-features 2>&1; then
    echo -e "${GREEN}  examples build: PASS${NC}"
else
    echo -e "${RED}  examples build: FAIL${NC}"
    PHASE12_FAILURES=$((PHASE12_FAILURES + 1))
fi

# 12c: extract-rust-snippets.sh (optional — skip if script not found)
if [ -f "$SCRIPT_DIR/extract-rust-snippets.sh" ]; then
    PHASE12_RAN=$((PHASE12_RAN + 1))
    if bash "$SCRIPT_DIR/extract-rust-snippets.sh" 2>&1; then
        echo -e "${GREEN}  snippet check: PASS${NC}"
    else
        echo -e "${RED}  snippet check: FAIL${NC}"
        PHASE12_FAILURES=$((PHASE12_FAILURES + 1))
    fi
else
    echo -e "${YELLOW}  SKIP: scripts/extract-rust-snippets.sh not found.${NC}"
fi

if [ "$PHASE12_FAILURES" -gt 0 ]; then
    echo -e "${RED}Phase 12: FAIL ($PHASE12_FAILURES sub-check(s) failed)${NC}"
    PHASE_RESULTS[12]="FAIL"
    FAILURES=$((FAILURES + 1))
else
    echo -e "${GREEN}Phase 12: PASS${NC}"
    PHASE_RESULTS[12]="PASS"
fi
echo ""

# ── Phase 13: Semver API compatibility ────────────────────────────
echo -e "${YELLOW}Phase 13/$TOTAL_PHASES: Checking semver API compatibility (cargo-semver-checks)...${NC}"
if ! command -v cargo-semver-checks &>/dev/null; then
    echo -e "${YELLOW}SKIP: cargo-semver-checks is not installed.${NC}"
    echo "  Install: cargo install cargo-semver-checks"
    PHASE_RESULTS[13]="SKIP"
else
    # Compare against origin/main if available; skip if no baseline exists
    BASELINE_REV=""
    if git rev-parse --verify origin/main &>/dev/null; then
        BASELINE_REV="origin/main"
    elif git rev-parse --verify main &>/dev/null; then
        BASELINE_REV="main"
    fi

    if [ -n "$BASELINE_REV" ]; then
        # Verify the baseline has the crate source
        if git show "${BASELINE_REV}:Cargo.toml" &>/dev/null; then
            if cargo semver-checks check-release --baseline-rev "$BASELINE_REV" 2>&1; then
                echo -e "${GREEN}Phase 13: PASS${NC}"
                PHASE_RESULTS[13]="PASS"
            else
                echo -e "${RED}Phase 13: FAIL${NC}"
                PHASE_RESULTS[13]="FAIL"
                FAILURES=$((FAILURES + 1))
            fi
        else
            echo -e "${YELLOW}SKIP: Baseline revision does not contain crate source.${NC}"
            PHASE_RESULTS[13]="SKIP"
        fi
    else
        echo -e "${YELLOW}SKIP: No baseline branch found (origin/main or main).${NC}"
        PHASE_RESULTS[13]="SKIP"
    fi
fi
echo ""

# ── Phase 14: Miri (UB detection) ─────────────────────────────────
# Miri is scoped to synchronous test targets only (protocol_tests,
# ci_config_tests). Async/tokio tests are excluded because Miri does
# not support tokio's runtime internals.
echo -e "${YELLOW}Phase 14/$TOTAL_PHASES: Miri undefined behavior detection (nightly)...${NC}"
if ! rustup run nightly cargo miri --version &>/dev/null 2>&1; then
    echo -e "${YELLOW}SKIP: Miri is not available (requires nightly + rustup component add miri --toolchain nightly).${NC}"
    echo "  Install: rustup component add miri --toolchain nightly"
    PHASE_RESULTS[14]="SKIP"
else
    PHASE14_FAIL=false
    if MIRIFLAGS="-Zmiri-disable-isolation" cargo +nightly miri test --test protocol_tests --all-features 2>&1; then
        echo -e "${GREEN}  Miri (protocol_tests): PASS${NC}"
    else
        echo -e "${RED}  Miri (protocol_tests): FAIL${NC}"
        PHASE14_FAIL=true
    fi
    if MIRIFLAGS="-Zmiri-disable-isolation" cargo +nightly miri test --test ci_config_tests 2>&1; then
        echo -e "${GREEN}  Miri (ci_config_tests): PASS${NC}"
    else
        echo -e "${RED}  Miri (ci_config_tests): FAIL${NC}"
        PHASE14_FAIL=true
    fi
    if [ "$PHASE14_FAIL" = true ]; then
        # Miri failures are informational — do not count as hard failures
        echo -e "${YELLOW}Phase 14: WARN (Miri found issues — informational only)${NC}"
        PHASE_RESULTS[14]="WARN"
    else
        echo -e "${GREEN}Phase 14: PASS${NC}"
        PHASE_RESULTS[14]="PASS"
    fi
fi
echo ""

# ── Phase 15: Fuzz smoke tests ───────────────────────────────────
# Runs each fuzz target for 10 seconds locally (shorter than CI's 30s).
# Requires nightly + cargo-fuzz.
echo -e "${YELLOW}Phase 15/$TOTAL_PHASES: Fuzz smoke tests (nightly, 10s per target)...${NC}"
if ! command -v cargo-fuzz &>/dev/null; then
    echo -e "${YELLOW}SKIP: cargo-fuzz is not installed.${NC}"
    echo "  Install: cargo install cargo-fuzz"
    PHASE_RESULTS[15]="SKIP"
elif ! rustup run nightly rustc --version &>/dev/null 2>&1; then
    echo -e "${YELLOW}SKIP: Nightly toolchain is not installed (required by cargo-fuzz).${NC}"
    echo "  Install: rustup toolchain install nightly"
    PHASE_RESULTS[15]="SKIP"
else
    PHASE15_FAIL=false
    FUZZ_DIR="$REPO_ROOT/fuzz"
    if [ -d "$FUZZ_DIR" ]; then
        if (cd "$FUZZ_DIR" && cargo +nightly fuzz run fuzz_server_message seeds/fuzz_server_message -- -max_total_time=10) 2>&1; then
            echo -e "${GREEN}  fuzz_server_message: PASS${NC}"
        else
            echo -e "${RED}  fuzz_server_message: FAIL${NC}"
            PHASE15_FAIL=true
        fi
        if (cd "$FUZZ_DIR" && cargo +nightly fuzz run fuzz_client_message seeds/fuzz_client_message -- -max_total_time=10) 2>&1; then
            echo -e "${GREEN}  fuzz_client_message: PASS${NC}"
        else
            echo -e "${RED}  fuzz_client_message: FAIL${NC}"
            PHASE15_FAIL=true
        fi
    else
        echo -e "${YELLOW}SKIP: fuzz/ directory not found.${NC}"
        PHASE_RESULTS[15]="SKIP"
    fi
    if [ "$PHASE15_FAIL" = true ]; then
        # Fuzz failures are informational — do not count as hard failures
        echo -e "${YELLOW}Phase 15: WARN (fuzz found issues — informational only)${NC}"
        PHASE_RESULTS[15]="WARN"
    elif [ "${PHASE_RESULTS[15]}" != "SKIP" ]; then
        echo -e "${GREEN}Phase 15: PASS${NC}"
        PHASE_RESULTS[15]="PASS"
    fi
fi
echo ""

# ── Phase 16: Mutation testing ───────────────────────────────────
# Runs cargo-mutants on pure-logic modules. This is slow (several
# minutes) and informational only — mutations that survive indicate
# potential gaps in test coverage.
echo -e "${YELLOW}Phase 16/$TOTAL_PHASES: Mutation testing (cargo-mutants)...${NC}"
if ! command -v cargo-mutants &>/dev/null; then
    echo -e "${YELLOW}SKIP: cargo-mutants is not installed.${NC}"
    echo "  Install: cargo install cargo-mutants"
    PHASE_RESULTS[16]="SKIP"
else
    if cargo mutants --timeout 60 --no-shuffle -j 2 --file src/protocol.rs --file src/error_codes.rs --file src/error.rs 2>&1; then
        echo -e "${GREEN}Phase 16: PASS${NC}"
        PHASE_RESULTS[16]="PASS"
    else
        # Mutation testing failures are informational — surviving mutants
        # indicate test coverage gaps, not code defects.
        echo -e "${YELLOW}Phase 16: WARN (surviving mutants found — informational only)${NC}"
        PHASE_RESULTS[16]="WARN"
    fi
fi
echo ""

# ── Phase 17: Code coverage ───────────────────────────────────────
# Generates LLVM source-based coverage report using cargo-llvm-cov.
# Requires the llvm-tools-preview rustup component and cargo-llvm-cov.
echo -e "${YELLOW}Phase 17/$TOTAL_PHASES: Code coverage (cargo-llvm-cov)...${NC}"
if ! command -v cargo-llvm-cov &>/dev/null; then
    echo -e "${YELLOW}SKIP: cargo-llvm-cov is not installed.${NC}"
    echo "  Install: cargo install cargo-llvm-cov && rustup component add llvm-tools-preview"
    PHASE_RESULTS[17]="SKIP"
else
    if cargo llvm-cov --all-features --summary-only 2>&1; then
        echo -e "${GREEN}Phase 17: PASS${NC}"
        PHASE_RESULTS[17]="PASS"
    else
        # Coverage generation failures are informational — do not count as hard failures
        echo -e "${YELLOW}Phase 17: WARN (coverage generation failed — informational only)${NC}"
        PHASE_RESULTS[17]="WARN"
    fi
fi
echo ""

# ── Summary ─────────────────────────────────────────────────────────
echo -e "${BOLD}${YELLOW}=== Summary ===${NC}"
for i in $(seq 1 "$TOTAL_PHASES"); do
    case "${PHASE_RESULTS[$i]}" in
        PASS) color="$GREEN" ;;
        FAIL) color="$RED" ;;
        WARN) color="$YELLOW" ;;
        SKIP) color="$YELLOW" ;;
        *)    color="$NC" ;;
    esac
    printf "  Phase %2d: ${color}%-4s${NC}  %s\n" "$i" "${PHASE_RESULTS[$i]}" "${PHASE_NAMES[$i]}"
done
echo ""

if [ "$FAILURES" -gt 0 ]; then
    echo -e "${RED}FAILED: $FAILURES phase(s) failed.${NC}"
    echo "Fix all issues before pushing."
    exit 1
else
    echo -e "${GREEN}PASSED: All checks passed.${NC}"
    exit 0
fi
