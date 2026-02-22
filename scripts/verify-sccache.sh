#!/usr/bin/env bash
# Verify that sccache can actually perform a compilation.
# This catches GitHub Actions Cache service outages that cause sccache to fail
# during server startup but not during version/stats checks.
#
# The script performs multiple verification stages:
# 1. Test rustc version query (catches "rustc -vV" timeout issues)
# 2. Test actual compilation
# 3. Retry both tests to catch intermittent failures
#
# Usage: ./scripts/verify-sccache.sh
# Exit code: 0 if sccache is working, 1 if it's not
# Output: Sets GITHUB_OUTPUT variable "working" to "true" or "false" if running in CI

set -euo pipefail

# Configuration
TEMP_DIR="${TMPDIR:-/tmp}"
TEST_FILE="$TEMP_DIR/sccache_test_$$.rs"
TEST_OUTPUT="$TEMP_DIR/sccache_test_$$"
ERROR_LOG="$TEMP_DIR/sccache_test_$$.log"
VERSION_LOG="$TEMP_DIR/sccache_version_$$.log"
MAX_RETRIES=3
RETRY_DELAY_SECONDS=2
SCCACHE_TIMEOUT=30

run_with_timeout() {
    local timeout_seconds="$1"
    shift

    if command -v timeout &>/dev/null; then
        timeout "$timeout_seconds" "$@"
    else
        "$@"
    fi
}

# shellcheck disable=SC2317  # Called indirectly via trap
cleanup() {
    rm -f "$TEST_FILE" "$TEST_OUTPUT" "$ERROR_LOG" "$VERSION_LOG"
}
trap cleanup EXIT

output_failure() {
    local reason="$1"
    echo "::warning::sccache verification failed ($reason) - falling back to direct compilation"
    if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
        echo "working=false" >> "$GITHUB_OUTPUT"
    fi
    exit 1
}

output_success() {
    echo "sccache verification: PASSED"
    if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
        echo "working=true" >> "$GITHUB_OUTPUT"
    fi
    exit 0
}

verify_version_query() {
    local attempt=$1
    echo "Attempt $attempt: Testing sccache version query (rustc -vV)..."

    if run_with_timeout "$SCCACHE_TIMEOUT" env RUSTC_WRAPPER=sccache rustc -vV 2>"$VERSION_LOG"; then
        echo "  Version query: OK"
        return 0
    else
        local exit_code=$?
        echo "  Version query: FAILED (exit code: $exit_code)"
        if [[ -s "$VERSION_LOG" ]]; then
            echo "  Error output:"
            head -5 "$VERSION_LOG" | sed 's/^/    /'
        fi
        return 1
    fi
}

verify_compilation() {
    local attempt=$1
    echo "Attempt $attempt: Testing sccache compilation..."

    echo 'fn main() {}' > "$TEST_FILE"

    if run_with_timeout "$SCCACHE_TIMEOUT" env RUSTC_WRAPPER=sccache rustc "$TEST_FILE" -o "$TEST_OUTPUT" 2>"$ERROR_LOG"; then
        echo "  Compilation: OK"
        return 0
    else
        local exit_code=$?
        echo "  Compilation: FAILED (exit code: $exit_code)"
        if [[ -s "$ERROR_LOG" ]]; then
            echo "  Error output:"
            head -10 "$ERROR_LOG" | sed 's/^/    /'
        fi
        return 1
    fi
}

main() {
    echo "=== sccache verification ==="
    echo "Max retries: $MAX_RETRIES"
    echo "Timeout per operation: ${SCCACHE_TIMEOUT}s"
    echo ""

    for ((attempt = 1; attempt <= MAX_RETRIES; attempt++)); do
        if [[ $attempt -gt 1 ]]; then
            echo ""
            echo "Retrying after ${RETRY_DELAY_SECONDS}s delay..."
            sleep "$RETRY_DELAY_SECONDS"
            sccache --stop-server 2>/dev/null || true
        fi

        if ! verify_version_query "$attempt"; then
            continue
        fi

        if ! verify_compilation "$attempt"; then
            continue
        fi

        echo ""
        output_success
    done

    echo ""
    echo "All $MAX_RETRIES attempts failed."
    output_failure "verification failed after $MAX_RETRIES attempts"
}

main
