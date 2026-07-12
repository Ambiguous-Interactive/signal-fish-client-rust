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
//! transport's `poll_recv()` method.
//!
//! # Compatibility
//!
//! This transport is designed exclusively for use with
//! [`SignalFishPollingClient`](crate::SignalFishPollingClient) and a noop-waker
//! polling model. It is **not** compatible with
//! [`SignalFishClient::start()`](crate::SignalFishClient::start) or any real
//! async runtime, because `poll_recv()` does not register a waker when no
//! messages are buffered.
//!
//! # `poll_recv()` caller contract
//!
//! The `poll_recv()` method returns [`std::task::Poll::Pending`] when the
//! internal channel is empty without registering the supplied waker. This is
//! intentional and correct under the noop-waker polling model used by
//! [`SignalFishPollingClient`](crate::SignalFishPollingClient):
//!
//! - **`Poll::Pending` intentionally does not register a waker.** When
//!   no messages are buffered, the transport signals "nothing yet" by
//!   returning `Poll::Pending`. The polling
//!   client discards this result and retries on the next tick, which
//!   can observe newly arrived messages.
//!
//! - **Only compatible with `SignalFishPollingClient`.** Standard async
//!   runtimes (Tokio, async-std, etc.) expect futures to register a
//!   waker so they can be re-polled when progress is possible. Since
//!   this transport does not register a waker, using it with
//!   [`SignalFishClient::start()`](crate::SignalFishClient::start) or
//!   any real executor will cause `poll_recv()` to remain pending indefinitely.
//!
//! - **Debug-build misuse detection.** In `cfg(debug_assertions)` builds,
//!   `poll_recv()` emits a `tracing::error!` once if it detects a non-noop waker,
//!   which indicates the transport is being driven by a real async runtime
//!   instead of `SignalFishPollingClient`. This makes misuse visible
//!   during development rather than manifesting as a silent hang.
//!
//! # Example
//!
//! ```rust,ignore
//! use signal_fish_client::{
//!     EmscriptenWebSocketTransport, SignalFishConfig, SignalFishPollingClient,
//! };
//!
//! let transport = EmscriptenWebSocketTransport::connect("wss://server/ws")?;
//! let config = SignalFishConfig::new("mb_app_abc123");
//! let mut client = SignalFishPollingClient::new(transport, config);
//! let events = client.poll();
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

use crate::error::SignalFishError;
use crate::transport::{Transport, TransportCloseInfo, TransportFrame};

// ── FFI Bindings ────────────────────────────────────────────────────────────

// These type aliases mirror Emscripten's C naming conventions exactly.
#[allow(non_camel_case_types)]
type EMSCRIPTEN_WEBSOCKET_T = c_int;

#[allow(non_camel_case_types)]
type EM_BOOL = c_int;

// Verified against Emscripten 3.1.74 system/include/emscripten/websocket.h:
// WebSocket event structs and creation attributes use C `bool` fields. On
// wasm32-unknown-emscripten that ABI type is one byte. Callback return values
// remain `EM_BOOL` (`c_int`) in this binding and must not use this field alias.
#[allow(non_camel_case_types)]
type C_BOOL = u8;

/// Emscripten result code indicating success.
const EMSCRIPTEN_RESULT_SUCCESS: c_int = 0;

#[repr(C)]
struct EmscriptenWebSocketCreateAttributes {
    url: *const c_char,
    protocols: *const c_char,
    create_on_main_thread: C_BOOL,
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
    is_text: C_BOOL,
}

#[repr(C)]
struct EmscriptenWebSocketErrorEvent {
    socket: EMSCRIPTEN_WEBSOCKET_T,
}

#[repr(C)]
struct EmscriptenWebSocketCloseEvent {
    socket: EMSCRIPTEN_WEBSOCKET_T,
    was_clean: C_BOOL,
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

