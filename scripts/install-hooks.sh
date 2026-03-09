#!/usr/bin/env bash
# install-hooks.sh — Install the pre-commit hook into .git/hooks/
#
# Usage:
#   bash scripts/install-hooks.sh
#
# This script installs a pre-commit hook that:
#   1. Runs scripts/pre-commit-llm.py (line-limit check + skills index generation)
#   2. Runs scripts/test_pre_commit_llm.py with pytest if available
#   3. Runs markdownlint if available (to catch docs formatting drift early)
#   4. Runs scripts/test_shell_portability.sh (shell portability checks)
#   5. Runs scripts/check-test-io-unwrap.sh (Rust test I/O unwrap check)
#   6. Runs scripts/check-workflows.sh (workflow guard checks)
#   7. Optionally uses the pre-commit framework if it is installed
#
# Hook behavior:
#   On every commit : llm-line-limit, markdownlint (optional), workflow guards,
#                     cargo-fmt, cargo-clippy, typos (optional)
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

# ── Python tests for pre-commit hook logic (optional) ─────────────────────
if command -v pytest &>/dev/null; then
    if ! pytest -q "${REPO_ROOT}/scripts/test_pre_commit_llm.py"; then
        echo ""
        echo "Commit aborted: pre-commit LLM script tests failed."
        echo "Fix the failing tests above, then re-stage and commit."
        exit 1
    fi
else
    echo "Note: pytest is not installed — skipping pre-commit LLM script tests."
fi

# ── Markdown lint (optional) ───────────────────────────────────────────────
if command -v markdownlint-cli2 &>/dev/null; then
    if ! markdownlint-cli2 "**/*.md"; then
        echo ""
        echo "Commit aborted: markdownlint-cli2 reported Markdown issues."
        echo "Fix the markdown issues above, then re-stage and commit."
        exit 1
    fi
elif command -v markdownlint &>/dev/null; then
    if ! markdownlint "**/*.md"; then
        echo ""
        echo "Commit aborted: markdownlint reported Markdown issues."
        echo "Fix the markdown issues above, then re-stage and commit."
        exit 1
    fi
else
    echo "Note: markdownlint is not installed — skipping markdown lint."
    echo "  Install: npm install -g markdownlint-cli2"
fi

