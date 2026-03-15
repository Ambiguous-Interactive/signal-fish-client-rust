#!/usr/bin/env bash
# install-hooks.sh — Install parallel pre-commit and pre-push hooks into .git/hooks/
#
# Usage:
#   bash scripts/install-hooks.sh
#
# This script installs a pre-commit hook that runs checks in parallel:
#   1.  scripts/pre-commit-llm.py  (line-limit + skills index)
#   2.  pytest -q scripts/test_pre_commit_llm.py (optional)
#   3.  markdownlint on **/*.md     (optional)
#   4.  shellcheck scripts/*.sh     (optional)
#   5.  scripts/check-ffi-safety.sh
#   6.  scripts/test_check_ffi_safety.sh
#   7.  scripts/check-target-gated-doc-links.sh
#   8.  scripts/test_check_target_gated_doc_links.sh
#   9.  scripts/test_shell_portability.sh
#  10.  scripts/check-test-io-unwrap.sh
#  11.  scripts/check-workflows.sh
#  12.  cargo fmt --all -- --check (skipped if no Rust files staged)
#  13.  cargo clippy --all-targets --all-features -- -D warnings (skipped if no Rust files staged)
#  14.  typos --config .typos.toml  (optional)
#  15.  TOML config validation      (optional)
#
# And a pre-push hook that runs checks in two phases:
#   Phase 1 (parallel, background — no target/ access):
#     3. scripts/check-no-panics.sh (phases 1-2 only)
#     4. scripts/extract-rust-snippets.sh
#     5. scripts/pre-commit-docs.sh (optional)
#   Phase 2 (sequential, foreground — shared target/):
#     1. cargo clippy --all-targets --no-default-features -- -D warnings
#     2. cargo test --all-features
#     6. cargo machete (optional — unused dependency heuristic check)
#
# Hook behavior:
#   On every commit : 1-11,13-15 run in parallel; 12 (cargo fmt) runs in foreground before 13
#   On push only    : non-cargo checks run in background; cargo commands run sequentially
#
# NOTE: .pre-commit-config.yaml is kept as documentation reference only.
# This script always installs custom parallel hooks for speed.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOOKS_DIR="${REPO_ROOT}/.git/hooks"
HOOK_FILE="${HOOKS_DIR}/pre-commit"
PUSH_HOOK_FILE="${HOOKS_DIR}/pre-push"

if [ ! -d "${HOOKS_DIR}" ]; then
    echo "Error: .git/hooks/ not found. Are you in a git repository?" >&2
    exit 1
fi

# ── Pre-commit hook (parallel) ───────────────────────────────────────────
cat > "${HOOK_FILE}" << 'HOOK_SCRIPT'
#!/usr/bin/env bash
# Auto-generated pre-commit hook — managed by scripts/install-hooks.sh
# Runs all checks in parallel for maximum speed.
# To update, re-run: bash scripts/install-hooks.sh

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"

# ── Temp directory for parallel check results ────────────────────────
CHECK_TMPDIR="$(mktemp -d)"
trap 'rm -rf "$CHECK_TMPDIR"' EXIT

