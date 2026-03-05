//! Emscripten WebSocket transport implementation using raw FFI to Emscripten's C API.
//!
//! This module provides [`EmscriptenWebSocketTransport`], a [`Transport`]
//! implementation that communicates over a WebSocket connection using
//! Emscripten's built-in `<emscripten/websocket.h>` C API. It is designed
//! for the `wasm32-unknown-emscripten` target (e.g., Godot 4.5 web builds
//! via gdext).
//!
//! # Feature gate
//!
//! This module is only available when the `transport-websocket-emscripten`
//! feature is enabled. The feature gating is applied in `transports/mod.rs`,
//! not in this file.
//!
//! # Threading model
//!
//! On `wasm32-unknown-emscripten`, everything runs on a single thread.
//! Emscripten WebSocket callbacks are invoked synchronously on the main
//! thread. A `std::sync::mpsc` channel bridges callback events into the
//! transport's `recv()` method.
//!
//! # Compatibility
//!
//! This transport is designed exclusively for use with
//! [`SignalFishPollingClient`](crate::SignalFishPollingClient) and a noop-waker
//! polling model. It is **not** compatible with
//! [`SignalFishClient::start()`](crate::SignalFishClient::start) or any real
//! async runtime, because `recv()` uses [`std::future::pending()`] when no
//! messages are buffered (it will never wake).
//!
//! # `recv()` caller contract
//!
//! The `recv()` method uses [`std::future::pending()`] when the internal
//! channel is empty. This means the returned future will **never wake** --
//! it permanently suspends without registering any waker. This is
//! intentional and correct under the noop-waker polling model used by
//! [`SignalFishPollingClient`](crate::SignalFishPollingClient):
//!
//! - **Callers must create a new future on every call.** Each invocation
//!   of `recv()` must produce a fresh future that is polled exactly once.
//!   Never store and re-poll a future returned by `recv()` -- doing so
//!   will hang forever because the pending future has no waker to trigger
//!   progress.
//!
//! - **`std::future::pending().await` intentionally never wakes.** When
//!   no messages are buffered, the transport signals "nothing yet" by
//!   returning `Poll::Pending` via `std::future::pending()`. The polling
//!   client discards this result and retries on the next tick, which
//!   creates a new future that can observe newly arrived messages.
//!
//! - **Only compatible with `SignalFishPollingClient`.** Standard async
//!   runtimes (Tokio, async-std, etc.) expect futures to register a
//!   waker so they can be re-polled when progress is possible. Since
//!   `pending()` never registers a waker, using this transport with
//!   [`SignalFishClient::start()`](crate::SignalFishClient::start) or
//!   any real executor will cause `recv()` to hang indefinitely.
//!
//! - **Debug-build misuse detection.** In `cfg(debug_assertions)` builds,
//!   `recv()` will emit a `tracing::error!` if it detects a non-noop waker,
//!   which indicates the transport is being driven by a real async runtime
//!   instead of `SignalFishPollingClient`. This makes misuse visible
//!   during development rather than manifesting as a silent hang.
//!
//! # Example
//!
//! ```rust,ignore
//! use signal_fish_client::{EmscriptenWebSocketTransport, Transport};
//!
//! let mut transport = EmscriptenWebSocketTransport::connect("wss://server/ws")?;
//! transport.send("hello".to_string()).await?;
//!
//! if let Some(Ok(msg)) = transport.recv().await {
//!     // handle message
//! }
//!
//! transport.close().await?;
//! ```

// Compile-time guard: this module requires the wasm32-unknown-emscripten target.
// The FFI functions (emscripten_websocket_new, etc.) are only available in
// Emscripten's C runtime. Compiling on any other target will produce linker errors.
#[cfg(not(target_os = "emscripten"))]
compile_error!(
    "The `transport-websocket-emscripten` feature requires the `wasm32-unknown-emscripten` target. \
     This module uses Emscripten's C WebSocket API which is unavailable on other targets. \
     Remove this feature or compile with `--target wasm32-unknown-emscripten`."
);

use std::ffi::{c_char, c_int, c_void};
use std::sync::mpsc as std_mpsc;

