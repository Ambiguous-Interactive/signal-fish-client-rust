---
name: ffi-safety
description: Implement and review safe native or WebAssembly FFI boundaries. Use when changing C type mappings, struct layout, callbacks, pointer lifetimes, cleanup, or target-gated bindings.
---

# FFI Safety

Reference for writing correct and safe FFI bindings, with emphasis on C type mapping, struct layout, pointer lifecycle, and cleanup patterns.

## C Type Mapping

Match the upstream header exactly: Emscripten `websocket.h` uses one-byte C
`bool` struct fields, while callback returns use integer-sized `EM_BOOL`.

### WebSocket struct bools and callback `EM_BOOL` are distinct

The callback alias shifts one-byte fields; bare Rust `bool` is unsupported, so use an explicit integer field alias.

```rust
use std::os::raw::c_int;

// Callback return value in this binding.
type EM_BOOL = c_int;

// Fields declared as bool in websocket.h on wasm32-unknown-emscripten.
type C_BOOL = u8;
```

### Common Emscripten Type Aliases

```rust
use std::os::raw::{c_int, c_double, c_long, c_ulong, c_ushort, c_char};

type EM_BOOL = c_int;
type EMSCRIPTEN_RESULT = c_int;
type EM_UTF8 = c_char;
```

### Verification Rule

Before writing any `#[repr(C)]` binding, open the upstream C header and verify every field type. Never guess based on semantic meaning (e.g., "it's a boolean flag so it must be `bool`").

## Struct Layout

### `#[repr(C)]` Field Order and Size

A `#[repr(C)]` struct lays out fields sequentially with C alignment rules. A single wrong-sized field shifts ALL subsequent field offsets, causing every read after the mistake to return garbage.

```rust
// websocket.h:
// struct EmscriptenWebSocketMessageEvent {
//     int socket;
//     uint8_t *data;
//     uint32_t numBytes;
//     bool isText;
// };

#[repr(C)]
pub struct EmscriptenWebSocketMessageEvent {
    pub socket: c_int,
    pub data: *const u8,
    pub num_bytes: u32,
    pub is_text: C_BOOL,
}
```

Using `EM_BOOL` for `is_text`, `was_clean`, or `create_on_main_thread`
corrupts the layout by widening a one-byte field to four bytes.

### Alignment Checklist

- Match every field's type to the C header, not to its logical meaning
- Preserve the exact field order from the C struct definition
- Use `#[repr(C)]` on every struct passed across the FFI boundary
- Run `std::mem::size_of::<YourStruct>()` in tests and compare against `sizeof(CStruct)` when possible

## FFI Return Value Checking

### Always Check Return Values

C functions communicate failure through return values. Ignoring them leads to silent failures that manifest as crashes later.

```rust
// SAFETY: `socket` is owned by this transport and `user_data` points to the
// live callback state allocated for it.
let result = unsafe {
    emscripten_websocket_set_onopen_callback_on_thread(
        socket, user_data, Some(on_open_callback), thread_id,
    )
};
if result != EMSCRIPTEN_RESULT_SUCCESS {
    // SAFETY: the socket remains owned here; close is attempted before delete.
    let close_result = unsafe { emscripten_websocket_close(socket, 1000, ptr::null()) };
    // SAFETY: this is the sole cleanup path for the still-owned socket handle.
    let delete_result = unsafe { emscripten_websocket_delete(socket) };
    if delete_result == EMSCRIPTEN_RESULT_SUCCESS {
        // SAFETY: successful deletion unregisters callbacks, so the foreign
        // runtime can no longer access the uniquely owned callback state.
        unsafe { drop(Box::from_raw(user_data as *mut State)) };
    }
    // If deletion failed, preserve (and ultimately leak) callback state: the
    // foreign runtime may still invoke a callback with `user_data`.
    return Err(format!(
        "onopen registration failed: {result}; close: {close_result}; delete: {delete_result}"
    ));
}
```

### Pattern: Register-and-Rollback

When registering multiple callbacks, iterate over results and roll back on the
first failure: attempt close, then delete/unregister, and reclaim state with
`Box::from_raw` only after deletion succeeds. If deletion fails, preserve the
callback state so a later foreign callback cannot access freed memory. See the
codebase for the complete retry-and-safe-leak state machine.

## Raw Pointer Lifecycle

### `Box::into_raw` and `Box::from_raw`

`Box::into_raw` transfers ownership to a raw pointer. Reclaim it with
`Box::from_raw` exactly once, but only after the foreign API proves that no
callback can use it again.

```rust
// Allocate and leak
let state = Box::new(CallbackState { /* ... */ });
let raw: *mut CallbackState = Box::into_raw(state);

// Pass raw pointer as user_data to C callbacks
register_callback(raw as *mut c_void);

// Later, after callback deletion succeeds, reclaim exactly once.
if callbacks_were_deleted {
    unsafe {
        let _state = Box::from_raw(raw);
        // _state is dropped here, freeing the memory
    }
}
```

