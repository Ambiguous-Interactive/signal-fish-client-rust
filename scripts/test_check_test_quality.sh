#!/usr/bin/env bash
# test_check_test_quality.sh — Unit tests for scripts/check-test-quality.sh
#
# Creates temporary Rust source files with known patterns and verifies that
# check-test-quality.sh correctly detects (or ignores) them.
#
# Exit codes:
#   0 — all tests passed
#   1 — one or more tests failed

set -euo pipefail

# ── Resolve paths ─────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK_SCRIPT="$SCRIPT_DIR/check-test-quality.sh"
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

# Set up a fake repo that mirrors the layout check-test-quality.sh expects:
#   <tmpdir>/scripts/check-test-quality.sh   (copy of real script)
#   <tmpdir>/src/<file>.rs                   (test fixture)
#   <tmpdir>/tests/<file>.rs                 (test fixture)
#
# Globals set by this function:
#   FAKE_REPO   — path to the fake repo root
#   FAKE_SCRIPT — path to the copied check script inside the fake repo
setup_fake_repo() {
    FAKE_REPO="$(mktemp -d "$TMPDIR_ROOT/repo-XXXXXX")"
    mkdir -p "$FAKE_REPO/scripts" "$FAKE_REPO/src" "$FAKE_REPO/tests"
    cp "$CHECK_SCRIPT" "$FAKE_REPO/scripts/check-test-quality.sh"
    chmod +x "$FAKE_REPO/scripts/check-test-quality.sh"
    FAKE_SCRIPT="$FAKE_REPO/scripts/check-test-quality.sh"
}

# Run check-test-quality.sh inside the fake repo and capture the exit code.
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

echo "=== Mutable reference to temporary tests ==="

# -- Should FAIL: &mut false in src/ --
setup_fake_repo
cat > "$FAKE_REPO/src/bad_mut_ref.rs" << 'RUST'
fn example() {
    some_fn(&mut false);
}
RUST
run_check
assert_exit "&mut false in src/ should FAIL" 1

# -- Should FAIL: &mut true in tests/ --
setup_fake_repo
cat > "$FAKE_REPO/tests/bad_mut_ref.rs" << 'RUST'
fn example() {
    some_fn(&mut true);
}
RUST
run_check
assert_exit "&mut true in tests/ should FAIL" 1

# -- Should FAIL: &mut 0 --
setup_fake_repo
cat > "$FAKE_REPO/src/bad_mut_zero.rs" << 'RUST'
fn example() {
    some_fn(&mut 0);
}
RUST
run_check
assert_exit "&mut 0 should FAIL" 1

# -- Should FAIL: &mut 1 --
setup_fake_repo
cat > "$FAKE_REPO/src/bad_mut_one.rs" << 'RUST'
fn example() {
    some_fn(&mut 1);
}
RUST
run_check
assert_exit "&mut 1 should FAIL" 1

# -- Should PASS: &mut variable (not a literal) --
setup_fake_repo
cat > "$FAKE_REPO/src/good_mut_ref.rs" << 'RUST'
fn example() {
    let mut val = false;
    some_fn(&mut val);
}
RUST
run_check
assert_exit "&mut variable (not literal) should PASS" 0

# -- Should PASS: &mut true_count (identifier starting with true) --
setup_fake_repo
cat > "$FAKE_REPO/src/good_true_count.rs" << 'RUST'
fn example() {
    let mut true_count = 0;
    some_fn(&mut true_count);
}
RUST
run_check
assert_exit "&mut true_count (identifier) should PASS" 0

# -- Should PASS: &mut 100 (multi-digit integer, not 0 or 1) --
setup_fake_repo
cat > "$FAKE_REPO/src/good_big_number.rs" << 'RUST'
fn example() {
    some_fn(&mut 100);
}
RUST
run_check
assert_exit "&mut 100 (not 0 or 1) should PASS" 0

# -- Should PASS: &mut false inside a comment --
setup_fake_repo
cat > "$FAKE_REPO/src/commented.rs" << 'RUST'
fn example() {
    // This used to be some_fn(&mut false) but was fixed
    let mut val = false;
    some_fn(&mut val);
}
RUST
run_check
assert_exit "&mut false inside a comment should PASS" 0

# -- Should PASS: &mut false inside a doc comment --
setup_fake_repo
cat > "$FAKE_REPO/src/doc_commented.rs" << 'RUST'
/// Example: some_fn(&mut false)
fn example() {
    let mut val = false;
    some_fn(&mut val);
}
RUST
run_check
assert_exit "&mut false inside a doc comment should PASS" 0

echo ""
echo "=== String literal heuristic tests ==="

# -- Should PASS: &mut false inside a string literal --
setup_fake_repo
cat > "$FAKE_REPO/src/in_string.rs" << 'RUST'
fn example() {
    let msg = "do not pass &mut false here";
}
RUST
run_check
assert_exit "&mut false inside a string literal should PASS" 0

# -- Should PASS: &mut true inside a string with escaped quotes before it --
# This tests the escaped-quote stripping: the \" before &mut should not
# count as an unescaped quote, so the heuristic should still detect
# that &mut is inside a string (1 real quote before it = odd = inside string).
setup_fake_repo
cat > "$FAKE_REPO/src/escaped_quote_string.rs" << 'RUST'
fn example() {
    let msg = "she said \"&mut true is bad\"";
}
RUST
run_check
assert_exit "&mut true inside string with escaped quotes should PASS" 0

# -- Should FAIL: &mut false after a complete string (even number of quotes before it) --
setup_fake_repo
cat > "$FAKE_REPO/src/after_string.rs" << 'RUST'
fn example() {
    let _msg = "hello"; some_fn(&mut false);
}
RUST
run_check
assert_exit "&mut false after a complete string should FAIL" 1

# -- Should PASS: Empty src/ and tests/ --
setup_fake_repo
run_check
assert_exit "Empty src/ and tests/ should PASS" 0

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