use async_trait::async_trait;

use crate::error::SignalFishError;
use crate::transport::Transport;

// ── FFI Bindings ────────────────────────────────────────────────────────────

// These type aliases mirror Emscripten's C naming conventions exactly.
#[allow(non_camel_case_types)]
type EMSCRIPTEN_WEBSOCKET_T = c_int;

#[allow(non_camel_case_types)]
type EM_BOOL = c_int;

/// Emscripten result code indicating success.
const EMSCRIPTEN_RESULT_SUCCESS: c_int = 0;

#[repr(C)]
struct EmscriptenWebSocketCreateAttributes {
    url: *const c_char,
    protocols: *const c_char,
    create_on_main_thread: EM_BOOL,
}

#[repr(C)]
struct EmscriptenWebSocketOpenEvent {
    socket: EMSCRIPTEN_WEBSOCKET_T,
}

#[repr(C)]
struct EmscriptenWebSocketMessageEvent {
    socket: EMSCRIPTEN_WEBSOCKET_T,
    data: *const u8,
    num_bytes: u32,
    is_text: EM_BOOL,
}

#[repr(C)]
struct EmscriptenWebSocketErrorEvent {
    socket: EMSCRIPTEN_WEBSOCKET_T,
}

#[repr(C)]
struct EmscriptenWebSocketCloseEvent {
    socket: EMSCRIPTEN_WEBSOCKET_T,
    was_clean: EM_BOOL,
    code: u16,
    reason: [u8; 512],
}

type OnOpenCallback =
    extern "C" fn(c_int, *const EmscriptenWebSocketOpenEvent, *mut c_void) -> EM_BOOL;

type OnMessageCallback =
    extern "C" fn(c_int, *const EmscriptenWebSocketMessageEvent, *mut c_void) -> EM_BOOL;

type OnErrorCallback =
    extern "C" fn(c_int, *const EmscriptenWebSocketErrorEvent, *mut c_void) -> EM_BOOL;

type OnCloseCallback =
    extern "C" fn(c_int, *const EmscriptenWebSocketCloseEvent, *mut c_void) -> EM_BOOL;

extern "C" {
    fn emscripten_websocket_new(
        attrs: *const EmscriptenWebSocketCreateAttributes,
    ) -> EMSCRIPTEN_WEBSOCKET_T;

    fn emscripten_websocket_send_utf8_text(
        socket: EMSCRIPTEN_WEBSOCKET_T,
        text: *const c_char,
    ) -> c_int;

    fn emscripten_websocket_close(
        socket: EMSCRIPTEN_WEBSOCKET_T,
        code: u16,
        reason: *const c_char,
    ) -> c_int;

    fn emscripten_websocket_delete(socket: EMSCRIPTEN_WEBSOCKET_T) -> c_int;

    fn emscripten_websocket_set_onopen_callback_on_thread(
        socket: EMSCRIPTEN_WEBSOCKET_T,
        user_data: *mut c_void,
        callback: Option<OnOpenCallback>,
        target_thread: usize,
    ) -> c_int;

    fn emscripten_websocket_set_onmessage_callback_on_thread(
        socket: EMSCRIPTEN_WEBSOCKET_T,
        user_data: *mut c_void,
        callback: Option<OnMessageCallback>,
        target_thread: usize,
    ) -> c_int;

    fn emscripten_websocket_set_onerror_callback_on_thread(
        socket: EMSCRIPTEN_WEBSOCKET_T,
        user_data: *mut c_void,
        callback: Option<OnErrorCallback>,
        target_thread: usize,
    ) -> c_int;

    fn emscripten_websocket_set_onclose_callback_on_thread(
        socket: EMSCRIPTEN_WEBSOCKET_T,
        user_data: *mut c_void,
        callback: Option<OnCloseCallback>,
        target_thread: usize,
    ) -> c_int;
}

// ── Internal Types ──────────────────────────────────────────────────────────

