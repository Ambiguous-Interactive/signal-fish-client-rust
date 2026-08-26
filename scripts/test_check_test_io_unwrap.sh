#!/usr/bin/env bash
# test_check_test_io_unwrap.sh — Unit tests for scripts/check-test-io-unwrap.sh
#
# Creates a temporary fake repository with known Rust fixtures and verifies
# that check-test-io-unwrap.sh:
#   - detects bare .unwrap() on I/O operations in tracked test sources,
#   - prunes generated/ignored nested Cargo target trees from the scan,
#   - still reports violations in tracked sources even when they live below
#     a directory named "target" (tracked sources are never name-excluded).
#
# Exit codes:
#   0 — all tests passed
#   1 — one or more tests failed

set -euo pipefail

# ── Resolve paths ─────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK_SCRIPT="$SCRIPT_DIR/check-test-io-unwrap.sh"
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

# Set up a fake git repo that mirrors the layout check-test-io-unwrap.sh
# expects: <tmpdir>/scripts/check-test-io-unwrap.sh plus staged fixtures.
#
# Globals set by this function:
#   FAKE_REPO   — path to the fake repo root
#   FAKE_SCRIPT — path to the copied check script inside the fake repo
setup_fake_repo() {
    FAKE_REPO="$(mktemp -d "$TMPDIR_ROOT/repo-XXXXXX")"
    mkdir -p "$FAKE_REPO/scripts"
    cp "$CHECK_SCRIPT" "$FAKE_REPO/scripts/check-test-io-unwrap.sh"
    chmod +x "$FAKE_REPO/scripts/check-test-io-unwrap.sh"
    FAKE_SCRIPT="$FAKE_REPO/scripts/check-test-io-unwrap.sh"
    git init --quiet "$FAKE_REPO"
}

# Stage every fixture written since setup so the checker sees it as tracked.
stage_fixtures() {
    git -C "$FAKE_REPO" add -A
}

# Run check-test-io-unwrap.sh inside the fake repo and capture the exit code.
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

# Assert that RUN_OUTPUT does or does not mention a relative path.
#   $1 — test name
#   $2 — relative path that must appear ("present") or be absent ("absent")
assert_path_reported() {
    local test_name="$1"
    local rel_path="$2"
    local mode="${3:-present}"

    TESTS_RUN=$((TESTS_RUN + 1))

    case "$mode" in
        present)
            if printf '%s\n' "$RUN_OUTPUT" | grep -qF "$rel_path"; then
                echo "  PASS: $test_name"
                TESTS_PASSED=$((TESTS_PASSED + 1))
            else
                echo "  FAIL: $test_name ('$rel_path' missing from checker output)"
                echo "  --- output ---"
                echo "$RUN_OUTPUT"
                echo "  --- end output ---"
                TESTS_FAILED=$((TESTS_FAILED + 1))
            fi
            ;;
        absent)
            if printf '%s\n' "$RUN_OUTPUT" | grep -qF "$rel_path"; then
                echo "  FAIL: $test_name ('$rel_path' unexpectedly in checker output)"
                echo "  --- output ---"
                echo "$RUN_OUTPUT"
                echo "  --- end output ---"
                TESTS_FAILED=$((TESTS_FAILED + 1))
            else
                echo "  PASS: $test_name"
                TESTS_PASSED=$((TESTS_PASSED + 1))
            fi
            ;;
        *)
            # Unreachable: both call sites pass a fixed mode; fail loudly if
            # that ever drifts.
            echo "  FAIL: $test_name (unknown mode '$mode')" >&2
            TESTS_FAILED=$((TESTS_FAILED + 1))
            ;;
    esac
}

echo "=== Tracked-source detection tests ==="

# -- Should FAIL: bare .unwrap() on File::open in a tracked test --
setup_fake_repo
mkdir -p "$FAKE_REPO/tests"
cat > "$FAKE_REPO/tests/with_violation.rs" << 'RUST'
#[test]
fn reads_fixture() {
    let data = std::fs::read_to_string("fixture.json").unwrap();
    assert!(!data.is_empty());
}
RUST
cat > "$FAKE_REPO/tests/clean.rs" << 'RUST'
#[test]
fn clean_case() {
    let n = 1 + 1;
    assert_eq!(n, 2);
}
RUST
stage_fixtures
run_check
assert_exit "Tracked single-line I/O unwrap should FAIL" 1
assert_path_reported "Violation path reported" "tests/with_violation.rs" present
assert_path_reported "Clean file untouched by report" "tests/clean.rs" absent

echo ""
echo "=== Multiline detection tests ==="

# -- Should FAIL: multiline .unwrap() continuation after read_dir --
setup_fake_repo
mkdir -p "$FAKE_REPO/tests"
cat > "$FAKE_REPO/tests/multiline_violation.rs" << 'RUST'
use std::fs;

#[test]
fn lists_dir() {
    let entries = fs::read_dir("some/dir")
        .unwrap()
        .count();
    assert!(entries >= 0);
}
RUST
stage_fixtures
run_check
assert_exit "Multiline I/O unwrap should FAIL" 1