    fn emscripten_websocket_send_binary(
        socket: EMSCRIPTEN_WEBSOCKET_T,
        data: *const c_void,
        length: u32,
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
    Message(TransportFrame),
    Error(String),
    Close {
        code: u16,
        was_clean: bool,
        reason: Option<String>,
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
/// channel to bridge asynchronous C callbacks into the transport's `poll_recv()`
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
/// is single-threaded. It deliberately does not implement `Send` and must be
/// driven by [`SignalFishPollingClient`](crate::SignalFishPollingClient).
#[deprecated(
    since = "0.8.0",
    note = "for standard Godot exports use GodotWebSocketTransport; this transport requires a custom Emscripten host that links the WebSocket library"
)]
pub struct EmscriptenWebSocketTransport {
    socket: EMSCRIPTEN_WEBSOCKET_T,
    incoming_rx: std_mpsc::Receiver<IncomingEvent>,
    /// Raw pointer to the `CallbackState`. Owned by this struct; reclaimed in `Drop`.
    callback_state: *mut CallbackState,
    closed: bool,
    /// Whether the browser's `onopen` callback has fired.
    opened: bool,
    /// Tracks whether `emscripten_websocket_delete` has been called, so `Drop`
    /// does not double-delete the socket handle.
    deleted: bool,
    close_info: Option<TransportCloseInfo>,
    #[cfg(debug_assertions)]
    reported_non_noop_waker: bool,
    /// Explicit `!Send` marker. The raw `callback_state` pointer already prevents
    /// auto-`Send`, but this field documents the intent and prevents it from being
    /// accidentally removed if the implementation ever changes.
    _not_send: std::marker::PhantomData<*const ()>,
}

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
            opened: false,
            deleted: false,
            close_info: None,
            #[cfg(debug_assertions)]
            reported_non_noop_waker: false,
            _not_send: std::marker::PhantomData,
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

/// Copy callback-owned payload bytes without constructing a slice from a null
/// pointer for an empty WebSocket frame.
///
/// # Safety
///
/// When `len` is non-zero, `data` must point to at least `len` initialized bytes
/// that remain valid for the duration of this call.
unsafe fn copy_event_payload(data: *const u8, len: usize) -> Vec<u8> {
    if len == 0 {
        return Vec::new();
    }

    // SAFETY: The caller guarantees a non-null pointer to `len` initialized
    // bytes. The zero-length case returned above before constructing a slice.
    unsafe { std::slice::from_raw_parts(data, len).to_vec() }
}

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
        // SAFETY: For a non-empty payload, Emscripten guarantees `event.data`
        // points to `event.num_bytes` valid bytes for this callback. `len`
        // excludes the NUL terminator. Empty payloads do not dereference data.
        let bytes = unsafe { copy_event_payload(event.data, len) };
        match std::str::from_utf8(&bytes) {
            Ok(s) => {
                let _ = state
                    .tx
                    .send(IncomingEvent::Message(TransportFrame::Text(s.to_owned())));
            }
            Err(e) => {
                tracing::warn!("received non-UTF-8 text message: {e}");
            }
        }
    } else {
        let len = event.num_bytes as usize;
        // SAFETY: For a non-empty payload, Emscripten guarantees `event.data`
        // points to `event.num_bytes` valid bytes for this callback. Empty
        // binary frames may carry a null data pointer and are copied directly
        // to an empty Vec without constructing a slice.
        let bytes = unsafe { copy_event_payload(event.data, len) };
        let _ = state
            .tx
            .send(IncomingEvent::Message(TransportFrame::Binary(bytes)));
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
    let reason_len = event
        .reason
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(event.reason.len());
    let reason = event
        .reason
        .get(..reason_len)
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .filter(|reason| !reason.is_empty())
        .map(str::to_owned);
    let _ = state.tx.send(IncomingEvent::Close {
        code: event.code,
        was_clean: event.was_clean != 0,
        reason,
    });
    1 // EM_TRUE
}

// ── Transport Trait Implementation ──────────────────────────────────────────