/// Events received from Emscripten WebSocket callbacks.
enum IncomingEvent {
    Open,
    Message(String),
    Error(String),
    Close {
        #[allow(dead_code)]
        code: u16,
        #[allow(dead_code)]
        was_clean: bool,
    },
}

/// State shared with C callbacks via raw pointer (`Box::into_raw`).
struct CallbackState {
    tx: std_mpsc::Sender<IncomingEvent>,
}

// ── Transport Struct ────────────────────────────────────────────────────────

/// A [`Transport`] implementation backed by Emscripten's built-in WebSocket API.
///
/// Uses raw FFI calls to `<emscripten/websocket.h>` and a `std::sync::mpsc`
/// channel to bridge asynchronous C callbacks into the transport's `recv()`
/// method.
///
/// # Construction
///
/// Use [`EmscriptenWebSocketTransport::connect`] to create a new connection:
///
/// ```rust,ignore
/// use signal_fish_client::EmscriptenWebSocketTransport;
///
/// let transport = EmscriptenWebSocketTransport::connect("wss://server/ws")?;
/// ```
///
/// # Threading
///
/// This type is only intended for use on `wasm32-unknown-emscripten`, which
/// is single-threaded. The `Send` implementation is safe because there are
/// no other threads.
pub struct EmscriptenWebSocketTransport {
    socket: EMSCRIPTEN_WEBSOCKET_T,
    incoming_rx: std_mpsc::Receiver<IncomingEvent>,
    /// Raw pointer to the `CallbackState`. Owned by this struct; reclaimed in `Drop`.
    callback_state: *mut CallbackState,
    closed: bool,
    /// Tracks whether `emscripten_websocket_delete` has been called, so `Drop`
    /// does not double-delete the socket handle.
    deleted: bool,
}

// SAFETY: On wasm32-unknown-emscripten, everything is single-threaded.
// The Send bound is required by the Transport trait but is vacuously
// satisfied since there are no other threads.
unsafe impl Send for EmscriptenWebSocketTransport {}

// ── Constructor ─────────────────────────────────────────────────────────────

impl EmscriptenWebSocketTransport {
    /// Create a new WebSocket connection to the given URL.
    ///
    /// This function is synchronous — the WebSocket is created immediately,
    /// but the connection handshake completes asynchronously. Messages sent
    /// before the connection opens are buffered by the browser.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::Io`] if the URL contains interior NUL bytes
    /// or if `emscripten_websocket_new` fails.
    pub fn connect(url: &str) -> Result<Self, SignalFishError> {
        let c_url = std::ffi::CString::new(url).map_err(|e| {
            SignalFishError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, e))
        })?;

        let (tx, rx) = std_mpsc::channel();
        let state = Box::new(CallbackState { tx });
        let state_ptr = Box::into_raw(state);

        let attrs = EmscriptenWebSocketCreateAttributes {
            url: c_url.as_ptr(),
            protocols: std::ptr::null(),
            create_on_main_thread: 1,
        };

        // SAFETY: `attrs` contains a valid NUL-terminated URL pointer and a null
        // protocols pointer. The Emscripten runtime owns the returned socket handle.
        let socket = unsafe { emscripten_websocket_new(&attrs) };
        if socket <= 0 {
            // Reclaim the leaked state before returning.
            // SAFETY: `state_ptr` was created by `Box::into_raw` above and has not
            // been aliased — the socket creation failed before any callbacks were registered.
            unsafe { drop(Box::from_raw(state_ptr)) };
            return Err(SignalFishError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                format!("emscripten_websocket_new failed with code {socket}"),
            )));
        }

        // Register callbacks (all fire on the calling thread = main thread).
        //
        // SAFETY — Cleanup on partial registration failure:
        // All four registration calls are eagerly evaluated into the `results`
        // array before any result is checked. When the loop detects a failure,
        // earlier callbacks may already be registered and hold `user_data`.
        // The cleanup sequence (close → delete → drop) is safe because:
        //
        // 1. `emscripten_websocket_close` may synchronously fire the close callback
        //    on the main thread. At this point, `state_ptr` is still valid — we have
        //    not yet freed it. The callback accesses `state_ptr` as a shared reference
        //    (`&*`) and returns before `close` returns.
        //
        // 2. `emscripten_websocket_delete` unregisters ALL callbacks from the socket
        //    handle. After this call returns, no further callback invocations can occur,
        //    so no code path will dereference `state_ptr` again.
        //
        // 3. Only then do we reclaim `state_ptr` via `Box::from_raw`, which is now
        //    the sole owner with no outstanding references.
        //
        // This relies on the single-threaded execution model of wasm32-unknown-emscripten:
        // all callback invocations are synchronous on the main thread, so there is no
        // window for a concurrent callback to race between steps 2 and 3.
        let user_data = state_ptr.cast::<c_void>();
        // SAFETY: See the detailed safety argument above regarding cleanup order.
        // `state_ptr` is valid, `socket` is a live handle, and all callback
        // function pointers match the Emscripten-expected signatures.
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
                (
                    "onclose",
                    emscripten_websocket_set_onclose_callback_on_thread(
                        socket,
                        user_data,
                        Some(on_close_callback),
                        0,
                    ),
                ),
            ];

            for (name, result) in results {
                if result != EMSCRIPTEN_RESULT_SUCCESS {
                    emscripten_websocket_close(socket, 1000, std::ptr::null());
                    emscripten_websocket_delete(socket);
                    drop(Box::from_raw(state_ptr));
                    return Err(SignalFishError::Io(std::io::Error::other(format!(
                        "emscripten_websocket_set_{name}_callback_on_thread failed: {result}"
                    ))));
                }
            }
        }

        Ok(Self {
            socket,
            incoming_rx: rx,
            callback_state: state_ptr,
            closed: false,
            deleted: false,
        })
    }
}