### Rules

- Every successfully unregistered `Box::into_raw` allocation must have exactly
  one matching `Box::from_raw`
- Zero calls after confirmed unregistration: memory leak
- Two calls: double-free (undefined behavior)
- Reclaim AFTER all callbacks that reference the pointer have been unregistered
- If unregistration fails, intentionally retain/leak the allocation rather
  than risk use-after-free

### Cleanup Order

Clean up resources in an order that prevents use-after-free:

1. **Close** the handle (may trigger synchronous callbacks — state pointer must still be valid)
2. **Delete/unregister** callbacks (prevents any further callback access to state)
3. **Reclaim** the state pointer via `Box::from_raw` only after confirmed
   deletion (safe — no callbacks can fire)

### `poll_close`, `abort`, and `Drop` Must Attempt Callback Unregistration

A `poll_close` method that only closes the handle but does **not** unregister
callbacks creates a window where callbacks can still fire before `Drop`. The
safe pattern is to attempt callback deletion during `poll_close`, `abort`, and
`Drop`, using shared state that makes successful steps one-shot and failed
steps retryable.

Use an ownership state machine to authorize reclamation only after a close
attempt and successful deletion:

```rust,ignore
fn cleanup(&mut self) -> Result<(), Error> {
    self.try_close();
    match self.try_delete_callbacks() {
        Ok(reclaim_authorization) => {
            self.callback_state.reclaim(reclaim_authorization);
            Ok(())
        }
        Err(error) => Err(error), // state remains live and retryable
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        if self.cleanup().is_err() {
            // The foreign runtime may still invoke callbacks. Intentionally
            // retain the backing allocation to prevent use-after-free.
        }
    }
}
```

### Error Path Cleanup Must Match `poll_close` + `abort` + `Drop`

Every error path that cleans up FFI resources **must** follow the same sequence
as `poll_close` + `abort` + `Drop`. A common bug is reclaiming callback state
after deletion failed, leaving the foreign runtime with a dangling pointer.

**Checklist for FFI cleanup paths:**

- [ ] Do `poll_close`, `abort`, and `Drop` attempt close before callback deletion?
- [ ] Does cleanup skip successful steps and safely retry failed ones?
- [ ] Is heap state reclaimed only after confirmed callback deletion?
- [ ] Does terminal deletion failure intentionally preserve callback state?
- [ ] Does every constructor error path follow close -> delete -> conditional reclaim?

## Single-Threaded Safety

The `Transport` trait has no `Send` bound. Keep an Emscripten transport
explicitly `!Send` when it owns main-thread callback state, and use it with
`SignalFishPollingClient`. Do not add `unsafe impl Send` merely because the
current runtime configuration is single-threaded; the async client applies
`Send + 'static` separately at its Tokio task-spawn boundary.

## Callback SAFETY Comment Convention

When a file has multiple `extern "C" fn` callbacks sharing common safety invariants (pointer validity, single-threaded execution, etc.), use a shared block comment plus per-function references:

```rust
// SAFETY (all callbacks): These `extern "C"` functions are registered with
// Emscripten's WebSocket API. The runtime guarantees that:
// - `user_data` is the same pointer passed during registration
// - `event` pointers are valid for the callback duration
// - Callbacks are invoked on the main thread (single-threaded model)

// SAFETY: See the callback SAFETY block comment above for pointer guarantees.
extern "C" fn on_open_callback(...) -> EM_BOOL { ... }

// SAFETY: See the callback SAFETY block comment above for pointer guarantees.
extern "C" fn on_message_callback(...) -> EM_BOOL { ... }
```

### Rules

- Every `extern "C" fn` in a file with a SAFETY block comment MUST have its own `// SAFETY:` comment on the line immediately before the `extern "C" fn` declaration
- The per-function comment should reference the block comment, not duplicate it
- Do NOT add redundant inline SAFETY comments inside the function body that duplicate the per-function comment
- Enforced by `check-ffi-safety.sh` (Check 4)

## Debug Assertions for FFI Transport Misuse

FFI-backed transports on WASM/Emscripten targets (like `EmscriptenWebSocketTransport`)
are designed for noop-waker polling only. When accidentally used with a real async
runtime, the result is a silent hang that is extremely difficult to diagnose --
especially in Godot/Emscripten debugging scenarios where standard debugger support
is limited.

