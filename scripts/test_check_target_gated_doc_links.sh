#!/usr/bin/env bash
# test_check_target_gated_doc_links.sh — Unit tests for scripts/check-target-gated-doc-links.sh
#
# Creates temporary Rust source files with known patterns and verifies that
# check-target-gated-doc-links.sh correctly detects (or ignores) them.
#
# Exit codes:
#   0 — all tests passed
#   1 — one or more tests failed

set -euo pipefail

# ── Resolve paths ─────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK_SCRIPT="$SCRIPT_DIR/check-target-gated-doc-links.sh"
if [ ! -f "$CHECK_SCRIPT" ]; then
    echo "ERROR: $CHECK_SCRIPT not found. Run from the repo root." >&2
    exit 1
fi

# ── Temp directory with cleanup ───────────────────────────────────────
TMPDIR_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_ROOT"' EXIT

# ── Counters ──────────────────────────────────────────────────────────
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

# ── Helpers ───────────────────────────────────────────────────────────

# Set up a fake repo that mirrors the layout check-target-gated-doc-links.sh expects:
#   <tmpdir>/scripts/check-target-gated-doc-links.sh   (copy of real script)
#   <tmpdir>/src/<file>.rs                              (test fixture)
#
# Globals set by this function:
#   FAKE_REPO   — path to the fake repo root
#   FAKE_SCRIPT — path to the copied check script inside the fake repo
setup_fake_repo() {
    FAKE_REPO="$(mktemp -d "$TMPDIR_ROOT/repo-XXXXXX")"
    mkdir -p "$FAKE_REPO/scripts" "$FAKE_REPO/src"
    cp "$CHECK_SCRIPT" "$FAKE_REPO/scripts/check-target-gated-doc-links.sh"
    chmod +x "$FAKE_REPO/scripts/check-target-gated-doc-links.sh"
    FAKE_SCRIPT="$FAKE_REPO/scripts/check-target-gated-doc-links.sh"
}

# Run check-target-gated-doc-links.sh inside the fake repo and capture the exit code.
# Stdout/stderr are captured in RUN_OUTPUT.
# Sets RUN_EXIT to the exit code.
run_check() {
    RUN_OUTPUT=""
    RUN_EXIT=0
    RUN_OUTPUT=$("$FAKE_SCRIPT" 2>&1) || RUN_EXIT=$?
}