// ── C Callback Implementations ──────────────────────────────────────────────
//
// SAFETY (all callbacks): These `extern "C"` functions are registered with
// Emscripten's WebSocket API via `emscripten_websocket_set_on*_callback_on_thread`.
// The Emscripten runtime guarantees that:
// - `user_data` is the same pointer we passed during registration (a valid
//   `*mut CallbackState` created via `Box::into_raw`).
// - `event` pointers are valid for the duration of the callback invocation.
// - Callbacks are invoked synchronously on the main thread (single-threaded
//   wasm32-unknown-emscripten execution model), so no data races are possible.
// The `CallbackState` remains live until `emscripten_websocket_delete` unregisters
// all callbacks, after which we reclaim it via `Box::from_raw` in `Drop`.

// SAFETY: See the callback SAFETY block comment above for pointer guarantees.
extern "C" fn on_open_callback(
    _event_type: c_int,
    _event: *const EmscriptenWebSocketOpenEvent,
    user_data: *mut c_void,
) -> EM_BOOL {
    let state = unsafe { &*(user_data as *const CallbackState) };
    let _ = state.tx.send(IncomingEvent::Open);
    1 // EM_TRUE
}

// SAFETY: See the callback SAFETY block comment above for pointer guarantees.
extern "C" fn on_message_callback(
    _event_type: c_int,
    event: *const EmscriptenWebSocketMessageEvent,
    user_data: *mut c_void,
) -> EM_BOOL {
    let state = unsafe { &*(user_data as *const CallbackState) };
    let event = unsafe { &*event };

    if event.is_text != 0 {
        // Text message — create String from UTF-8 bytes.
        // num_bytes includes the NUL terminator for text messages.
        let len = if event.num_bytes > 0 {
            (event.num_bytes - 1) as usize
        } else {
            0
        };
        // SAFETY: Emscripten guarantees `event.data` points to `event.num_bytes`
        // valid bytes for the duration of this callback. `len` excludes the NUL
        // terminator so the slice covers only the payload bytes.
        let bytes = unsafe { std::slice::from_raw_parts(event.data, len) };
        match std::str::from_utf8(bytes) {
            Ok(s) => {
                let _ = state.tx.send(IncomingEvent::Message(s.to_owned()));
            }
            Err(e) => {
                tracing::warn!("received non-UTF-8 text message: {e}");
            }
        }
    } else {
        // Binary message — the signal-fish protocol uses text/JSON only,
        // so skip binary frames (matches WebSocketTransport behavior).
        tracing::debug!(
            "skipping binary WebSocket message ({} bytes)",
            event.num_bytes
        );
    }
    1 // EM_TRUE
}