# ── Shell lint (optional) ──────────────────────────────────────────────────
if command -v shellcheck &>/dev/null; then
    if ! shellcheck "${REPO_ROOT}"/scripts/*.sh; then
        echo ""
        echo "Commit aborted: shellcheck reported shell script issues."
        echo "Fix the shell issues above, then re-stage and commit."
        exit 1
    fi
else
    echo "Note: shellcheck is not installed — skipping shell lint."
    echo "  Install: apt install shellcheck"
    echo "       or: brew install shellcheck"
fi

# ── TOML config validation ────────────────────────────────────────────────
TOML_FAIL=0
TOML_HAS_PARSER=false
for toml_file in "${REPO_ROOT}"/*.toml "${REPO_ROOT}"/.*.toml; do
    [ -f "$toml_file" ] || continue
    if command -v python3 &>/dev/null; then
        # Exit 0 = valid TOML, exit 2 = no parser available, exit 1 = invalid
        TOML_EXIT=0
        python3 -c "
import sys
try:
    import tomllib
except ImportError:
    try:
        import tomli as tomllib
    except ImportError:
        try:
            import toml
        except ImportError:
            sys.exit(2)
        toml.load(sys.argv[1])
        sys.exit(0)
with open(sys.argv[1], 'rb') as f:
    tomllib.load(f)
" "$toml_file" 2>/dev/null || TOML_EXIT=$?

        if [ "$TOML_EXIT" -eq 0 ]; then
            TOML_HAS_PARSER=true
        elif [ "$TOML_EXIT" -eq 2 ]; then
            : # No parser available — skip, do not report as error
        else
            TOML_HAS_PARSER=true
            echo "TOML parse error: $toml_file"
            TOML_FAIL=1
        fi
    fi
done
if [ "$TOML_FAIL" -ne 0 ]; then
    echo ""
    echo "Commit aborted: one or more TOML config files failed to parse."
    echo "Fix the TOML syntax errors above, then re-stage and commit."
    exit 1
fi
if [ "$TOML_HAS_PARSER" = false ] && command -v python3 &>/dev/null; then
    echo "Note: no Python TOML parser available — skipping TOML validation."
    echo "  Python 3.11+ includes tomllib; or install: pip install tomli"
fi

# ── FFI safety check ──────────────────────────────────────────────────────
if [ -f "${REPO_ROOT}/scripts/check-ffi-safety.sh" ]; then
    if ! bash "${REPO_ROOT}/scripts/check-ffi-safety.sh"; then
        echo ""
        echo "Commit aborted: FFI safety check failed."
        echo "Fix the FFI safety violations above, then re-stage and commit."
        exit 1
    fi
else
    echo "Note: scripts/check-ffi-safety.sh not found — skipping FFI safety check."
fi

# ── FFI safety script tests ──────────────────────────────────────────────
if [ -f "${REPO_ROOT}/scripts/test_check_ffi_safety.sh" ]; then
    if ! bash "${REPO_ROOT}/scripts/test_check_ffi_safety.sh"; then
        echo ""
        echo "Commit aborted: FFI safety script tests failed."
        echo "Fix the test failures above, then re-stage and commit."
        exit 1
    fi
else
    echo "Note: scripts/test_check_ffi_safety.sh not found — skipping FFI safety script tests."
fi

# ── Target-gated doc-link check ─────────────────────────────────────────
if [ -f "${REPO_ROOT}/scripts/check-target-gated-doc-links.sh" ]; then
    if ! bash "${REPO_ROOT}/scripts/check-target-gated-doc-links.sh"; then
        echo ""
        echo "Commit aborted: target-gated doc-link check failed."
        echo "Fix the doc-link violations above, then re-stage and commit."
        exit 1
    fi
else
    echo "Note: scripts/check-target-gated-doc-links.sh not found — skipping target-gated doc-link check."
fi

# ── Target-gated doc-link script tests ──────────────────────────────────
if [ -f "${REPO_ROOT}/scripts/test_check_target_gated_doc_links.sh" ]; then
    if ! bash "${REPO_ROOT}/scripts/test_check_target_gated_doc_links.sh"; then
        echo ""
        echo "Commit aborted: target-gated doc-link script tests failed."
        echo "Fix the test failures above, then re-stage and commit."
        exit 1
    fi
else
    echo "Note: scripts/test_check_target_gated_doc_links.sh not found — skipping target-gated doc-link script tests."
fi

# ── Shell portability checks ──────────────────────────────────────────────
if [ -f "${REPO_ROOT}/scripts/test_shell_portability.sh" ]; then
    if ! bash "${REPO_ROOT}/scripts/test_shell_portability.sh"; then
        echo ""
        echo "Commit aborted: shell portability checks failed."
        echo "Fix the portability violations above, then re-stage and commit."
        exit 1
    fi
else
    echo "Note: scripts/test_shell_portability.sh not found — skipping shell portability checks."
fi

# ── Rust test I/O unwrap check ───────────────────────────────────────────
if [ -f "${REPO_ROOT}/scripts/check-test-io-unwrap.sh" ]; then
    if ! bash "${REPO_ROOT}/scripts/check-test-io-unwrap.sh"; then
        echo ""
        echo "Commit aborted: Rust test I/O unwrap check failed."
        echo "Fix the violations above, then re-stage and commit."
        exit 1
    fi
else
    echo "Note: scripts/check-test-io-unwrap.sh not found — skipping Rust test I/O unwrap check."
fi

# ── Workflow guard checks ───────────────────────────────────────────────────
if [ -f "${REPO_ROOT}/scripts/check-workflows.sh" ]; then
    if ! bash "${REPO_ROOT}/scripts/check-workflows.sh"; then
        echo ""
        echo "Commit aborted: workflow guard checks failed."
        echo "Fix the workflow issues above, then re-stage and commit."
        exit 1
    fi
else
    echo "Note: scripts/check-workflows.sh not found — skipping workflow guard checks."
fi

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

# ── Spell check (typos) — optional ───────────────────────────────────────
if command -v typos &>/dev/null; then
    if [ -f "${REPO_ROOT}/.typos.toml" ]; then
        if ! typos --config "${REPO_ROOT}/.typos.toml"; then
            echo ""
            echo "Commit aborted: typos spell check found errors."
            echo "Fix the spelling issues above, then re-stage and commit."
            echo "To add exceptions, edit .typos.toml."
            exit 1
        fi
    fi
else
    echo "Note: typos is not installed — skipping spell check."
    echo "  Install: cargo install typos-cli"
fi

echo "All pre-commit checks passed."
HOOK_SCRIPT

chmod +x "${HOOK_FILE}"

# Fallback: write a minimal shell hook for pre-push (runs cargo test + CI scripts)
cat > "${PUSH_HOOK_FILE}" << 'PUSH_SCRIPT'
#!/usr/bin/env bash
# Auto-generated pre-push hook — managed by scripts/install-hooks.sh
# To update, re-run: bash scripts/install-hooks.sh

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"

# ── Cargo clippy (no-default-features) ────────────────────────────────────
if ! cargo clippy --all-targets --no-default-features -- -D warnings; then
    echo ""
    echo "Push aborted: cargo clippy (no-default-features) reported warnings or errors."
    echo "Fix the issues above, then re-push."
    exit 1
fi

# ── Cargo test ────────────────────────────────────────────────────────────
if ! cargo test --all-features; then
    echo ""
    echo "Push aborted: cargo test failed."
    echo "Fix the failing tests above, then re-push."
    exit 1
fi

# ── Panic-free policy check ──────────────────────────────────────────────
if [ -f "${REPO_ROOT}/scripts/check-no-panics.sh" ]; then
    if ! bash "${REPO_ROOT}/scripts/check-no-panics.sh"; then
        echo ""
        echo "Push aborted: panic-free policy check failed."
        echo "Fix the violations above, then re-push."
        exit 1
    fi
else
    echo "Note: scripts/check-no-panics.sh not found — skipping panic-free policy check."
fi

# ── Markdown snippet compilation check ───────────────────────────────────
if [ -f "${REPO_ROOT}/scripts/extract-rust-snippets.sh" ]; then
    if ! bash "${REPO_ROOT}/scripts/extract-rust-snippets.sh"; then
        echo ""
        echo "Push aborted: markdown snippet compilation check failed."
        echo "Fix the snippet issues above, then re-push."
        exit 1
    fi
else
    echo "Note: scripts/extract-rust-snippets.sh not found — skipping snippet check."
fi

# ── Docs rendering check (optional — requires mkdocs) ───────────────────
if [ -f "${REPO_ROOT}/scripts/pre-commit-docs.sh" ]; then
    if ! bash "${REPO_ROOT}/scripts/pre-commit-docs.sh"; then
        echo ""
        echo "Push aborted: docs rendering check failed."
        echo "Fix the rendering issues above, then re-push."
        exit 1
    fi
else
    echo "Note: scripts/pre-commit-docs.sh not found — skipping docs rendering check."
fi

echo "All pre-push checks passed."
PUSH_SCRIPT

chmod +x "${PUSH_HOOK_FILE}"

echo "Pre-commit hook installed at: ${HOOK_FILE}"
echo "Pre-push hook installed at:   ${PUSH_HOOK_FILE}"
echo ""
echo "The pre-commit hook runs on every 'git commit':"
echo "  1. scripts/pre-commit-llm.py  (line-limit + skills index)"
echo "  2. pytest -q scripts/test_pre_commit_llm.py (optional, skipped if not installed)"
echo "  3. markdownlint on **/*.md     (optional, skipped if not installed)"
echo "  4. shellcheck scripts/*.sh     (optional, skipped if not installed)"
echo "  5. TOML config validation     (optional, requires python3)"
echo "  6. bash scripts/check-ffi-safety.sh (FFI safety check)"
echo "  7. bash scripts/test_check_ffi_safety.sh (FFI safety script tests)"
echo "  8. bash scripts/check-target-gated-doc-links.sh (target-gated doc-link check)"
echo "  9. bash scripts/test_check_target_gated_doc_links.sh (target-gated doc-link script tests)"
echo " 10. bash scripts/test_shell_portability.sh (shell portability checks)"
echo " 11. bash scripts/check-test-io-unwrap.sh (Rust test I/O unwrap check)"
echo " 12. bash scripts/check-workflows.sh"
echo " 13. cargo fmt --all -- --check"
echo " 14. cargo clippy --all-targets --all-features -- -D warnings"
echo " 15. typos --config .typos.toml  (spell check — optional, skipped if not installed)"
echo ""
echo "The pre-push hook runs on every 'git push':"
echo "  1. cargo clippy --all-targets --no-default-features -- -D warnings"
echo "  2. cargo test --all-features"
echo "  3. bash scripts/check-no-panics.sh (panic-free policy)"
echo "  4. bash scripts/extract-rust-snippets.sh (markdown snippet compilation)"
echo "  5. bash scripts/pre-commit-docs.sh (docs rendering — optional, skipped if mkdocs not installed)"
echo ""
echo "Tip: Install the pre-commit framework for richer hook management:"
echo "  pip install pre-commit && pre-commit install && pre-commit install --hook-type pre-push"
