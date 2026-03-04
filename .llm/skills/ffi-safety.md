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

C functions communicate failure through return values. Ignoring them leads to silent failures that manifest as hard-to-debug crashes later.

```rust
// CORRECT: check every registration result
let result = emscripten_websocket_set_onopen_callback_on_thread(
    socket, user_data, Some(on_open_callback), thread_id,
);
if result != EMSCRIPTEN_RESULT_SUCCESS {
    // Clean up before returning error
    emscripten_websocket_close(socket, 1000, ptr::null());
    drop(Box::from_raw(user_data as *mut State));
    return Err(format!("onopen registration failed: {result}"));
}
```

### Pattern: Register-and-Rollback

When registering multiple callbacks, collect results and roll back on first failure:

```rust
let registrations = [
    ("onopen", register_onopen(socket, user_data)),
    ("onmessage", register_onmessage(socket, user_data)),
    ("onclose", register_onclose(socket, user_data)),
    ("onerror", register_onerror(socket, user_data)),
];

for (name, result) in &registrations {
    if *result != EMSCRIPTEN_RESULT_SUCCESS {
        // Clean up: close socket, reclaim Box pointer
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

```rust
impl Drop for EmscriptenWebSocket {
    fn drop(&mut self) {
        unsafe {
            // 1. Close the socket (callback may fire synchronously here;
            //    state pointer is still valid at this point).
            emscripten_websocket_close(self.socket, 1000, ptr::null());

            // 2. Delete the socket handle — this implicitly unregisters
            //    all callbacks, preventing further access to the state pointer.
            emscripten_websocket_delete(self.socket);

            // 3. Reclaim the state pointer (safe now — no callbacks can fire)
            drop(Box::from_raw(self.state_ptr));
        }
    }
}
```

### Error Path Cleanup Must Match `Drop`

A common FFI bug: the `Drop` implementation follows the correct cleanup
sequence (close -> delete -> free), but error paths in other methods skip
one or more steps. Every error path that cleans up FFI resources **must**
follow the same sequence as `Drop`.

```rust,ignore
// Drop does: close -> delete -> free (correct)
impl Drop for EmscriptenWebSocket {
    fn drop(&mut self) {
        unsafe {
            emscripten_websocket_close(self.socket, 1000, ptr::null());
            emscripten_websocket_delete(self.socket);
            drop(Box::from_raw(self.state_ptr));
        }
    }
}

// BAD: error path skips delete — leaks the socket handle
fn setup_callbacks(&mut self) -> Result<(), String> {
    let result = register_onopen(self.socket, self.state_ptr);
    if result != EMSCRIPTEN_RESULT_SUCCESS {
        unsafe {
            emscripten_websocket_close(self.socket, 1000, ptr::null());
            // BUG: missing emscripten_websocket_delete()
            drop(Box::from_raw(self.state_ptr));
        }
        return Err("onopen failed".into());
    }
    Ok(())
}

// GOOD: error path mirrors Drop exactly
fn setup_callbacks(&mut self) -> Result<(), String> {
    let result = register_onopen(self.socket, self.state_ptr);
    if result != EMSCRIPTEN_RESULT_SUCCESS {
        unsafe {
            emscripten_websocket_close(self.socket, 1000, ptr::null());
            emscripten_websocket_delete(self.socket);
            drop(Box::from_raw(self.state_ptr));
        }
        return Err("onopen failed".into());
    }
    Ok(())
}
```

**Prevention pattern:** extract the cleanup sequence into a helper so that
`Drop` and all error paths call the same code:

```rust,ignore
impl EmscriptenWebSocket {
    /// Performs the full cleanup sequence: close -> delete -> free.
    unsafe fn cleanup(&mut self) {
        emscripten_websocket_close(self.socket, 1000, ptr::null());
        emscripten_websocket_delete(self.socket);
        drop(Box::from_raw(self.state_ptr));
    }
}
```

**Checklist for FFI error paths:**

- [ ] Does every error path that cleans up resources follow the same
      sequence as `Drop`?
- [ ] Are all steps present (close, delete/unregister, free)?
- [ ] Consider extracting cleanup into a shared helper to prevent drift.

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
- [ ] Error paths follow the **same cleanup sequence** as `Drop` (see "Error Path Cleanup Must Match `Drop`")
- [ ] Raw pointer lifetimes are documented with `// SAFETY:` comments
- [ ] Callback `user_data` lifetime outlives all possible callback invocations
- [ ] `Drop` impl cleans up resources in the correct order (close -> unregister -> delete -> reclaim)
- [ ] Target-restricted FFI modules have a `compile_error!()` guard at the file top

## Common Mistakes

| Mistake | Symptom | Fix |
|---------|---------|-----|
| `bool` for `EM_BOOL` | All fields after the `bool` read garbage | Use `c_int` |
| Missing return value check | Silent callback registration failure | Check every FFI return value |
| Double `Box::from_raw` | Double-free crash or UB | Track ownership, reclaim exactly once |
| Wrong cleanup order | Use-after-free in callbacks | Close socket before reclaiming state |
| Error path skips cleanup step | Resource leak (e.g., missing `delete` between `close` and `free`) | Mirror `Drop` sequence exactly; extract a shared helper |
| `unsafe impl Send` without safety justification | Unsound on multi-threaded targets | Document single-threaded assumption; gate at module or impl level |
