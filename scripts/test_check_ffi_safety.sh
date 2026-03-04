#!/usr/bin/env bash
# test_check_ffi_safety.sh — Unit tests for scripts/check-ffi-safety.sh
#
# Creates temporary Rust source files with known patterns and verifies that
# check-ffi-safety.sh correctly detects (or ignores) them.
#
# Exit codes:
#   0 — all tests passed
#   1 — one or more tests failed

set -euo pipefail

# ── Resolve paths ─────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK_SCRIPT="$SCRIPT_DIR/check-ffi-safety.sh"
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

# Set up a fake repo that mirrors the layout check-ffi-safety.sh expects:
#   <tmpdir>/scripts/check-ffi-safety.sh   (copy of real script)
#   <tmpdir>/src/<file>.rs                 (test fixture)
#
# Globals set by this function:
#   FAKE_REPO   — path to the fake repo root
#   FAKE_SCRIPT — path to the copied check script inside the fake repo
setup_fake_repo() {
    FAKE_REPO="$(mktemp -d "$TMPDIR_ROOT/repo-XXXXXX")"
    mkdir -p "$FAKE_REPO/scripts" "$FAKE_REPO/src"
    cp "$CHECK_SCRIPT" "$FAKE_REPO/scripts/check-ffi-safety.sh"
    chmod +x "$FAKE_REPO/scripts/check-ffi-safety.sh"
    FAKE_SCRIPT="$FAKE_REPO/scripts/check-ffi-safety.sh"
}

# Run check-ffi-safety.sh inside the fake repo and capture the exit code.
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

echo "=== Bool-in-repr-C tests ==="

# -- Should FAIL: #[repr(C)] struct with a bool field --
setup_fake_repo
cat > "$FAKE_REPO/src/bad_bool.rs" << 'RUST'
#[repr(C)]
pub struct MyStruct {
    pub active: bool,
    pub count: u32,
}
RUST
run_check
assert_exit "repr(C) struct with bool field should FAIL" 1

# -- Should PASS: #[repr(C)] struct with no bool fields --
setup_fake_repo
cat > "$FAKE_REPO/src/good_struct.rs" << 'RUST'
use std::os::raw::c_int;

#[repr(C)]
pub struct MyStruct {
    pub active: c_int,
    pub count: u32,
}
RUST
run_check
assert_exit "repr(C) struct without bool should PASS" 0

# -- Should PASS: Regular (non-repr-C) struct with bool fields --
setup_fake_repo
cat > "$FAKE_REPO/src/regular_struct.rs" << 'RUST'
pub struct MyStruct {
    pub active: bool,
    pub count: u32,
}
RUST
run_check
assert_exit "Non-repr(C) struct with bool should PASS" 0

# -- Should PASS: bool mentioned in a comment inside a repr(C) struct --
setup_fake_repo
cat > "$FAKE_REPO/src/commented_bool.rs" << 'RUST'
use std::os::raw::c_int;

#[repr(C)]
pub struct MyStruct {
    // This was previously a bool, changed to c_int
    pub active: c_int,
    pub count: u32,
}
RUST
run_check
assert_exit "bool in comment inside repr(C) struct should PASS" 0

echo ""
echo "=== Unchecked callback tests ==="

# -- Should FAIL: Bare emscripten_websocket_set_onopen_callback_on_thread call --
setup_fake_repo
cat > "$FAKE_REPO/src/unchecked_callback.rs" << 'RUST'
#[cfg(not(target_os = "emscripten"))]
compile_error!("This module requires the emscripten target.");

fn setup_callbacks(socket: EMSCRIPTEN_WEBSOCKET_T) {
    unsafe {
        emscripten_websocket_set_onopen_callback_on_thread(
            socket,
            std::ptr::null_mut(),
            Some(on_open),
            0,
        );
    }
}
RUST
run_check
assert_exit "Bare emscripten callback call should FAIL" 1

# -- Should PASS: let result = emscripten_websocket_set_onopen_callback_on_thread --
setup_fake_repo
cat > "$FAKE_REPO/src/checked_callback.rs" << 'RUST'
#[cfg(not(target_os = "emscripten"))]
compile_error!("This module requires the emscripten target.");

