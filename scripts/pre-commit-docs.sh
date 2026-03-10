#!/usr/bin/env bash
# pre-commit-docs.sh — Local pre-commit hook for documentation validation.
#
# Runs a lightweight subset of the docs validation checks that developers
# can use locally before committing documentation changes. Requires Python
# and the MkDocs dependencies from requirements-docs.txt.
#
# Usage:
#   bash scripts/pre-commit-docs.sh
#
# This script is intentionally optional — it requires Python + MkDocs
# dependencies that not all developers may have installed. The full
# rendering check runs in CI via the docs-validation.yml workflow.
#
# Exit codes:
#   0 — all checks passed (or mkdocs not available)
#   1 — one or more checks failed

set -euo pipefail

# ── Resolve paths relative to this script ────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# ── Color constants ──────────────────────────────────────────────────
RED='\033[0;31m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

# ── Preflight: verify mkdocs is available ────────────────────────────
# Honor the MKDOCS env var and fallback path, matching check-docs-rendering.sh.
if [ -n "${MKDOCS:-}" ] && [ -x "$MKDOCS" ]; then
    : # MKDOCS env var points to a valid executable — proceed
elif command -v mkdocs &>/dev/null; then
    : # mkdocs found in PATH — proceed
elif [ -x "/tmp/docs-venv/bin/mkdocs" ]; then
    : # mkdocs found at CI fallback path — proceed
else
    echo -e "${YELLOW}SKIP: mkdocs is not installed — skipping docs rendering check.${NC}"
    echo "  Install: pip install -r requirements-docs.txt"
    echo "  Or set:  MKDOCS=/path/to/mkdocs"
    exit 0
fi

# ── Delegate to the full rendering check script ──────────────────────
if [ -f "$SCRIPT_DIR/check-docs-rendering.sh" ]; then
    exec bash "$SCRIPT_DIR/check-docs-rendering.sh"
else
    echo -e "${RED}ERROR: scripts/check-docs-rendering.sh not found.${NC}" >&2
    exit 1
fi
