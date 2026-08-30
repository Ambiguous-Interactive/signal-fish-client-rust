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

# -- Should FAIL: one-line #[repr(C)] struct body with a bool field --
# (the declaration line never reached the per-line body scan before)
setup_fake_repo
cat > "$FAKE_REPO/src/one_line_bool.rs" << 'RUST'
#[repr(C)]
pub struct OneLine { pub flag: bool }
RUST
run_check
assert_exit "one-line repr(C) struct with bool should FAIL" 1

# -- Should PASS: one-line #[repr(C)] struct body without a bool field --
setup_fake_repo
cat > "$FAKE_REPO/src/one_line_clean.rs" << 'RUST'
use std::os::raw::c_int;

#[repr(C)]
pub struct OneLine { pub flag: c_int }
RUST
run_check
assert_exit "one-line repr(C) struct without bool should PASS" 0

# -- Should FAIL: bare bool inside a member crate under crates/ --
# (the scan roots must cover every workspace member, not only the core)
setup_fake_repo
mkdir -p "$FAKE_REPO/src"
printf 'pub fn ok() {}\n' > "$FAKE_REPO/src/lib.rs"
mkdir -p "$FAKE_REPO/crates/some-adapter/src"
cat > "$FAKE_REPO/crates/some-adapter/src/ffi.rs" << 'RUST'
#[repr(C)]
pub struct AdapterStruct {
    pub ready: bool,
}
RUST
run_check
assert_exit "bool struct in a member crate should FAIL" 1

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
mkdir -p "$FAKE_REPO/src/transports"
cat > "$FAKE_REPO/src/transports/emscripten_websocket.rs" << 'RUST'
#[cfg(not(target_os = "emscripten"))]
compile_error!("This module requires the emscripten target.");

impl Transport {
    fn poll_close(&mut self) {
        let close_error = self.close_native_socket().err();
    }

    fn is_ready(&self) -> bool {
        true
    }
}
RUST
run_check
assert_exit "poll_close with native close but no delete should FAIL" 1

# -- Should PASS: close() calls both emscripten_websocket_close AND emscripten_websocket_delete --
setup_fake_repo
mkdir -p "$FAKE_REPO/src/transports"
cat > "$FAKE_REPO/src/transports/emscripten_websocket.rs" << 'RUST'
#[cfg(not(target_os = "emscripten"))]
compile_error!("This module requires the emscripten target.");

impl Transport {
    fn poll_close(&mut self) {
        let close_error = self.close_native_socket().err();
        let delete_result = self.delete_after_close_attempt();
    }

    fn is_ready(&self) -> bool {
        true
    }
}
RUST
run_check
assert_exit "poll_close with ordered top-level close and delete should PASS" 0

# -- Should FAIL: the real Transport contract uses a multiline poll_close --
setup_fake_repo
mkdir -p "$FAKE_REPO/src/transports"
cat > "$FAKE_REPO/src/transports/emscripten_websocket.rs" << 'RUST'
#[cfg(not(target_os = "emscripten"))]
compile_error!("This module requires the emscripten target.");

impl Transport {
    fn poll_close(&mut self) {
        let close_error = self.close_native_socket().err();
        if false {
        let delete_result = self.delete_after_close_attempt();
        }
    }

    fn is_ready(&self) -> bool {
        true
    }
}
RUST
run_check
assert_exit "nested unreachable delete must not satisfy poll_close cleanup" 1

# -- Should PASS: poll_close funnels through both audited helpers --
setup_fake_repo
mkdir -p "$FAKE_REPO/src/transports"
cat > "$FAKE_REPO/src/transports/emscripten_websocket.rs" << 'RUST'
#[cfg(not(target_os = "emscripten"))]
compile_error!("This module requires the emscripten target.");

impl Transport {
    fn poll_close(&mut self) {
        let delete_result = self.delete_after_close_attempt();
        let close_error = self.close_native_socket().err();
    }

    fn is_ready(&self) -> bool {
        true
    }
}
RUST
run_check
assert_exit "delete before close must FAIL" 1