// SAFETY: See the callback SAFETY block comment above for pointer guarantees.
extern "C" fn on_error_callback(
    _event_type: c_int,
    _event: *const EmscriptenWebSocketErrorEvent,
    user_data: *mut c_void,
) -> EM_BOOL {
    let state = unsafe { &*(user_data as *const CallbackState) };
    let _ = state
        .tx
        .send(IncomingEvent::Error("WebSocket error".into()));
    1 // EM_TRUE
}

// SAFETY: See the callback SAFETY block comment above for pointer guarantees.
extern "C" fn on_close_callback(
    _event_type: c_int,
    event: *const EmscriptenWebSocketCloseEvent,
    user_data: *mut c_void,
) -> EM_BOOL {
    let state = unsafe { &*(user_data as *const CallbackState) };
    let event = unsafe { &*event };
    let _ = state.tx.send(IncomingEvent::Close {
        code: event.code,
        was_clean: event.was_clean != 0,
    });
    1 // EM_TRUE
}

// ── Debug-Only Misuse Detection ─────────────────────────────────────────────

/// A future that yields `Poll::Pending` exactly once, like `std::future::pending()`,
/// but in debug builds detects misuse with a real async runtime by checking
/// whether the provided waker is a noop waker.
///
/// If a real waker is detected (one that is not the noop waker), this emits a
/// `tracing::error!` diagnostic to help users identify that they are incorrectly
/// using `EmscriptenWebSocketTransport` with `SignalFishClient::start()` or
/// another real async executor instead of `SignalFishPollingClient`.
struct NoopWakerPending;

impl std::future::Future for NoopWakerPending {
    type Output = std::convert::Infallible;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        #[cfg(debug_assertions)]
        {
            // Detect non-noop wakers via `Waker::will_wake`.
            // `Waker::noop()` returns a waker whose `wake()` is a no-op.
            // `will_wake` compares both the data pointer and vtable of
            // the two wakers — a real runtime's waker will not match.
            let noop = std::task::Waker::noop();
            if !_cx.waker().will_wake(noop) {
                tracing::error!(
                    "EmscriptenWebSocketTransport::recv() is being polled with a real async \
                     runtime waker. This transport is designed exclusively for use with \
                     SignalFishPollingClient (noop-waker polling). Using it with \
                     SignalFishClient::start() or any real async executor (Tokio, async-std, etc.) \
                     will cause recv() to hang indefinitely. \
                     See: https://docs.rs/signal-fish-client/latest/signal_fish_client/struct.SignalFishPollingClient.html"
                );
            }
        }
        std::task::Poll::Pending
    }
}

// ── Transport Trait Implementation ──────────────────────────────────────────

#[async_trait]
impl Transport for EmscriptenWebSocketTransport {
    async fn send(&mut self, message: String) -> Result<(), SignalFishError> {
        if self.closed {
            return Err(SignalFishError::TransportClosed);
        }
        let c_msg = std::ffi::CString::new(message)
            .map_err(|e| SignalFishError::TransportSend(e.to_string()))?;
        // SAFETY: `c_msg` is a valid NUL-terminated CString; `self.socket` is a
        // valid handle obtained from `emscripten_websocket_new`.
        let result = unsafe { emscripten_websocket_send_utf8_text(self.socket, c_msg.as_ptr()) };
        if result != EMSCRIPTEN_RESULT_SUCCESS {
            return Err(SignalFishError::TransportSend(format!(
                "emscripten_websocket_send_utf8_text failed: {result}"
            )));
        }
        Ok(())
    }

