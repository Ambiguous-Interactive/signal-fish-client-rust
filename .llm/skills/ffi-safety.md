# FFI Safety

Reference for writing correct and safe FFI bindings, with emphasis on C type mapping, struct layout, pointer lifecycle, and cleanup patterns.

## C Type Mapping

Emscripten (and many C APIs) use integer-sized types where Rust has narrower equivalents. Always match the C type exactly.

### EM_BOOL Is `c_int`, Not `bool`

Emscripten's `EM_BOOL` is defined as `int` (4 bytes) in the C header. Rust's `bool` is 1 byte. Using the wrong type in FFI bindings silently corrupts struct layout.

```rust
use std::os::raw::c_int;

// CORRECT: matches the C typedef
type EM_BOOL = c_int;

// WRONG: 1 byte instead of 4, corrupts all subsequent fields
// type EM_BOOL = bool;
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
// C header:
// struct EmscriptenWebSocketOpenEvent {
//     int socket;             // 4 bytes
//     EM_BOOL isSecure;       // 4 bytes (int, NOT bool)
//     const char *url;        // pointer-sized
// };

#[repr(C)]
pub struct EmscriptenWebSocketOpenEvent {
    pub socket: c_int,        // 4 bytes - correct
    pub is_secure: EM_BOOL,   // 4 bytes - correct (c_int)
    pub url: *const c_char,   // pointer - correct
}
```

If `is_secure` were declared as `bool` (1 byte + 3 bytes padding on some targets, or no padding on others), the `url` pointer would read from the wrong offset, producing an invalid address and likely a segfault or silent data corruption.

### Alignment Checklist

- Match every field's type to the C header, not to its logical meaning
- Preserve the exact field order from the C struct definition
- Use `#[repr(C)]` on every struct passed across the FFI boundary
- Run `std::mem::size_of::<YourStruct>()` in tests and compare against `sizeof(CStruct)` when possible

## FFI Return Value Checking

### Always Check Return Values

C functions communicate failure through return values. Ignoring them leads to silent failures that manifest as crashes later.

```rust
let result = emscripten_websocket_set_onopen_callback_on_thread(
    socket, user_data, Some(on_open_callback), thread_id,
);
if result != EMSCRIPTEN_RESULT_SUCCESS {
    emscripten_websocket_close(socket, 1000, ptr::null());
    drop(Box::from_raw(user_data as *mut State));
    return Err(format!("onopen registration failed: {result}"));
}
```

### Pattern: Register-and-Rollback

When registering multiple callbacks, roll back on first failure:

```rust
let registrations = [
    ("onopen", register_onopen(socket, user_data)),
    ("onmessage", register_onmessage(socket, user_data)),
    ("onclose", register_onclose(socket, user_data)),
    ("onerror", register_onerror(socket, user_data)),
];
for (name, result) in &registrations {
    if *result != EMSCRIPTEN_RESULT_SUCCESS {
        unsafe {
            emscripten_websocket_close(socket, 1000, ptr::null());
            drop(Box::from_raw(user_data as *mut State));
        }
        return Err(format!("{name} callback registration failed: {result}"));
    }
}
```

## Raw Pointer Lifecycle

### `Box::into_raw` and `Box::from_raw`

`Box::into_raw` leaks memory intentionally — ownership transfers to the raw pointer. You must reclaim it with `Box::from_raw` exactly once.

```rust
// Allocate and leak
let state = Box::new(CallbackState { /* ... */ });
let raw: *mut CallbackState = Box::into_raw(state);

// Pass raw pointer as user_data to C callbacks
register_callback(raw as *mut c_void);

// Later, reclaim exactly once (usually in Drop or a close handler)
unsafe {
    let _state = Box::from_raw(raw);
    // _state is dropped here, freeing the memory
}
```

### Rules

- Every `Box::into_raw` must have exactly one matching `Box::from_raw`
- Zero calls: memory leak
- Two calls: double-free (undefined behavior)
- Reclaim AFTER all callbacks that reference the pointer have been unregistered

### Cleanup Order

Clean up resources in an order that prevents use-after-free:

1. **Close** the handle (may trigger synchronous callbacks — state pointer must still be valid)
2. **Delete/unregister** callbacks (prevents any further callback access to state)
3. **Reclaim** the state pointer via `Box::from_raw` (safe — no callbacks can fire)

### `close()` Must Also Unregister Callbacks

A `close()` method that only closes the handle but does **not** unregister callbacks creates a window where callbacks can still fire between `close()` returning and `Drop` running. On the single-threaded Emscripten model this only matters if a JavaScript event loop tick occurs in that window, but the safe pattern is to **always unregister callbacks in `close()`**.