# -- Should FAIL: exact cleanup lines inside a raw string are not executable --
setup_fake_repo
mkdir -p "$FAKE_REPO/src/transports"
cat > "$FAKE_REPO/src/transports/emscripten_websocket.rs" << 'RUST'
impl Transport {
    fn poll_close(&mut self) {
        let _spoof = r#"
        let close_error = self.close_native_socket().err();
        let delete_result = self.delete_after_close_attempt();
"#;
    }

    fn is_ready(&self) -> bool {
        true
    }
}
RUST
run_check
assert_exit "raw-string cleanup lines must not satisfy poll_close" 1

# -- Should FAIL: ordinary multiline string contents are not executable --
setup_fake_repo
mkdir -p "$FAKE_REPO/src/transports"
cat > "$FAKE_REPO/src/transports/emscripten_websocket.rs" << 'RUST'
impl Transport {
    fn poll_close(&mut self) {
        let _spoof = "
        let close_error = self.close_native_socket().err();
        let delete_result = self.delete_after_close_attempt();
";
    }

    fn is_ready(&self) -> bool {
        true
    }
}
RUST
run_check
assert_exit "ordinary multiline-string cleanup lines must not satisfy poll_close" 1

# -- Should FAIL: exact cleanup lines inside a block comment are not executable --
setup_fake_repo
mkdir -p "$FAKE_REPO/src/transports"
cat > "$FAKE_REPO/src/transports/emscripten_websocket.rs" << 'RUST'
impl Transport {
    fn poll_close(&mut self) {
/*
        let close_error = self.close_native_socket().err();
        let delete_result = self.delete_after_close_attempt();
*/
    }

    fn is_ready(&self) -> bool {
        true
    }
}
RUST
run_check
assert_exit "block-comment cleanup lines must not satisfy poll_close" 1

printf '\n'
printf '%s\n' "=== callback-state reclamation tests ==="

# -- Should PASS: tungstenite's exact framing constructor is not reclamation --
setup_fake_repo
cat > "$FAKE_REPO/src/from_raw_socket_constructor.rs" << 'RUST'
async fn wrap_socket(socket: DuplexStream) {
    let _stream =
        tokio_tungstenite::WebSocketStream::from_raw_socket(socket, Role::Client, None).await;
}
RUST
run_check
assert_exit "from_raw_socket constructor is not ownership reclamation" 0

# -- Should FAIL: a lookalike type is not the exact tungstenite constructor --
setup_fake_repo
cat > "$FAKE_REPO/src/lookalike_from_raw_socket.rs" << 'RUST'
unsafe fn reconstruct(socket: RawSocket) {
    let _stream = other_crate::WebSocketStream::from_raw_socket(socket);
}
RUST
run_check
assert_exit "lookalike from_raw_socket APIs must still FAIL" 1

# -- Should FAIL: a crate-name suffix cannot impersonate tokio-tungstenite --
setup_fake_repo
cat > "$FAKE_REPO/src/suffix_lookalike_from_raw_socket.rs" << 'RUST'
unsafe fn reconstruct(socket: RawSocket) {
    let _stream = evil_tokio_tungstenite::WebSocketStream::from_raw_socket(socket);
}
RUST
run_check
assert_exit "suffix lookalike from_raw_socket APIs must still FAIL" 1

# -- Should FAIL: whitespace after :: cannot impersonate the exact call line --
setup_fake_repo
cat > "$FAKE_REPO/src/whitespace_namespace_from_raw_socket.rs" << 'RUST'
unsafe fn reconstruct(socket: RawSocket) {
    let _stream = evil:: tokio_tungstenite::WebSocketStream::from_raw_socket(socket);
}
RUST
run_check
assert_exit "whitespace-qualified from_raw_socket APIs must still FAIL" 1