    async fn recv(&mut self) -> Option<Result<String, SignalFishError>> {
        if self.closed {
            return None;
        }
        loop {
            match self.incoming_rx.try_recv() {
                Ok(IncomingEvent::Message(text)) => return Some(Ok(text)),
                Ok(IncomingEvent::Open) => {
                    tracing::info!(
                        "WebSocket connection opened (onopen callback received). \
                         SignalFishPollingClient emits Connected on first poll(), \
                         which may precede this event."
                    );
                    continue;
                }
                Ok(IncomingEvent::Error(e)) => {
                    self.closed = true;
                    return Some(Err(SignalFishError::TransportReceive(e)));
                }
                Ok(IncomingEvent::Close { .. }) => {
                    self.closed = true;
                    return None;
                }
                Err(std_mpsc::TryRecvError::Empty) => {
                    // No messages buffered — yield `Poll::Pending` via
                    // `NoopWakerPending`, which never registers a waker
                    // and therefore never wakes. In debug builds, it
                    // logs an error if a real (non-noop) waker is detected.
                    // See the module-level "recv() caller contract" section
                    // for the full rationale.
                    match NoopWakerPending.await {}
                }
                Err(std_mpsc::TryRecvError::Disconnected) => {
                    self.closed = true;
                    return None;
                }
            }
        }
    }

    async fn close(&mut self) -> Result<(), SignalFishError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        // SAFETY: `self.socket` is a valid handle; `self.closed` prevents double-close.
        // `emscripten_websocket_close` initiates the WebSocket close handshake. On the
        // single-threaded Emscripten model, the onclose callback may fire synchronously
        // during this call — `callback_state` is still valid at this point since we do
        // not free it until `Drop`.
        let result = unsafe { emscripten_websocket_close(self.socket, 1000, std::ptr::null()) };
        if result != EMSCRIPTEN_RESULT_SUCCESS {
            tracing::warn!("emscripten_websocket_close returned {result}");
        }
        // SAFETY: `emscripten_websocket_delete` unregisters ALL callbacks from the
        // socket handle. After this call returns, no further callback invocations can
        // occur, so no code path will dereference `callback_state` again. Without this
        // call, callbacks could still fire between `close()` returning and `Drop`
        // running if a JavaScript event loop tick occurs in that window.
        // `callback_state` is NOT freed here — it is reclaimed in `Drop` via
        // `Box::from_raw`. This separation ensures the pointer remains valid for any
        // synchronous callbacks that fire during the `close` call above.
        unsafe {
            let delete_result = emscripten_websocket_delete(self.socket);
            if delete_result != EMSCRIPTEN_RESULT_SUCCESS {
                tracing::warn!("emscripten_websocket_delete returned {delete_result}");
            }
        }
        self.deleted = true;
        Ok(())
    }
}

// ── Drop Implementation ─────────────────────────────────────────────────────

impl Drop for EmscriptenWebSocketTransport {
    fn drop(&mut self) {
        // Two code paths depending on whether `close()` was previously called:
        //
        // If `close()` was called, both `emscripten_websocket_close` and
        // `emscripten_websocket_delete` have already run — all callbacks are
        // unregistered. We skip straight to reclaiming `callback_state`.
        //
        // If `close()` was NOT called (e.g., the transport is dropped without
        // explicit shutdown), we run the full close/delete/reclaim sequence.
        if !self.closed {
            // SAFETY: `self.socket` is a valid handle. `emscripten_websocket_close`
            // initiates closure — if the onclose callback fires synchronously,
            // `callback_state` is still valid since we have not freed it yet.
            unsafe {
                emscripten_websocket_close(self.socket, 1000, std::ptr::null());
            }
        }
        if !self.deleted {
            // SAFETY: `emscripten_websocket_delete` unregisters all callbacks,
            // preventing any further access to `callback_state` from the
            // Emscripten event loop.
            unsafe {
                let result = emscripten_websocket_delete(self.socket);
                if result != EMSCRIPTEN_RESULT_SUCCESS {
                    tracing::warn!("emscripten_websocket_delete returned {result}");
                }
            }
        }
        // SAFETY: `callback_state` was created by `Box::into_raw` in `connect()`.
        // All callbacks have been unregistered (either in `close()` or above),
        // so no code path can dereference this pointer after this point.
        unsafe {
            drop(Box::from_raw(self.callback_state));
        }
    }
}