# Assert that the check script exited with the expected code.
#   $1 — test name
#   $2 — expected exit code (0 = pass, 1 = fail)
assert_exit() {
    local test_name="$1"
    local expected="$2"

    TESTS_RUN=$((TESTS_RUN + 1))

    if [ "$RUN_EXIT" -eq "$expected" ]; then
        echo "  PASS: $test_name"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        echo "  FAIL: $test_name (expected exit $expected, got $RUN_EXIT)"
        echo "  --- output ---"
        echo "$RUN_OUTPUT"
        echo "  --- end output ---"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

# ── Test cases ────────────────────────────────────────────────────────

echo "=== Violation detection tests ==="

# -- Should FAIL: Intra-doc link to EmscriptenWebSocketTransport --
setup_fake_repo
cat > "$FAKE_REPO/src/lib.rs" << 'RUST'
//! See [`EmscriptenWebSocketTransport`] for the Emscripten transport.
pub mod transports;
RUST
run_check
assert_exit "Intra-doc link to EmscriptenWebSocketTransport should FAIL" 1

# -- Should FAIL: Intra-doc link to EmscriptenWebSocketTransport method --
setup_fake_repo
cat > "$FAKE_REPO/src/lib.rs" << 'RUST'
//! Call [`EmscriptenWebSocketTransport::connect`] to establish a connection.
pub mod transports;
RUST
run_check
assert_exit "Intra-doc link to EmscriptenWebSocketTransport method should FAIL" 1

# -- Should PASS: Plain backtick reference (not an intra-doc link) --
setup_fake_repo
cat > "$FAKE_REPO/src/lib.rs" << 'RUST'
//! See `EmscriptenWebSocketTransport` for the Emscripten transport.
pub mod transports;
RUST
run_check
assert_exit "Plain backtick reference should PASS" 0

# -- Should PASS: Empty file --
setup_fake_repo
touch "$FAKE_REPO/src/empty.rs"
run_check
assert_exit "Empty file should PASS" 0

# -- Should PASS: No Rust files at all --
setup_fake_repo
run_check
assert_exit "No Rust files should PASS" 0

echo ""
echo "=== Exclusion tests (flat file layout) ==="

# -- Should PASS: Intra-doc link inside emscripten_websocket.rs (flat file) --
setup_fake_repo
cat > "$FAKE_REPO/src/emscripten_websocket.rs" << 'RUST'
//! This module provides [`EmscriptenWebSocketTransport`].
pub struct EmscriptenWebSocketTransport;
RUST
run_check
assert_exit "Intra-doc link inside emscripten_websocket.rs (flat) should PASS (excluded)" 0

# -- Should FAIL: Intra-doc link inside emscripten_websocket_ffi.rs (NOT excluded — different module) --
setup_fake_repo
cat > "$FAKE_REPO/src/emscripten_websocket_ffi.rs" << 'RUST'
//! FFI bindings for [`EmscriptenWebSocketTransport`].
RUST
run_check
assert_exit "Intra-doc link inside emscripten_websocket_ffi.rs should FAIL (different module)" 1

echo ""
echo "=== Exclusion tests (directory layout) ==="

# -- Should PASS: Intra-doc link inside emscripten_websocket/ directory --
setup_fake_repo
mkdir -p "$FAKE_REPO/src/emscripten_websocket"
cat > "$FAKE_REPO/src/emscripten_websocket/mod.rs" << 'RUST'
//! This module provides [`EmscriptenWebSocketTransport`].
pub struct EmscriptenWebSocketTransport;
RUST
run_check
assert_exit "Intra-doc link inside emscripten_websocket/mod.rs should PASS (excluded)" 0

# -- Should PASS: Intra-doc link inside emscripten_websocket/ subdirectory --
setup_fake_repo
mkdir -p "$FAKE_REPO/src/emscripten_websocket/connection"
cat > "$FAKE_REPO/src/emscripten_websocket/connection/transport.rs" << 'RUST'
//! Internal transport using [`EmscriptenWebSocketTransport`].
RUST
run_check
assert_exit "Intra-doc link inside emscripten_websocket/connection/transport.rs should PASS (excluded)" 0

# -- Should FAIL: Intra-doc link inside emscripten_websocket_v2/ (NOT excluded — different module) --
setup_fake_repo
mkdir -p "$FAKE_REPO/src/emscripten_websocket_v2"
cat > "$FAKE_REPO/src/emscripten_websocket_v2/mod.rs" << 'RUST'
//! V2 transport using [`EmscriptenWebSocketTransport`].
RUST
run_check
assert_exit "Intra-doc link inside emscripten_websocket_v2/mod.rs should FAIL (different module)" 1

echo ""
echo "=== Mixed tests (violations + exclusions) ==="

# -- Should FAIL: One excluded file + one violating file --
setup_fake_repo
cat > "$FAKE_REPO/src/emscripten_websocket.rs" << 'RUST'
//! This module provides [`EmscriptenWebSocketTransport`].
pub struct EmscriptenWebSocketTransport;
RUST
cat > "$FAKE_REPO/src/lib.rs" << 'RUST'
//! See [`EmscriptenWebSocketTransport`] for the Emscripten transport.
pub mod transports;
RUST
run_check
assert_exit "Excluded file + violating file should FAIL" 1

# -- Should PASS: Multiple clean files --
setup_fake_repo
cat > "$FAKE_REPO/src/lib.rs" << 'RUST'
//! See `EmscriptenWebSocketTransport` for the Emscripten transport.
pub mod transports;
RUST
cat > "$FAKE_REPO/src/client.rs" << 'RUST'
//! The client does not reference emscripten types with doc links.
pub struct Client;
RUST
run_check
assert_exit "Multiple clean files should PASS" 0

# -- Should FAIL: Violation in a non-emscripten subdirectory --
setup_fake_repo
mkdir -p "$FAKE_REPO/src/transports"
cat > "$FAKE_REPO/src/transports/mod.rs" << 'RUST'
//! Available transports: [`EmscriptenWebSocketTransport`].
RUST
run_check
assert_exit "Violation in transports/mod.rs should FAIL" 1

echo ""
echo "=== Results ==="
echo "Tests run:    $TESTS_RUN"
echo "Tests passed: $TESTS_PASSED"
echo "Tests failed: $TESTS_FAILED"

if [ "$TESTS_FAILED" -gt 0 ]; then
    echo "FAILED: $TESTS_FAILED test(s) did not produce the expected result."
    exit 1
else
    echo "ALL TESTS PASSED."
    exit 0
fi