# -- Should FAIL: longer from_raw ownership APIs remain audited --
setup_fake_repo
cat > "$FAKE_REPO/src/longer_from_raw_ownership_apis.rs" << 'RUST'
unsafe fn reconstruct(fd: RawFd, socket: RawSocket, ptr: *mut u8) {
    let _fd = OwnedFd::from_raw_fd(fd);
    let _socket = OwnedSocket::from_raw_socket(socket);
    let _boxed = Box::from_raw_in(ptr, Global);
}
RUST
run_check
assert_exit "longer from_raw ownership APIs must still FAIL" 1

# -- Should FAIL: benign constructors cannot mask reclamation on one line --
setup_fake_repo
cat > "$FAKE_REPO/src/mixed_constructor_and_reclaim.rs" << 'RUST'
unsafe fn mixed(socket: DuplexStream, ptr: *mut u8) {
    let _ = (tokio_tungstenite::WebSocketStream::from_raw_socket(socket, Role::Client, None), Box::from_raw(ptr));
}
RUST
run_check
assert_exit "from_raw_socket cannot mask same-line ownership reclamation" 1

setup_fake_repo
cat > "$FAKE_REPO/src/mixed_slice_and_reclaim.rs" << 'RUST'
unsafe fn mixed(ptr: *mut u8, len: usize) {
    let _ = (std::slice::from_raw_parts(ptr, len), Box::from_raw(ptr));
}
RUST
run_check
assert_exit "from_raw_parts cannot mask same-line ownership reclamation" 1

# -- Should FAIL: deletion failure can fall through to unconditional reclamation --
setup_fake_repo
cat > "$FAKE_REPO/src/unconditional_callback_reclaim.rs" << 'RUST'
#[cfg(not(target_os = "emscripten"))]
compile_error!("This module requires the emscripten target.");

unsafe fn cleanup(socket: i32, state_ptr: *mut u8) {
    let delete_result = emscripten_websocket_delete(socket);
    if delete_result != EMSCRIPTEN_RESULT_SUCCESS {
        warn_delete_failed(delete_result);
    }
    drop(Box::from_raw(state_ptr));
}
RUST
run_check
assert_exit "callback state reclaimed after an unchecked delete failure should FAIL" 1

# -- Should FAIL: a comment cannot spoof the required success guard --
setup_fake_repo
cat > "$FAKE_REPO/src/comment_spoofed_callback_reclaim.rs" << 'RUST'
#[cfg(not(target_os = "emscripten"))]
compile_error!("This module requires the emscripten target.");

unsafe fn cleanup(socket: i32, state_ptr: *mut u8) {
    let delete_result = emscripten_websocket_delete(socket);
    // if delete_result == EMSCRIPTEN_RESULT_SUCCESS {
    warn_delete_result(delete_result);
    drop(Box::from_raw(state_ptr));
}
RUST
run_check
assert_exit "commented success guard must not authorize callback reclamation" 1

# -- Should FAIL: a closed success block does not dominate later reclamation --
setup_fake_repo
cat > "$FAKE_REPO/src/closed_success_guard.rs" << 'RUST'
#[cfg(not(target_os = "emscripten"))]
compile_error!("This module requires the emscripten target.");

unsafe fn cleanup(socket: i32, state_ptr: *mut u8) {
    let delete_result = emscripten_websocket_delete(socket);
    if delete_result == EMSCRIPTEN_RESULT_SUCCESS {
        note_success();
    }
    drop(Box::from_raw(state_ptr));
}
RUST
run_check
assert_exit "closed success guard must not authorize later reclamation" 1

# -- Should FAIL: a block comment cannot spoof the required success branch --
setup_fake_repo
cat > "$FAKE_REPO/src/block_comment_spoofed_callback_reclaim.rs" << 'RUST'
#[cfg(not(target_os = "emscripten"))]
compile_error!("This module requires the emscripten target.");

unsafe fn cleanup(socket: i32, state_ptr: *mut u8) {
    let delete_result = emscripten_websocket_delete(socket);
    /* if delete_result == EMSCRIPTEN_RESULT_SUCCESS {
       this is not executable code
    } */
    drop(Box::from_raw(state_ptr));
}
RUST
run_check
assert_exit "block-commented success guard must not authorize reclamation" 1

