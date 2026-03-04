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

        let socket = unsafe { emscripten_websocket_new(&attrs) };
        if socket <= 0 {
            // Reclaim the leaked state before returning.
            unsafe { drop(Box::from_raw(state_ptr)) };
            return Err(SignalFishError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                format!("emscripten_websocket_new failed with code {socket}"),
            )));
        }

        // Register callbacks (all fire on the calling thread = main thread).
        let user_data = state_ptr.cast::<c_void>();
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
        })
    }
}

// ── C Callback Implementations ──────────────────────────────────────────────

extern "C" fn on_open_callback(
    _event_type: c_int,
    _event: *const EmscriptenWebSocketOpenEvent,
    user_data: *mut c_void,
) -> EM_BOOL {
    let state = unsafe { &*(user_data as *const CallbackState) };
    let _ = state.tx.send(IncomingEvent::Open);
    1 // EM_TRUE
}

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

// ── Transport Trait Implementation ──────────────────────────────────────────

#[async_trait]
impl Transport for EmscriptenWebSocketTransport {
    async fn send(&mut self, message: String) -> Result<(), SignalFishError> {
        if self.closed {
            return Err(SignalFishError::TransportClosed);
        }
        let c_msg = std::ffi::CString::new(message)
            .map_err(|e| SignalFishError::TransportSend(e.to_string()))?;
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
                    tracing::debug!("WebSocket connection opened");
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
                    // No messages buffered — return Pending.
                    // The polling client will call recv() again next frame.
                    std::future::pending().await
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
        let result = unsafe { emscripten_websocket_close(self.socket, 1000, std::ptr::null()) };
        if result != EMSCRIPTEN_RESULT_SUCCESS {
            tracing::warn!("emscripten_websocket_close returned {result}");
        }
        Ok(())
    }
}

// ── Drop Implementation ─────────────────────────────────────────────────────

impl Drop for EmscriptenWebSocketTransport {
    fn drop(&mut self) {
        // SAFETY: On wasm32-unknown-emscripten, the execution model is single-threaded.
        // The close/delete/reclaim sequence is safe because:
        // 1. `emscripten_websocket_close` initiates closure — if the close callback fires
        //    synchronously within this call, `callback_state` is still valid.
        // 2. `emscripten_websocket_delete` unregisters all callbacks, preventing any
        //    further access to `callback_state` from the Emscripten event loop.
        // 3. Only then do we reclaim the `CallbackState` via `Box::from_raw`.
        if !self.closed {
            unsafe {
                emscripten_websocket_close(self.socket, 1000, std::ptr::null());
            }
        }
        unsafe {
            let result = emscripten_websocket_delete(self.socket);
            if result != EMSCRIPTEN_RESULT_SUCCESS {
                tracing::warn!("emscripten_websocket_delete returned {result}");
            }
            drop(Box::from_raw(self.callback_state));
        }
    }
}