# ── Timing helper ────────────────────────────────────────────────────
# Returns current time in nanoseconds (Linux) or padded seconds (macOS fallback)
_now() {
    local t
    t=$(date +%s%N 2>/dev/null)
    if [ -n "$t" ] && [ "$t" = "${t%%[!0-9]*}" ] && [ ${#t} -gt 10 ]; then
        printf '%s' "$t"
    else
        printf '%s' "$(date +%s)000000000"
    fi
}

# ── Elapsed time computation ─────────────────────────────────────────
_elapsed() {
    local start=$1 end=$2
    local diff=$(( end - start ))
    local sec=$(( diff / 1000000000 ))
    local tenths=$(( (diff % 1000000000) / 100000000 ))
    printf '%d.%d' "$sec" "$tenths"
}

# ── Run a single check (foreground or background) ────────────────────
run_check() {
    local name="$1" id="$2"
    shift 2
    local start end elapsed
    start=$(_now)
    if "$@" > "$CHECK_TMPDIR/$id.stdout" 2> "$CHECK_TMPDIR/$id.stderr"; then
        end=$(_now)
        elapsed=$(_elapsed "$start" "$end")
        printf 'PASS %s %s\n' "$elapsed" "$name" > "$CHECK_TMPDIR/$id.result"
    else
        end=$(_now)
        elapsed=$(_elapsed "$start" "$end")
        printf 'FAIL %s %s\n' "$elapsed" "$name" > "$CHECK_TMPDIR/$id.result"
    fi
}

# ── Detect staged file types ─────────────────────────────────────────
HAS_RUST_FILES=false
if git diff --cached --name-only --diff-filter=ACMR | grep -qE '\.(rs)$|Cargo\.(toml|lock)$'; then
    HAS_RUST_FILES=true
fi

OVERALL_START=$(_now)
echo "Pre-commit checks (parallel)..."

PIDS=()

# ── 1. LLM line limit + skills index ────────────────────────────────
run_check "LLM line limit" "01-llm" \
    python3 "${REPO_ROOT}/scripts/pre-commit-llm.py" &
PIDS+=($!)

# ── 2. Python tests for pre-commit hook logic (optional) ────────────
if command -v pytest &>/dev/null; then
    run_check "pytest pre-commit" "02-pytest" \
        pytest -q "${REPO_ROOT}/scripts/test_pre_commit_llm.py" &
    PIDS+=($!)
else
    printf 'SKIP 0.0 pytest pre-commit (not installed)\n' > "$CHECK_TMPDIR/02-pytest.result"
fi

# ── 3. Markdown lint (optional) ──────────────────────────────────────
if command -v markdownlint-cli2 &>/dev/null; then
    run_check "markdownlint" "03-mdlint" \
        markdownlint-cli2 "**/*.md" &
    PIDS+=($!)
elif command -v markdownlint &>/dev/null; then
    run_check "markdownlint" "03-mdlint" \
        markdownlint "**/*.md" &
    PIDS+=($!)
else
    printf 'SKIP 0.0 markdownlint (not installed)\n' > "$CHECK_TMPDIR/03-mdlint.result"
fi

# ── 4. Shell lint (optional) ─────────────────────────────────────────
if command -v shellcheck &>/dev/null; then
    run_check "shellcheck" "04-shellcheck" \
        shellcheck "${REPO_ROOT}"/scripts/*.sh &
    PIDS+=($!)
else
    printf 'SKIP 0.0 shellcheck (not installed)\n' > "$CHECK_TMPDIR/04-shellcheck.result"
fi

# ── 5. FFI safety check ─────────────────────────────────────────────
if [ -f "${REPO_ROOT}/scripts/check-ffi-safety.sh" ]; then
    run_check "FFI safety" "05-ffi" \
        bash "${REPO_ROOT}/scripts/check-ffi-safety.sh" &
    PIDS+=($!)
else
    printf 'SKIP 0.0 FFI safety (script not found)\n' > "$CHECK_TMPDIR/05-ffi.result"
fi

# ── 6. FFI safety script tests ──────────────────────────────────────
if [ -f "${REPO_ROOT}/scripts/test_check_ffi_safety.sh" ]; then
    run_check "FFI safety tests" "06-ffi-test" \
        bash "${REPO_ROOT}/scripts/test_check_ffi_safety.sh" &
    PIDS+=($!)
else
    printf 'SKIP 0.0 FFI safety tests (script not found)\n' > "$CHECK_TMPDIR/06-ffi-test.result"
fi

# ── 7. Target-gated doc-link check ──────────────────────────────────
if [ -f "${REPO_ROOT}/scripts/check-target-gated-doc-links.sh" ]; then
    run_check "doc-link check" "07-doclink" \
        bash "${REPO_ROOT}/scripts/check-target-gated-doc-links.sh" &
    PIDS+=($!)
else
    printf 'SKIP 0.0 doc-link check (script not found)\n' > "$CHECK_TMPDIR/07-doclink.result"
fi

# ── 8. Target-gated doc-link script tests ────────────────────────────
if [ -f "${REPO_ROOT}/scripts/test_check_target_gated_doc_links.sh" ]; then
    run_check "doc-link tests" "08-doclink-test" \
        bash "${REPO_ROOT}/scripts/test_check_target_gated_doc_links.sh" &
    PIDS+=($!)
else
    printf 'SKIP 0.0 doc-link tests (script not found)\n' > "$CHECK_TMPDIR/08-doclink-test.result"
fi

# ── 9. Shell portability checks ─────────────────────────────────────
if [ -f "${REPO_ROOT}/scripts/test_shell_portability.sh" ]; then
    run_check "shell portability" "09-shellport" \
        bash "${REPO_ROOT}/scripts/test_shell_portability.sh" &
    PIDS+=($!)
else
    printf 'SKIP 0.0 shell portability (script not found)\n' > "$CHECK_TMPDIR/09-shellport.result"
fi

# ── 10. Rust test I/O unwrap check ──────────────────────────────────
if [ -f "${REPO_ROOT}/scripts/check-test-io-unwrap.sh" ]; then
    run_check "test I/O unwrap" "10-iounwrap" \
        bash "${REPO_ROOT}/scripts/check-test-io-unwrap.sh" &
    PIDS+=($!)
else
    printf 'SKIP 0.0 test I/O unwrap (script not found)\n' > "$CHECK_TMPDIR/10-iounwrap.result"
fi

# ── 11. Workflow guard checks ────────────────────────────────────────
if [ -f "${REPO_ROOT}/scripts/check-workflows.sh" ]; then
    run_check "workflow guards" "11-workflows" \
        bash "${REPO_ROOT}/scripts/check-workflows.sh" &
    PIDS+=($!)
else
    printf 'SKIP 0.0 workflow guards (script not found)\n' > "$CHECK_TMPDIR/11-workflows.result"
fi

# ── 12. Cargo fmt check (skip if no Rust files staged) ──────────────
# Run cargo fmt in the foreground (fast, no compilation) before
# backgrounding clippy.  Both contend for the Cargo package lock, so
# running them in parallel causes lock contention with no real speedup.
if [ "$HAS_RUST_FILES" = true ]; then
    run_check "cargo fmt" "12-cargofmt" \
        cargo fmt --all -- --check
else
    printf 'SKIP 0.0 cargo fmt (no Rust files staged)\n' > "$CHECK_TMPDIR/12-cargofmt.result"
fi

# ── 13. Cargo clippy (skip if no Rust files staged) ─────────────────
if [ "$HAS_RUST_FILES" = true ]; then
    run_check "cargo clippy" "13-clippy" \
        cargo clippy --all-targets --all-features -- -D warnings &
    PIDS+=($!)
else
    printf 'SKIP 0.0 cargo clippy (no Rust files staged)\n' > "$CHECK_TMPDIR/13-clippy.result"
fi

# ── 14. Spell check — typos (optional) ──────────────────────────────
if command -v typos &>/dev/null && [ -f "${REPO_ROOT}/.typos.toml" ]; then
    run_check "typos" "14-typos" \
        typos --config "${REPO_ROOT}/.typos.toml" &
    PIDS+=($!)
else
    printf 'SKIP 0.0 typos (not installed or no config)\n' > "$CHECK_TMPDIR/14-typos.result"
fi

# ── 15. TOML config validation (optional) ────────────────────────────
# This check is self-contained in a single background subshell that
# writes its own result file (since it needs special exit-code handling).
(
    TOML_START=$(_now)
    TOML_FAIL=0
    TOML_HAS_PARSER=false
    for toml_file in "${REPO_ROOT}"/*.toml "${REPO_ROOT}"/.*.toml; do
        [ -f "$toml_file" ] || continue
        if command -v python3 &>/dev/null; then
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
                : # No parser available
            else
                TOML_HAS_PARSER=true
                echo "TOML parse error: $toml_file" >> "$CHECK_TMPDIR/15-toml.stdout"
                TOML_FAIL=1
            fi
        fi
    done
    TOML_END=$(_now)
    TOML_ELAPSED=$(_elapsed "$TOML_START" "$TOML_END")
    if [ "$TOML_HAS_PARSER" = false ]; then
        printf 'SKIP 0.0 TOML validation (no parser)\n' > "$CHECK_TMPDIR/15-toml.result"
    elif [ "$TOML_FAIL" -ne 0 ]; then
        printf 'FAIL %s %s\n' "$TOML_ELAPSED" "TOML validation" > "$CHECK_TMPDIR/15-toml.result"
    else
        printf 'PASS %s %s\n' "$TOML_ELAPSED" "TOML validation" > "$CHECK_TMPDIR/15-toml.result"
    fi
) &
PIDS+=($!)

# ── Wait for all checks ─────────────────────────────────────────────
if [ ${#PIDS[@]} -gt 0 ]; then
    for pid in "${PIDS[@]}"; do
        wait "$pid" 2>/dev/null || true
    done
fi

OVERALL_END=$(_now)
OVERALL_ELAPSED=$(_elapsed "$OVERALL_START" "$OVERALL_END")

# ── Collect and display results ──────────────────────────────────────
TOTAL=0
FAILED=0
SKIPPED=0

for result_file in "$CHECK_TMPDIR"/*.result; do
    [ -f "$result_file" ] || continue
    status=$(cut -d' ' -f1 < "$result_file")
    elapsed=$(cut -d' ' -f2 < "$result_file")
    name=$(cut -d' ' -f3- < "$result_file")

    if [ "$status" = "SKIP" ]; then
        SKIPPED=$((SKIPPED + 1))
        continue
    fi

    TOTAL=$((TOTAL + 1))

    if [ "$status" = "PASS" ]; then
        printf '  \xe2\x9c\x93 %-24s (%ss)\n' "$name" "$elapsed"
    else
        printf '  \xe2\x9c\x97 %-24s (%ss)\n' "$name" "$elapsed"
        FAILED=$((FAILED + 1))
        # Show first few lines of error output
        id=$(basename "$result_file" .result)
        if [ -s "$CHECK_TMPDIR/$id.stderr" ]; then
            head -20 "$CHECK_TMPDIR/$id.stderr" | sed 's/^/    /'
        elif [ -s "$CHECK_TMPDIR/$id.stdout" ]; then
            head -20 "$CHECK_TMPDIR/$id.stdout" | sed 's/^/    /'
        fi
    fi
done

echo ""
if [ "$SKIPPED" -gt 0 ]; then
    printf '(%d checks skipped — optional tools not installed)\n' "$SKIPPED"
fi

if [ "$FAILED" -gt 0 ]; then
    printf 'FAILED: %d of %d checks failed. (%ss total)\n' "$FAILED" "$TOTAL" "$OVERALL_ELAPSED"
    echo "Fix the issues above, then re-stage and commit."
    exit 1
else
    printf 'All %d checks passed. (%ss total)\n' "$TOTAL" "$OVERALL_ELAPSED"
fi
HOOK_SCRIPT

chmod +x "${HOOK_FILE}"

# ── Pre-push hook (two-phase) ────────────────────────────────────────────
cat > "${PUSH_HOOK_FILE}" << 'PUSH_SCRIPT'
#!/usr/bin/env bash
# Auto-generated pre-push hook — managed by scripts/install-hooks.sh
# Non-cargo checks run in parallel (background); cargo commands run sequentially
# to avoid target/ cache thrashing from different feature flags.
# To update, re-run: bash scripts/install-hooks.sh

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"

# ── Temp directory for parallel check results ────────────────────────
CHECK_TMPDIR="$(mktemp -d)"
trap 'rm -rf "$CHECK_TMPDIR"' EXIT

# ── Timing helper ────────────────────────────────────────────────────
_now() {
    local t
    t=$(date +%s%N 2>/dev/null)
    if [ -n "$t" ] && [ "$t" = "${t%%[!0-9]*}" ] && [ ${#t} -gt 10 ]; then
        printf '%s' "$t"
    else
        printf '%s' "$(date +%s)000000000"
    fi
}

_elapsed() {
    local start=$1 end=$2
    local diff=$(( end - start ))
    local sec=$(( diff / 1000000000 ))
    local tenths=$(( (diff % 1000000000) / 100000000 ))
    printf '%d.%d' "$sec" "$tenths"
}

run_check() {
    local name="$1" id="$2"
    shift 2
    local start end elapsed
    start=$(_now)
    if "$@" > "$CHECK_TMPDIR/$id.stdout" 2> "$CHECK_TMPDIR/$id.stderr"; then
        end=$(_now)
        elapsed=$(_elapsed "$start" "$end")
        printf 'PASS %s %s\n' "$elapsed" "$name" > "$CHECK_TMPDIR/$id.result"
    else
        end=$(_now)
        elapsed=$(_elapsed "$start" "$end")
        printf 'FAIL %s %s\n' "$elapsed" "$name" > "$CHECK_TMPDIR/$id.result"
    fi
}

OVERALL_START=$(_now)
echo "Pre-push checks..."

PIDS=()

# ── Phase 1: Non-cargo checks (parallel, in background) ──────────
# These don't write to the project's target/ directory.

# ── 3. Panic-free policy check (phases 1-2 only) ────────────────────
if [ -f "${REPO_ROOT}/scripts/check-no-panics.sh" ]; then
    run_check "panic-free check" "03-panics" \
        bash "${REPO_ROOT}/scripts/check-no-panics.sh" &
    PIDS+=($!)
else
    printf 'SKIP 0.0 panic-free check (script not found)\n' > "$CHECK_TMPDIR/03-panics.result"
fi

# ── 4. Markdown snippet compilation ──────────────────────────────────
if [ -f "${REPO_ROOT}/scripts/extract-rust-snippets.sh" ]; then
    run_check "rust snippets" "04-snippets" \
        bash "${REPO_ROOT}/scripts/extract-rust-snippets.sh" &
    PIDS+=($!)
else
    printf 'SKIP 0.0 rust snippets (script not found)\n' > "$CHECK_TMPDIR/04-snippets.result"
fi

# ── 5. Docs rendering check (optional) ──────────────────────────────
if [ -f "${REPO_ROOT}/scripts/pre-commit-docs.sh" ]; then
    run_check "docs rendering" "05-docs" \
        bash "${REPO_ROOT}/scripts/pre-commit-docs.sh" &
    PIDS+=($!)
else
    printf 'SKIP 0.0 docs rendering (script not found)\n' > "$CHECK_TMPDIR/05-docs.result"
fi

# ── Phase 2: Cargo commands (sequential, foreground) ─────────────
# Different feature flags share the same target/ directory; running
# them in parallel causes cache thrashing with no real speedup.

# ── 1. Cargo clippy (no-default-features) ────────────────────────────
run_check "clippy no-default" "01-clippy-nodef" \
    cargo clippy --all-targets --no-default-features -- -D warnings

# ── 2. Cargo test ────────────────────────────────────────────────────
run_check "cargo test" "02-test" \
    cargo test --all-features

# ── 6. Unused dependency check — cargo-machete (optional) ────────────
if command -v cargo-machete &>/dev/null; then
    run_check "cargo-machete" "06-machete" \
        cargo machete
else
    printf 'SKIP 0.0 cargo-machete (not installed)\n' > "$CHECK_TMPDIR/06-machete.result"
fi

# ── Wait for background non-cargo checks ─────────────────────────
if [ ${#PIDS[@]} -gt 0 ]; then
    for pid in "${PIDS[@]}"; do
        wait "$pid" 2>/dev/null || true
    done
fi

OVERALL_END=$(_now)
OVERALL_ELAPSED=$(_elapsed "$OVERALL_START" "$OVERALL_END")

# ── Collect and display results ──────────────────────────────────────
TOTAL=0
FAILED=0
SKIPPED=0

for result_file in "$CHECK_TMPDIR"/*.result; do
    [ -f "$result_file" ] || continue
    status=$(cut -d' ' -f1 < "$result_file")
    elapsed=$(cut -d' ' -f2 < "$result_file")
    name=$(cut -d' ' -f3- < "$result_file")

    if [ "$status" = "SKIP" ]; then
        SKIPPED=$((SKIPPED + 1))
        continue
    fi

    TOTAL=$((TOTAL + 1))

    if [ "$status" = "PASS" ]; then
        printf '  \xe2\x9c\x93 %-24s (%ss)\n' "$name" "$elapsed"
    else
        printf '  \xe2\x9c\x97 %-24s (%ss)\n' "$name" "$elapsed"
        FAILED=$((FAILED + 1))
        id=$(basename "$result_file" .result)
        if [ -s "$CHECK_TMPDIR/$id.stderr" ]; then
            head -20 "$CHECK_TMPDIR/$id.stderr" | sed 's/^/    /'
        elif [ -s "$CHECK_TMPDIR/$id.stdout" ]; then
            head -20 "$CHECK_TMPDIR/$id.stdout" | sed 's/^/    /'
        fi
    fi
done

echo ""
if [ "$SKIPPED" -gt 0 ]; then
    printf '(%d checks skipped — optional tools not installed)\n' "$SKIPPED"
fi

if [ "$FAILED" -gt 0 ]; then
    printf 'FAILED: %d of %d checks failed. (%ss total)\n' "$FAILED" "$TOTAL" "$OVERALL_ELAPSED"
    echo "Fix the issues above, then re-push."
    exit 1
else
    printf 'All %d checks passed. (%ss total)\n' "$TOTAL" "$OVERALL_ELAPSED"
fi
PUSH_SCRIPT

chmod +x "${PUSH_HOOK_FILE}"

# If pre-commit framework hooks exist, warn about the change.
if command -v pre-commit &>/dev/null; then
    echo ""
    echo "Note: The pre-commit framework is installed, but these hooks use custom"
    echo "parallel execution for speed. See .pre-commit-config.yaml for reference."
fi

echo "Pre-commit hook installed at: ${HOOK_FILE}"
echo "Pre-push hook installed at:   ${PUSH_HOOK_FILE}"
echo ""
echo "The pre-commit hook runs on every 'git commit' (all checks in parallel):"
echo "  1.  python3 scripts/pre-commit-llm.py  (line-limit + skills index)"
echo "  2.  pytest -q scripts/test_pre_commit_llm.py (optional, skipped if not installed)"
echo "  3.  markdownlint on **/*.md     (optional, skipped if not installed)"
echo "  4.  shellcheck scripts/*.sh     (optional, skipped if not installed)"
echo "  5.  bash scripts/check-ffi-safety.sh (FFI safety check)"
echo "  6.  bash scripts/test_check_ffi_safety.sh (FFI safety script tests)"
echo "  7.  bash scripts/check-target-gated-doc-links.sh (target-gated doc-link check)"
echo "  8.  bash scripts/test_check_target_gated_doc_links.sh (target-gated doc-link script tests)"
echo "  9.  bash scripts/test_shell_portability.sh (shell portability checks)"
echo " 10.  bash scripts/check-test-io-unwrap.sh (Rust test I/O unwrap check)"
echo " 11.  bash scripts/check-workflows.sh"
echo " 12.  cargo fmt --all -- --check (skipped if no Rust files staged)"
echo " 13.  cargo clippy --all-targets --all-features -- -D warnings (skipped if no Rust files staged)"
echo " 14.  typos --config .typos.toml  (spell check — optional, skipped if not installed)"
echo " 15.  TOML config validation      (optional, requires python3)"
echo ""
echo "The pre-push hook runs on every 'git push' (two-phase execution):"
echo "  Phase 1 — parallel background (non-cargo):"
echo "    3. bash scripts/check-no-panics.sh (panic-free policy — phases 1-2)"
echo "    4. bash scripts/extract-rust-snippets.sh (markdown snippet compilation)"
echo "    5. bash scripts/pre-commit-docs.sh (docs rendering — optional, skipped if mkdocs not installed)"
echo "  Phase 2 — sequential foreground (cargo, shared target/):"
echo "    1. cargo clippy --all-targets --no-default-features -- -D warnings"
echo "    2. cargo test --all-features"
echo "    6. cargo machete (optional — unused dependency check, skipped if not installed)"
echo ""
echo "NOTE: .pre-commit-config.yaml is kept as documentation reference only."
echo "      These hooks always use custom parallel execution for speed."