fn setup_callbacks(socket: EMSCRIPTEN_WEBSOCKET_T) {
    unsafe {
        let result = emscripten_websocket_set_onopen_callback_on_thread(
            socket,
            std::ptr::null_mut(),
            Some(on_open),
            0,
        );
        assert_eq!(result, EMSCRIPTEN_RESULT_SUCCESS);
    }
}
RUST
run_check
assert_exit "Checked (let result =) emscripten callback should PASS" 0

# -- Should PASS: Tuple pattern — call inside a tuple within an array --
setup_fake_repo
cat > "$FAKE_REPO/src/tuple_callback.rs" << 'RUST'
#[cfg(not(target_os = "emscripten"))]
compile_error!("This module requires the emscripten target.");

fn setup_callbacks(socket: EMSCRIPTEN_WEBSOCKET_T, user_data: *mut c_void) {
    unsafe {
        let registrations = [
            (
                "onopen",
                emscripten_websocket_set_onopen_callback_on_thread(
                    socket,
                    user_data,
                    Some(on_open),
                    0,
                ),
            ),
        ];
        for (name, result) in &registrations {
            assert_eq!(*result, EMSCRIPTEN_RESULT_SUCCESS, "{name} failed");
        }
    }
}
RUST
run_check
assert_exit "Tuple pattern (call inside array of tuples) should PASS" 0

# -- Should PASS: Array-of-tuples pattern — multiple registrations like production code --
setup_fake_repo
cat > "$FAKE_REPO/src/array_tuples_callback.rs" << 'RUST'
#[cfg(not(target_os = "emscripten"))]
compile_error!("This module requires the emscripten target.");

fn register_all_callbacks(socket: EMSCRIPTEN_WEBSOCKET_T, user_data: *mut c_void) {
    unsafe {
        let results = [
            (
                "onopen",
                emscripten_websocket_set_onopen_callback_on_thread(
                    socket,
                    user_data,
                    Some(on_open_callback),
                    0,
                ),
            ),
            (
                "onmessage",
                emscripten_websocket_set_onmessage_callback_on_thread(
                    socket,
                    user_data,
                    Some(on_message_callback),
                    0,
                ),
            ),
            (
                "onerror",
                emscripten_websocket_set_onerror_callback_on_thread(
                    socket,
                    user_data,
                    Some(on_error_callback),
                    0,
                ),
            ),
        ];
        for (name, result) in &results {
            if *result != EMSCRIPTEN_RESULT_SUCCESS {
                panic!("{name} callback registration failed: {result}");
            }
        }
    }
}
RUST
run_check
assert_exit "Array-of-tuples pattern (multiple registrations) should PASS" 0

# -- Should FAIL: Bare call on its own line even with tuple on a LATER line --
setup_fake_repo
cat > "$FAKE_REPO/src/bare_then_tuple.rs" << 'RUST'
#[cfg(not(target_os = "emscripten"))]
compile_error!("This module requires the emscripten target.");

fn setup_callbacks(socket: EMSCRIPTEN_WEBSOCKET_T, user_data: *mut c_void) {
    unsafe {
        emscripten_websocket_set_onopen_callback_on_thread(
            socket,
            user_data,
            Some(on_open),
            0,
        );
        let _unrelated = ("some_tuple", 42);
    }
}
RUST
run_check
assert_exit "Bare call with tuple on later line should FAIL" 1

# -- Should PASS: Call preceded by line ending with = (split across lines) --
setup_fake_repo
cat > "$FAKE_REPO/src/split_line_assign.rs" << 'RUST'
#[cfg(not(target_os = "emscripten"))]
compile_error!("This module requires the emscripten target.");

fn setup_callbacks(socket: EMSCRIPTEN_WEBSOCKET_T, user_data: *mut c_void) {
    unsafe {
        let result =
            emscripten_websocket_set_onopen_callback_on_thread(
                socket,
                user_data,
                Some(on_open),
                0,
            );
        assert_eq!(result, EMSCRIPTEN_RESULT_SUCCESS);
    }
}
RUST
run_check
assert_exit "Split-line assignment (= on previous line) should PASS" 0

echo ""
echo "=== Edge-case tests ==="

# -- Should PASS: Empty Rust file --
setup_fake_repo
touch "$FAKE_REPO/src/empty.rs"
run_check
assert_exit "Empty Rust file should PASS" 0

# -- Should PASS: File with repr(C) but no struct following it --
setup_fake_repo
cat > "$FAKE_REPO/src/repr_no_struct.rs" << 'RUST'
// This file mentions #[repr(C)] in a comment but has no struct.
fn some_function() {
    let x = 42;
}
RUST
run_check
assert_exit "repr(C) in comment with no struct should PASS" 0

