#!/usr/bin/env bash
# install-hooks.sh — Install the pre-commit hook into .git/hooks/
#
# Usage:
#   bash scripts/install-hooks.sh
#
# This script installs a pre-commit hook that:
#   1. Runs scripts/pre-commit-llm.py (line-limit check + skills index generation)
#   2. Optionally uses the pre-commit framework if it is installed
#
# Hook behaviour:
#   On every commit : llm-line-limit, cargo-fmt, cargo-clippy
#   On push only    : cargo-test (too slow for every commit)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOOKS_DIR="${REPO_ROOT}/.git/hooks"
HOOK_FILE="${HOOKS_DIR}/pre-commit"
PUSH_HOOK_FILE="${HOOKS_DIR}/pre-push"

if [ ! -d "${HOOKS_DIR}" ]; then
    echo "Error: .git/hooks/ not found. Are you in a git repository?" >&2
    exit 1
fi

# Check if the pre-commit framework is available
if command -v pre-commit &>/dev/null; then
    echo "pre-commit framework detected — installing via 'pre-commit install'..."
    cd "${REPO_ROOT}"
    pre-commit install
    pre-commit install --hook-type pre-push
    echo "Done. Hooks installed (pre-commit + pre-push)."
    exit 0
fi

# Fallback: write a minimal shell hook for pre-commit
cat > "${HOOK_FILE}" << 'HOOK_SCRIPT'
#!/usr/bin/env bash
# Auto-generated pre-commit hook — managed by scripts/install-hooks.sh
# To update, re-run: bash scripts/install-hooks.sh

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"

# ── LLM line limit + skills index ─────────────────────────────────────────
python3 "${REPO_ROOT}/scripts/pre-commit-llm.py"

# ── Cargo fmt check ───────────────────────────────────────────────────────
if ! cargo fmt --all -- --check; then
    echo ""
    echo "Commit aborted: cargo fmt check failed."
    echo "Run 'cargo fmt' to fix formatting, then re-stage and commit."
    exit 1
fi

# ── Cargo clippy ──────────────────────────────────────────────────────────
if ! cargo clippy --all-targets --all-features -- -D warnings; then
    echo ""
    echo "Commit aborted: cargo clippy reported warnings or errors."
    echo "Fix the issues above, then re-stage and commit."
    exit 1
fi

echo "All pre-commit checks passed."
HOOK_SCRIPT

chmod +x "${HOOK_FILE}"

# Fallback: write a minimal shell hook for pre-push (runs cargo test)
cat > "${PUSH_HOOK_FILE}" << 'PUSH_SCRIPT'
#!/usr/bin/env bash
# Auto-generated pre-push hook — managed by scripts/install-hooks.sh
# To update, re-run: bash scripts/install-hooks.sh

set -euo pipefail

# ── Cargo test ────────────────────────────────────────────────────────────
if ! cargo test --all-features; then
    echo ""
    echo "Push aborted: cargo test failed."
    echo "Fix the failing tests above, then re-push."
    exit 1
fi

echo "All pre-push checks passed."
PUSH_SCRIPT

chmod +x "${PUSH_HOOK_FILE}"

echo "Pre-commit hook installed at: ${HOOK_FILE}"
echo "Pre-push hook installed at:   ${PUSH_HOOK_FILE}"
echo ""
echo "The pre-commit hook runs on every 'git commit':"
echo "  1. scripts/pre-commit-llm.py  (line-limit + skills index)"
echo "  2. cargo fmt --all -- --check"
echo "  3. cargo clippy --all-targets --all-features -- -D warnings"
echo ""
echo "The pre-push hook runs on every 'git push':"
echo "  1. cargo test --all-features"
echo ""
echo "Tip: Install the pre-commit framework for richer hook management:"
echo "  pip install pre-commit && pre-commit install && pre-commit install --hook-type pre-push"
