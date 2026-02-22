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

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

VIOLATIONS=0

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

# ── Result ────────────────────────────────────────────────────────────
if [ "$VIOLATIONS" -gt 0 ]; then
    echo -e "${RED}FAILED: $VIOLATIONS violation(s) found.${NC}"
    echo "Fix all workflow and script issues before committing."
    exit 1
else
    echo -e "${GREEN}PASSED: No workflow or script violations found.${NC}"
    exit 0
fi