Use a `deleted: bool` flag to prevent double-unregistration in `Drop`:

```rust,ignore
async fn close(&mut self) -> Result<(), Error> {
    if self.closed { return Ok(()); }
    self.closed = true;
    emscripten_websocket_close(self.socket, 1000, ptr::null());
    emscripten_websocket_delete(self.socket); // unregister callbacks NOW
    self.deleted = true;
    Ok(())
}

impl Drop for Transport {
    fn drop(&mut self) {
        if !self.closed {
            emscripten_websocket_close(self.socket, 1000, ptr::null());
        }
        if !self.deleted {
            emscripten_websocket_delete(self.socket);
        }
        drop(Box::from_raw(self.state_ptr)); // always reclaim
    }
}
```

### Error Path Cleanup Must Match `close()` + `Drop`

Every error path that cleans up FFI resources **must** follow the same sequence as `close()` + `Drop`. A common bug: `Drop` does close -> delete -> free correctly, but an error path or `close()` skips `delete`, leaving callbacks registered.

**Checklist for FFI cleanup paths:**

- [ ] Does `close()` both close the handle AND unregister callbacks?
- [ ] Does `Drop` skip steps already performed by `close()` (using boolean flags)?
- [ ] Does `Drop` always reclaim heap-allocated state (`Box::from_raw`) as the final step?
- [ ] Does every constructor error path follow the same close -> delete -> free sequence?

## Single-Threaded Safety

### `unsafe impl Send` on wasm32-unknown-emscripten

The `wasm32-unknown-emscripten` target is single-threaded. Types that hold raw pointers or other `!Send` fields can safely implement `Send` on this target because no concurrent access is possible.

```rust
// SAFETY: wasm32-unknown-emscripten is single-threaded. The Send bound is
// required by the Transport trait but is vacuously satisfied since there
// are no other threads.
unsafe impl Send for EmscriptenWebSocketTransport {}
```

### Rules

- Always include a `// SAFETY:` comment explaining the single-threaded assumption
- If the containing module is already feature-gated to emscripten-only, the module-level gate is sufficient. Otherwise, gate the impl directly with `#[cfg(target_os = "emscripten")]`
- Never add `unsafe impl Sync` unless the type is genuinely safe for shared references (rare for FFI wrappers)

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

### Why This Matters

- Consistent per-function comments make safety audits easier at a glance
- A missing comment on one callback (while others have it) creates doubt about whether the safety analysis was done
- `check-ffi-safety.sh` enforces this automatically

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

- [ ] All `#[repr(C)]` struct fields match the C header types exactly (`EM_BOOL` = `c_int`, not `bool`)
- [ ] Field order matches the C header exactly
- [ ] All return values from FFI functions are checked
- [ ] Error paths follow the **same cleanup sequence** as `close()` + `Drop`
- [ ] Raw pointer lifetimes are documented with `// SAFETY:` comments
- [ ] Callback `user_data` lifetime outlives all possible callback invocations
- [ ] `close()` both closes the handle AND unregisters callbacks (no window for late callbacks)
- [ ] `Drop` skips steps already done by `close()` using boolean flags (`closed`, `deleted`)
- [ ] `Drop` always reclaims heap state (`Box::from_raw`) as the final step
- [ ] Target-restricted FFI modules have a `compile_error!()` guard at the file top
- [ ] Every `extern "C" fn` in files with a shared SAFETY block has a per-function `// SAFETY:` comment

## Common Mistakes

| Mistake | Symptom | Fix |
|---------|---------|-----|
| `bool` for `EM_BOOL` | All fields after the `bool` read garbage | Use `c_int` |
| Missing return value check | Silent callback registration failure | Check every FFI return value |
| Double `Box::from_raw` | Double-free crash or UB | Track ownership, reclaim exactly once |
| Wrong cleanup order | Use-after-free in callbacks | Close socket before reclaiming state |
| Error path skips cleanup step | Resource leak (e.g., missing `delete` between `close` and `free`) | Mirror `close()` + `Drop` sequence exactly |
| `close()` skips callback unregistration | Late callbacks fire between `close()` and `Drop` | Call `delete`/unregister in `close()`, use `deleted` flag to prevent double-delete in `Drop` |
| `unsafe impl Send` without safety justification | Unsound on multi-threaded targets | Document single-threaded assumption; gate at module or impl level |
| Missing per-function SAFETY comment on callback | Inconsistent safety documentation, harder to audit | Add `// SAFETY:` referencing the block comment before every `extern "C" fn` |