impl Transport for EmscriptenWebSocketTransport {
    fn poll_send(
        &mut self,
        _cx: &mut std::task::Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        if self.closed {
            return std::task::Poll::Ready(Err(SignalFishError::TransportClosed));
        }
        let Some(frame) = frame.take() else {
            return std::task::Poll::Ready(Ok(()));
        };
        let result = match frame {
            TransportFrame::Text(message) => {
                let c_msg = match std::ffi::CString::new(message) {
                    Ok(message) => message,
                    Err(error) => {
                        return std::task::Poll::Ready(Err(SignalFishError::TransportSend(
                            error.to_string(),
                        )));
                    }
                };
                // SAFETY: `self.socket` is a live Emscripten WebSocket handle and
                // `c_msg` remains allocated and NUL-terminated for the duration
                // of this synchronous FFI call.
                unsafe { emscripten_websocket_send_utf8_text(self.socket, c_msg.as_ptr()) }
            }
            TransportFrame::Binary(bytes) => {
                let Ok(length) = u32::try_from(bytes.len()) else {
                    return std::task::Poll::Ready(Err(SignalFishError::TransportSend(
                        "binary frame exceeds Emscripten u32 length".into(),
                    )));
                };
                // SAFETY: `self.socket` is live, `bytes` remains allocated for
                // this synchronous call, and `length` was checked to fit `u32`.
                unsafe {
                    emscripten_websocket_send_binary(
                        self.socket,
                        bytes.as_ptr().cast::<c_void>(),
                        length,
                    )
                }
            }
        };
        if result == EMSCRIPTEN_RESULT_SUCCESS {
            std::task::Poll::Ready(Ok(()))
        } else {
            std::task::Poll::Ready(Err(SignalFishError::TransportSend(format!(
                "Emscripten WebSocket send failed: {result}"
            ))))
        }
    }

    fn poll_recv(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<TransportFrame, SignalFishError>>> {
        // Keep release builds warning-free when the debug-only diagnostic below
        // is compiled out.
        let _ = cx;
        #[cfg(debug_assertions)]
        if !self.reported_non_noop_waker && !cx.waker().will_wake(std::task::Waker::noop()) {
            self.reported_non_noop_waker = true;
            tracing::error!(
                "EmscriptenWebSocketTransport must be driven by \
                 SignalFishPollingClient with a noop waker; a wake-driven \
                 executor can remain pending indefinitely"
            );
        }
        if self.closed {
            return std::task::Poll::Ready(None);
        }
        loop {
            match self.incoming_rx.try_recv() {
                Ok(IncomingEvent::Message(frame)) => {
                    return std::task::Poll::Ready(Some(Ok(frame)));
                }
                Ok(IncomingEvent::Open) => {
                    self.opened = true;
                }
                Ok(IncomingEvent::Error(error)) => {
                    self.closed = true;
                    return std::task::Poll::Ready(Some(Err(SignalFishError::TransportReceive(
                        error,
                    ))));
                }
                Ok(IncomingEvent::Close {
                    code,
                    was_clean,
                    reason,
                }) => {
                    self.closed = true;
                    self.close_info = Some(TransportCloseInfo {
                        code: Some(code),
                        reason,
                        clean: Some(was_clean),
                        initiated_by_peer: true,
                    });
                    return std::task::Poll::Ready(None);
                }
                Err(std_mpsc::TryRecvError::Empty) => return std::task::Poll::Pending,
                Err(std_mpsc::TryRecvError::Disconnected) => {
                    self.closed = true;
                    return std::task::Poll::Ready(None);
                }
            }
        }
    }

    fn poll_close(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        if self.closed {
            return std::task::Poll::Ready(Ok(()));
        }
        self.closed = true;
        // SAFETY: `self.socket` is a live Emscripten WebSocket handle, and a null
        // reason pointer is permitted when initiating a normal close.
        let result = unsafe { emscripten_websocket_close(self.socket, 1000, std::ptr::null()) };
        if result != EMSCRIPTEN_RESULT_SUCCESS {
            tracing::warn!("emscripten_websocket_close returned {result}");
        }
        // SAFETY: This is the sole deletion path for the live handle; `closed`
        // prevents subsequent transport operations and `deleted` is set below.
        unsafe {
            let delete_result = emscripten_websocket_delete(self.socket);
            if delete_result != EMSCRIPTEN_RESULT_SUCCESS {
                tracing::warn!("emscripten_websocket_delete returned {delete_result}");
            }
        }
        self.deleted = true;
        std::task::Poll::Ready(Ok(()))
    }

    fn is_ready(&self) -> bool {
        self.opened
    }

    fn close_info(&self) -> Option<TransportCloseInfo> {
        self.close_info.clone()
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
