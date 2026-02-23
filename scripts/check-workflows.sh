#!/usr/bin/env bash
# check-workflows.sh — Local lint and hygiene checks for CI workflows and scripts.
#
# Runs actionlint, yamllint, and shellcheck against the repository's GitHub
# Actions workflows and shell scripts. Each tool is optional — if a tool is
# not installed the corresponding phase is skipped with a warning.
#
# Exit codes:
#   0 — no violations found (or all tools skipped)
#   1 — one or more violations detected

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

VIOLATIONS=0

TMP_TOOLCHAIN_VIOLATIONS="$(mktemp -t signal-fish-toolchain-violations.XXXXXX)"

# shellcheck disable=SC2317  # trap handler invoked indirectly
cleanup() {
    rm -f "$TMP_TOOLCHAIN_VIOLATIONS"
}

trap cleanup EXIT

echo -e "${YELLOW}=== Workflow lint and hygiene check ===${NC}"
echo ""

# ── Phase 1: actionlint — validate GitHub Actions workflow syntax ─────
echo -e "${YELLOW}Phase 1: Running actionlint on .github/workflows/...${NC}"

if ! command -v actionlint &>/dev/null; then
    echo -e "${YELLOW}SKIP: actionlint is not installed.${NC}"
    echo "  Install: go install github.com/rhysd/actionlint/cmd/actionlint@v1.7.7"
    echo "       or: brew install actionlint"
else
    if actionlint -color 2>&1; then
        echo -e "${GREEN}Phase 1: PASS${NC}"
    else
        echo -e "${RED}Phase 1: FAIL${NC}"
        VIOLATIONS=$((VIOLATIONS + 1))
    fi
fi
echo ""

# ── Phase 2: yamllint — validate YAML style and syntax ───────────────
echo -e "${YELLOW}Phase 2: Running yamllint on .github/workflows/...${NC}"

if ! command -v yamllint &>/dev/null; then
    echo -e "${YELLOW}SKIP: yamllint is not installed.${NC}"
    echo "  Install: pip install yamllint"
else
    if yamllint --strict .github/workflows/ .github/dependabot.yml .yamllint.yml 2>&1; then
        echo -e "${GREEN}Phase 2: PASS${NC}"
    else
        echo -e "${RED}Phase 2: FAIL${NC}"
        VIOLATIONS=$((VIOLATIONS + 1))
    fi
fi
echo ""

# ── Phase 3: shellcheck — lint shell scripts ─────────────────────────
echo -e "${YELLOW}Phase 3: Running shellcheck on scripts/*.sh...${NC}"

if ! command -v shellcheck &>/dev/null; then
    echo -e "${YELLOW}SKIP: shellcheck is not installed.${NC}"
    echo "  Install: apt install shellcheck"
    echo "       or: brew install shellcheck"