# -- Should FAIL: a closed pre-registration branch cannot exempt later free --
setup_fake_repo
cat > "$FAKE_REPO/src/closed_creation_failure_guard.rs" << 'RUST'
#[cfg(not(target_os = "emscripten"))]
compile_error!("This module requires the emscripten target.");

unsafe fn cleanup(socket: i32, state_ptr: *mut u8) {
    if socket <= 0 {
        report_creation_failure();
    }
    let _delete_result = emscripten_websocket_delete(socket);
    drop(Box::from_raw(state_ptr));
}
RUST
run_check
assert_exit "closed creation-failure guard must not authorize later reclamation" 1

# -- Should FAIL: qualified/aliased from_raw spellings remain audited --
setup_fake_repo
cat > "$FAKE_REPO/src/qualified_unconditional_reclaim.rs" << 'RUST'
#[cfg(not(target_os = "emscripten"))]
compile_error!("This module requires the emscripten target.");

unsafe fn cleanup(socket: i32, state_ptr: *mut u8) {
    let _delete_result = emscripten_websocket_delete(socket);
    drop(std::boxed::Box::from_raw(state_ptr));
}
RUST
run_check
assert_exit "qualified unconditional from_raw must still FAIL" 1

# -- Should FAIL: taking from_raw as a function item still reconstructs ownership --
setup_fake_repo
cat > "$FAKE_REPO/src/from_raw_function_alias.rs" << 'RUST'
unsafe fn cleanup(state_ptr: *mut CallbackState) {
    let reclaim: unsafe fn(*mut CallbackState) -> Box<CallbackState> = Box::from_raw;
    drop(reclaim(state_ptr));
}
RUST
run_check
assert_exit "from_raw function-item alias must FAIL" 1

# -- Should FAIL: comments cannot split the ownership reconstruction token --
setup_fake_repo
cat > "$FAKE_REPO/src/comment_split_from_raw.rs" << 'RUST'
unsafe fn cleanup(state_ptr: *mut CallbackState) {
    drop(Box:: /* audit gap */ from_raw(state_ptr));
}
RUST
run_check
assert_exit "comment-split from_raw ownership reconstruction must FAIL" 1

# -- Should FAIL: allocator deallocation aliases bypass the typed owner --
setup_fake_repo
cat > "$FAKE_REPO/src/dealloc_function_alias.rs" << 'RUST'
unsafe fn cleanup(state_ptr: *mut u8, layout: Layout) {
    let release = std::alloc::dealloc;
    release(state_ptr, layout);
}
RUST
run_check
assert_exit "allocator deallocation alias must FAIL" 1

# -- Should FAIL: transmute can reconstruct owning state without authorization --
setup_fake_repo
cat > "$FAKE_REPO/src/transmute_reclaim.rs" << 'RUST'
unsafe fn cleanup(state_ptr: *mut CallbackState) {
    let state: Box<CallbackState> = std::mem::transmute(state_ptr);
    drop(state);
}
RUST
run_check
assert_exit "transmute ownership reconstruction must FAIL" 1

# -- Should FAIL: branch-local raw reclamation bypasses the typed owner --
setup_fake_repo
cat > "$FAKE_REPO/src/guarded_callback_reclaim.rs" << 'RUST'
#[cfg(not(target_os = "emscripten"))]
compile_error!("This module requires the emscripten target.");

unsafe fn cleanup(socket: i32, state_ptr: *mut u8) {
    let delete_result = emscripten_websocket_delete(socket);
    if delete_result == EMSCRIPTEN_RESULT_SUCCESS {
        drop(Box::from_raw(state_ptr));
    }
}
RUST
run_check
assert_exit "raw branch-local reclamation must use the typed owner" 1

# -- Should FAIL: callback allocation must not precede socket creation --
setup_fake_repo
cat > "$FAKE_REPO/src/pre_registration_reclaim.rs" << 'RUST'
#[cfg(not(target_os = "emscripten"))]
compile_error!("This module requires the emscripten target.");