echo ""
echo "=== Generated target-tree pruning tests ==="

# -- Should PASS: violations inside an ignored nested Cargo target tree --
# Mirrors the issue scenario: tests/godot-web-smoke/target/** generated .rs
# files with violations must never be scanned because the tree is ignored.
setup_fake_repo
mkdir -p "$FAKE_REPO/tests"
cat > "$FAKE_REPO/tests/clean.rs" << 'RUST'
#[test]
fn clean_case() {
    let n = 1 + 1;
    assert_eq!(n, 2);
}
RUST
mkdir -p "$FAKE_REPO/tests/godot-web-smoke/target/debug/build/out/deeper"
printf '# fixture to stand in for thousands of generated sources\n' \
    > "$FAKE_REPO/tests/godot-web-smoke/generated-header.txt"
for dir in debug/build/out/deeper release/deps docs; do
    mkdir -p "$FAKE_REPO/tests/godot-web-smoke/target/$dir"
    cat > "$FAKE_REPO/tests/godot-web-smoke/target/$dir/generated.rs" << 'RUST'
fn generated() {
    let contents = std::fs::File::open("x").unwrap();
}
RUST
done
mkdir -p "$FAKE_REPO/tests/godot-web-smoke"
cat > "$FAKE_REPO/tests/godot-web-smoke/.gitignore" << 'EOF'
/target/
EOF
git -C "$FAKE_REPO" add -A
run_check
assert_exit "Ignored target-tree violations should PASS (pruned)" 0
assert_path_reported "Target-tree file absent from scan" \
    "tests/godot-web-smoke/target" absent
# .gitignore itself is tracked but is not a Rust file; verify it was ignored
# by the scanner and did not crash the run.
assert_path_reported "Non-Rust tracked file ignored" ".gitignore" absent

echo ""
echo "=== Tracked source under a 'target'-named directory ==="

# -- Should FAIL: tracked fixture source below a directory named target/ --
# Name-based pruning would silently skip this violation; only ignoring
# untracked/generated trees is correct.
setup_fake_repo
mkdir -p "$FAKE_REPO/tests/http-fixtures/target/snapshots"
cat > "$FAKE_REPO/tests/http-fixtures/target/snapshots/literal_target_source.rs" << 'RUST'
#[test]
fn writes_snapshot() {
    use std::fs::File;
    let f = File::create("snap.bin").unwrap();
    drop(f);
}
RUST
stage_fixtures
run_check
assert_exit "Tracked source under directory named target/ should FAIL" 1
assert_path_reported "Literal-target tracked source detected" \
    "tests/http-fixtures/target/snapshots/literal_target_source.rs" present

echo ""
echo "=== Degenerate layout tests ==="

# -- Should PASS: ignored-only tree with no tracked Rust files --
setup_fake_repo
mkdir -p "$FAKE_REPO/tests/nobody/target/garbage"
cat > "$FAKE_REPO/tests/nobody/target/garbage/gone.rs" << 'RUST'
fn gone() {
    std::fs::read_to_string("x").unwrap();
}
RUST
cat > "$FAKE_REPO/.gitignore" << 'EOF'
tests/**/target/
EOF
stage_fixtures
run_check
assert_exit "Repo with no tracked Rust files should PASS" 0
assert_path_reported "Untracked garbage never scanned" "gone.rs" absent

echo ""
echo "=== Environment-error tests ==="

# -- Should FAIL(2): invocation outside a git repository must not pass green --
# A swallowed git failure would turn every environment error into a clean scan.
NONREPO="$(mktemp -d "$TMPDIR_ROOT/nonrepo-XXXXXX")"
mkdir -p "$NONREPO/scripts" "$NONREPO/tests"
cp "$CHECK_SCRIPT" "$NONREPO/scripts/check-test-io-unwrap.sh"
cat > "$NONREPO/tests/violation_outside_repo.rs" << 'RUST'
fn violation() {
    std::fs::File::open("x").unwrap();
}
RUST
TESTS_RUN=$((TESTS_RUN + 1))
NONREPO_EXIT=0
# Run from outside any repository; capture the checker's own exit code.
NONREPO_OUTPUT="$(
    cd / && bash "$NONREPO/scripts/check-test-io-unwrap.sh" 2>&1
)" || NONREPO_EXIT=$?
if [ "$NONREPO_EXIT" -eq 2 ] && ! printf '%s\n' "$NONREPO_OUTPUT" |
    grep -q "VIOLATION"; then
    echo "  PASS: Outside a git repository exits 2 without scanning"
    TESTS_PASSED=$((TESTS_PASSED + 1))
else
    echo "  FAIL: Outside a git repository (expected exit 2, got $NONREPO_EXIT)"
    echo "  --- output ---"
    echo "$NONREPO_OUTPUT"
    echo "  --- end output ---"
    TESTS_FAILED=$((TESTS_FAILED + 1))
fi

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