else
    # Guard: at least one .sh file must exist for shellcheck
    if compgen -G "scripts/*.sh" > /dev/null; then
        if shellcheck scripts/*.sh 2>&1; then
            echo -e "${GREEN}Phase 3: PASS${NC}"
        else
            echo -e "${RED}Phase 3: FAIL${NC}"
            VIOLATIONS=$((VIOLATIONS + 1))
        fi
    else
        echo -e "${YELLOW}SKIP: No .sh files found in scripts/${NC}"
    fi
fi
echo ""

# ── Phase 4: rust-toolchain usage guard — catch MSRV misconfiguration ──
echo -e "${YELLOW}Phase 4: Checking dtolnay/rust-toolchain usage patterns...${NC}"

if grep -R -n -E 'uses:[[:space:]]*dtolnay/rust-toolchain@[0-9]' .github/workflows/*.yml >"$TMP_TOOLCHAIN_VIOLATIONS"; then
    echo -e "${RED}Phase 4: FAIL${NC}"
    echo "Found numeric dtolnay/rust-toolchain refs (e.g. @1.x, @2.x, @3) in workflow files:"
    cat "$TMP_TOOLCHAIN_VIOLATIONS"
    echo ""
    echo "Action: Use 'dtolnay/rust-toolchain@stable' and set the Rust version via:"
    echo "  with:"
    echo "    toolchain: <msrv-version>"
    VIOLATIONS=$((VIOLATIONS + 1))
else
    grep_status=$?

    if [ "$grep_status" -gt 1 ]; then
        echo -e "${RED}Phase 4: FAIL${NC}"
        echo "Error: grep execution failed while scanning workflow files for numeric dtolnay refs."
        echo "Action: Verify .github/workflows/*.yml files are readable and retry."
        VIOLATIONS=$((VIOLATIONS + 1))
    else
        CARGO_MSRV="$(awk -F'"' '/^rust-version[[:space:]]*=[[:space:]]*"/ {print $2; exit}' Cargo.toml)"

        if [ -z "$CARGO_MSRV" ]; then
            echo -e "${RED}Phase 4: FAIL${NC}"
            echo "Could not read rust-version from Cargo.toml."
            echo "Action: Add a quoted rust-version (e.g., rust-version = \"1.85.0\") to Cargo.toml."
            VIOLATIONS=$((VIOLATIONS + 1))
        else
            CI_MSRV_BLOCK="$(awk '
                /^  msrv:/ {in_msrv=1}
                in_msrv && /^  [a-zA-Z0-9-]+:/ && $0 !~ /^  msrv:/ {exit}
                in_msrv {print}
            ' .github/workflows/ci.yml)"

            if [ -z "$CI_MSRV_BLOCK" ]; then
                echo -e "${RED}Phase 4: FAIL${NC}"
                echo "Action: Add an 'msrv' job to .github/workflows/ci.yml with explicit rust-toolchain setup."
                VIOLATIONS=$((VIOLATIONS + 1))
            elif [[ "$CI_MSRV_BLOCK" != *"uses: dtolnay/rust-toolchain@stable"* ]]; then
                echo -e "${RED}Phase 4: FAIL${NC}"
                echo "MSRV job exists but does not use 'dtolnay/rust-toolchain@stable' in .github/workflows/ci.yml."
                echo "Action: In the msrv job, use:"
                echo "  - uses: dtolnay/rust-toolchain@stable"
                echo "    with:"
                echo "      toolchain: <msrv-version-from-Cargo.toml>"
                echo ""
                echo "Current extracted msrv block:"
                echo "$CI_MSRV_BLOCK"
                VIOLATIONS=$((VIOLATIONS + 1))
            else
                CI_MSRV_TOOLCHAIN="$(printf '%s\n' "$CI_MSRV_BLOCK" | awk '/toolchain:[[:space:]]*/ {sub(/.*toolchain:[[:space:]]*/, "", $0); gsub(/[[:space:]]+$/, "", $0); print; exit}')"
                CI_MSRV_TOOLCHAIN="${CI_MSRV_TOOLCHAIN%\"}"
                CI_MSRV_TOOLCHAIN="${CI_MSRV_TOOLCHAIN#\"}"
                CI_MSRV_TOOLCHAIN="${CI_MSRV_TOOLCHAIN%\'}"
                CI_MSRV_TOOLCHAIN="${CI_MSRV_TOOLCHAIN#\'}"

                if [ -z "$CI_MSRV_TOOLCHAIN" ]; then
                    echo -e "${RED}Phase 4: FAIL${NC}"
                    echo "MSRV job exists but does not set an explicit 'with.toolchain' value in .github/workflows/ci.yml."
                    echo "Action: In the msrv job, set:"
                    echo "  with:"
                    echo "    toolchain: $CARGO_MSRV"
                    echo ""
                    echo "Current extracted msrv block:"
                    echo "$CI_MSRV_BLOCK"
                    VIOLATIONS=$((VIOLATIONS + 1))
                elif [ "$CI_MSRV_TOOLCHAIN" != "$CARGO_MSRV" ]; then
                    echo -e "${RED}Phase 4: FAIL${NC}"
                    echo "MSRV mismatch: Cargo.toml rust-version is '$CARGO_MSRV' but ci.yml msrv toolchain is '$CI_MSRV_TOOLCHAIN'."
                    echo "Action: Update .github/workflows/ci.yml msrv toolchain to match Cargo.toml rust-version."
                    echo ""
                    echo "Current extracted msrv block:"
                    echo "$CI_MSRV_BLOCK"
                    VIOLATIONS=$((VIOLATIONS + 1))
                else
                    echo -e "${GREEN}Phase 4: PASS${NC}"
                fi
            fi
        fi
    fi
fi
echo ""

# ── Result ────────────────────────────────────────────────────────────
if [ "$VIOLATIONS" -gt 0 ]; then
    echo -e "${RED}FAILED: $VIOLATIONS violation(s) found.${NC}"
    echo "Fix all workflow and script issues before committing."
    exit 1
else
    echo -e "${GREEN}PASSED: No workflow or script violations found.${NC}"
    exit 0
fi