unsafe fn connect(socket: i32, state_ptr: *mut u8) {
    if socket <= 0 {
        drop(Box::from_raw(state_ptr));
        return;
    }
    let _ = emscripten_websocket_delete(socket);
}
RUST
run_check
assert_exit "pre-registration raw reclamation is no longer permitted" 1

# -- Should PASS: the sole owner consumes typed authorization and takes once --
setup_fake_repo
mkdir -p "$FAKE_REPO/src/transports"
cat > "$FAKE_REPO/src/transports/emscripten_websocket.rs" << 'RUST'
struct ReclaimAuthorization(());
struct RegisteredCallbackState(Option<NonNull<u8>>);

impl RegisteredCallbackState {
    fn reclaim(&mut self, _authorization: ReclaimAuthorization) {
        let Some(state_ptr) = self.0.take() else {
            return;
        };
        unsafe { drop(Box::from_raw(state_ptr.as_ptr())) };
    }
}

impl Transport {
    fn poll_close(&mut self) {
        let close_error = self.close_native_socket().err();
        let delete_result = self.delete_after_close_attempt();
    }

    fn is_ready(&self) -> bool {
        true
    }
}
RUST
run_check
assert_exit "typed exactly-once reclamation boundary should PASS" 0

printf '\n'
echo "=== will_wake() reference argument tests (Check 6 retired) ==="
echo "  Check 6 is retired — .will_wake ref enforcement is now handled by clippy."
echo "  All will_wake test cases should PASS since Check 6 is a no-op."

# -- Should PASS: .will_wake(noop) without & (retired check) --
setup_fake_repo
cat > "$FAKE_REPO/src/will_wake_no_ref.rs" << 'RUST'
fn poll_something(cx: &mut Context<'_>, old_waker: &Waker) {
    if !old_waker.will_wake(noop) {
        // re-register
    }
}
RUST
run_check
assert_exit ".will_wake(noop) without & should PASS (check retired)" 0

# -- Should PASS: .will_wake(&noop) with & --
setup_fake_repo
cat > "$FAKE_REPO/src/will_wake_with_ref.rs" << 'RUST'
fn poll_something(cx: &mut Context<'_>, old_waker: &Waker) {
    if !old_waker.will_wake(&noop) {
        // re-register
    }
}
RUST
run_check
assert_exit ".will_wake(&noop) with & should PASS" 0

# -- Should PASS: .will_wake( inside a comment --
setup_fake_repo
cat > "$FAKE_REPO/src/will_wake_in_comment.rs" << 'RUST'
fn poll_something(cx: &mut Context<'_>) {
    // Previously used .will_wake(old) here but removed it
    let _ = cx.waker();
}
RUST
run_check
assert_exit ".will_wake( inside a comment should PASS" 0

# -- Should PASS: No .will_wake() calls at all --
setup_fake_repo
cat > "$FAKE_REPO/src/no_will_wake.rs" << 'RUST'
fn poll_something(cx: &mut Context<'_>) {
    let waker = cx.waker().clone();
    // no will_wake usage
}
RUST
run_check
assert_exit "No .will_wake() calls at all should PASS" 0

# -- Should PASS: Multi-line .will_wake( with & on next line --
setup_fake_repo
cat > "$FAKE_REPO/src/will_wake_multiline_ref.rs" << 'RUST'
fn poll_something(cx: &mut Context<'_>) {
    let noop = std::task::Waker::noop();
    if !_cx.waker().will_wake(
        &noop
    ) {
        // re-register
    }
}
RUST
run_check
assert_exit "Multi-line .will_wake( with & on next line should PASS" 0

# -- Should PASS: Multi-line .will_wake( WITHOUT & on next line (retired check) --
setup_fake_repo
cat > "$FAKE_REPO/src/will_wake_multiline_no_ref.rs" << 'RUST'
fn poll_something(cx: &mut Context<'_>) {
    let noop = std::task::Waker::noop();
    if !_cx.waker().will_wake(
        noop
    ) {
        // re-register
    }
}
RUST
run_check
assert_exit "Multi-line .will_wake( WITHOUT & on next line should PASS (check retired)" 0

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