**Do not attempt runtime waker-misuse detection.** `Waker::will_wake` is
documented best-effort: it compares `RawWaker` data plus vtable pointer
identity, and the noop waker's `#[inline] const` internals duplicate that
identity per crate. A check like
`cx.waker().will_wake(Waker::noop())` returned `false` for genuine noop wakers
in a verified two-crate probe (and in the on-target harness), so the check
that once shipped in `EmscriptenWebSocketTransport::poll_recv` false-positived
for every out-of-crate caller in debug builds and was removed. Enforce the
noop-waker contract through documentation and the driver design
(`SignalFishPollingClient` constructs its own noop-waker contexts; see the
`transport-abstraction` skill) instead of a runtime probe.

## Std API Calls in `cfg`-Guarded Code

Code behind `#[cfg(debug_assertions)]` or target-specific `compile_error!()` guards may
not be compiled in normal CI. This creates a blind spot where type errors and API misuse
can hide indefinitely.

### Rules

1. **Always verify argument types for std API calls in cfg-guarded blocks.** The compiler
   won't catch errors in code that's never compiled for CI targets.
2. **`Waker::will_wake` takes `&Waker`.** The compiler auto-refs owned `Waker`
   values, so `.will_wake(noop)` is idiomatic. Do **not** write `.will_wake(&noop)`
   — nightly clippy flags the explicit `&` as `needless_borrow`. The emscripten CI
   job now runs clippy on the actual target, catching type errors directly.
3. **Consider adding static analysis checks** (in `check-ffi-safety.sh`) for known
   patterns that are prone to this class of bug.

## Target-Restricted Features

### compile_error!() Guard for FFI Modules

Feature-gated modules that use FFI bindings available only on a specific target
must include a `compile_error!()` guard at the top of the file. Without this
guard, enabling the feature on the wrong target produces cryptic linker errors
instead of a clear diagnostic.

```rust
// At the top of the file (after module docs, before `use` statements):
#[cfg(not(target_os = "emscripten"))]
compile_error!(
    "The `transport-websocket-emscripten` feature requires the \
     `wasm32-unknown-emscripten` target."
);
```

### Rules

- Every FFI module that links against target-specific C libraries must have a
  `compile_error!()` guard
- The guard goes at the top of the `.rs` file, not in `mod.rs` -- this produces
  a clearer error message pointing at the FFI code
- Document the restriction in `Cargo.toml` with a comment above the feature
- Add a test in `ci_config_tests.rs` to prevent accidental removal of the guard

## Checklist for New FFI Bindings

Use this checklist when adding or reviewing any FFI binding:

- [ ] All `#[repr(C)]` struct fields match the C header exactly (WebSocket C `bool` fields use one-byte `C_BOOL`; callback returns use `EM_BOOL = c_int`)
- [ ] Field order matches the C header exactly
- [ ] All return values from FFI functions are checked
- [ ] Error paths follow the **same cleanup sequence** as `poll_close` + `abort` + `Drop`
- [ ] Raw pointer lifetimes are documented with `// SAFETY:` comments
- [ ] Callback `user_data` lifetime outlives all possible callback invocations
- [ ] `poll_close`, `abort`, and `Drop` attempt close before callback deletion
- [ ] Successful cleanup is one-shot and failed cleanup remains retryable
- [ ] Heap state is reclaimed only after confirmed callback deletion
- [ ] Terminal deletion failure preserves callback backing state
- [ ] Target-restricted FFI modules have a `compile_error!()` guard at the file top
- [ ] Every `extern "C" fn` in files with a shared SAFETY block has a per-function `// SAFETY:` comment
- [ ] All std library API calls in `#[cfg(...)]` blocks use correct argument types (especially reference vs. owned)

## Common Mistakes

| Mistake | Symptom | Fix |
|---------|---------|-----|
| `EM_BOOL` for a WebSocket C `bool` field | Following fields read at the wrong offsets | Use the one-byte `C_BOOL` alias |
| Bare Rust `bool` in a repr(C) binding | Unsupported FFI field ABI | Use the verified integer alias for the header type |
| Missing return value check | Silent callback registration failure | Check every FFI return value |
| Double `Box::from_raw` | Double-free crash or UB | Track ownership, reclaim exactly once |
| Wrong cleanup order | Use-after-free in callbacks | Close socket before reclaiming state |
| Error path skips cleanup step | Resource leak (e.g., missing `delete` between close and free) | Mirror the shared close/delete state machine |
| Deletion failure still frees callback state | Callback use-after-free | Preserve state and allow retry; safety-leak on final failure |
| `unsafe impl Send` added for Emscripten | Main-thread callback state crosses the async spawn boundary | Keep the transport `!Send` and use the polling client |
| Missing per-function SAFETY comment on callback | Inconsistent safety documentation, harder to audit | Add `// SAFETY:` referencing the block comment before every `extern "C" fn` |
| `.will_wake(&waker)` with explicit `&` | Nightly clippy `needless_borrow` warning | Omit the `&` — write `.will_wake(waker)`; the compiler auto-refs. Caught by emscripten CI clippy job |