# -- Should PASS: #[repr(C)] followed by an enum, not a struct --
setup_fake_repo
cat > "$FAKE_REPO/src/repr_enum.rs" << 'RUST'
#[repr(C)]
pub enum MyEnum {
    A,
    B,
    C,
}
RUST
run_check
assert_exit "repr(C) enum (not struct) should PASS" 0

echo ""
echo "=== Callback SAFETY comment tests ==="

# -- Should FAIL: extern "C" fn with SAFETY block but missing per-function comment --
setup_fake_repo
cat > "$FAKE_REPO/src/missing_per_fn_safety.rs" << 'RUST'
// SAFETY (all callbacks): These extern "C" functions are registered
// with a C API that guarantees pointer validity.

extern "C" fn on_open_callback(
    _event_type: i32,
    _event: *const u8,
    user_data: *mut u8,
) -> i32 {
    1
}
RUST
run_check
assert_exit "extern C fn with SAFETY block but missing per-function comment should FAIL" 1

# -- Should PASS: extern "C" fn with SAFETY block AND per-function comment --
setup_fake_repo
cat > "$FAKE_REPO/src/with_per_fn_safety.rs" << 'RUST'
// SAFETY (all callbacks): These extern "C" functions are registered
// with a C API that guarantees pointer validity.

// SAFETY: See the callback SAFETY block comment above for pointer guarantees.
extern "C" fn on_open_callback(
    _event_type: i32,
    _event: *const u8,
    user_data: *mut u8,
) -> i32 {
    1
}
RUST
run_check
assert_exit "extern C fn with SAFETY block AND per-function comment should PASS" 0

# -- Should PASS: extern "C" fn WITHOUT any SAFETY block in the file (Check 4 skips) --
setup_fake_repo
cat > "$FAKE_REPO/src/no_safety_block.rs" << 'RUST'
extern "C" fn standalone_callback(
    _event_type: i32,
    user_data: *mut u8,
) -> i32 {
    1
}
RUST
run_check
assert_exit "extern C fn without any SAFETY block in file should PASS (Check 4 skips)" 0

# -- Should PASS: Blank line between SAFETY comment and extern "C" fn --
# Check 4 walks backwards over blank lines, so this should still pass.
setup_fake_repo
cat > "$FAKE_REPO/src/blank_line_before_fn.rs" << 'RUST'
// SAFETY (all callbacks): These extern "C" functions are registered
// with a C API that guarantees pointer validity.

// SAFETY: See the callback SAFETY block comment above for pointer guarantees.

extern "C" fn on_open_callback(
    _event_type: i32,
    _event: *const u8,
    user_data: *mut u8,
) -> i32 {
    1
}
RUST
run_check
assert_exit "Blank line between SAFETY comment and extern C fn should PASS" 0

echo ""
echo "=== close()-must-also-delete tests ==="

# -- Should FAIL: close() calls emscripten_websocket_close but NOT emscripten_websocket_delete --
setup_fake_repo
cat > "$FAKE_REPO/src/close_without_delete.rs" << 'RUST'
#[cfg(not(target_os = "emscripten"))]
compile_error!("This module requires the emscripten target.");

struct Transport {
    socket: i32,
    closed: bool,
}

impl Transport {
    async fn close(&mut self) -> Result<(), ()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        unsafe {
            emscripten_websocket_close(self.socket, 1000, std::ptr::null());
        }
        Ok(())
    }
}
RUST
run_check
assert_exit "close() with emscripten_websocket_close but no delete should FAIL" 1

# -- Should PASS: close() calls both emscripten_websocket_close AND emscripten_websocket_delete --
setup_fake_repo
cat > "$FAKE_REPO/src/close_with_delete.rs" << 'RUST'
#[cfg(not(target_os = "emscripten"))]
compile_error!("This module requires the emscripten target.");

struct Transport {
    socket: i32,
    closed: bool,
    deleted: bool,
}

impl Transport {
    async fn close(&mut self) -> Result<(), ()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        unsafe {
            emscripten_websocket_close(self.socket, 1000, std::ptr::null());
            emscripten_websocket_delete(self.socket);
        }
        self.deleted = true;
        Ok(())
    }
}
RUST
run_check
assert_exit "close() with both emscripten_websocket_close and delete should PASS" 0

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
