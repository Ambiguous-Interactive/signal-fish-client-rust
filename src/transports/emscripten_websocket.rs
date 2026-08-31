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
//! The message callback admits input against a shared inbound-byte ledger
//! (default 8 MiB, configurable through
//! [`EmscriptenWebSocketConnectOptions::max_inbound_queue_bytes`]) before it
//! copies any payload, so a server cannot grow this transport's heap without
//! limit between polling ticks; zero-length frames carry a conservative
//! minimum charge so count-based floods hit the same bound. The first
//! over-limit frame or backlog, or a text frame that is not valid UTF-8,
//! fuses the transport with one terminal receive error; later flood frames
//! drop silently. Drained frames release their charge back to the ledger.
//!
//! Sends consult the browser's live `WebSocket.readyState` before each FFI
//! call: the close/error callbacks only reach the transport when `poll_recv`
//! drains them, and the JS send shim reports success unconditionally even for
//! sockets the browser already closed, so a dying socket fails its queued
//! send terminally instead of silently discarding it.
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
//! let transport = EmscriptenWebSocketTransport::connect("wss://server/v2/ws")?;
//! let config = SignalFishConfig::new("mb_app_abc123");
//! let mut client = SignalFishPollingClient::new(transport, config);
//! let events = client.poll();
//! ```

// This target-only module is the core crate's sole unsafe-code exception. Its
// raw calls, pointer lifetimes, callbacks, and cleanup paths are audited by
// scripts/check-ffi-safety.sh and the Emscripten target CI lane.
#![allow(unsafe_code)]
#![allow(deprecated)]

// Compile-time guard: this module requires the wasm32-unknown-emscripten target.
// The FFI functions (emscripten_websocket_new, etc.) are only available in
// Emscripten's C runtime. Compiling on any other target will produce linker errors.
#[cfg(not(target_os = "emscripten"))]
compile_error!(
    "The `transport-websocket-emscripten` feature requires the `wasm32-unknown-emscripten` target. \
     This module uses Emscripten's C WebSocket API which is unavailable on other targets. \
     Remove this feature or compile with `--target wasm32-unknown-emscripten`."
);

use std::cell::Cell;
use std::ffi::{c_char, c_int, c_void};
use std::rc::Rc;
use std::sync::mpsc as std_mpsc;

use super::emscripten_cleanup::{CleanupState, DeleteAuthorization, ReclaimAuthorization};
use super::emscripten_inbound_queue::{
    charged_frame_bytes, InboundQueueBound, QueueRefusal, DEFAULT_MAX_INBOUND_QUEUE_BYTES,
};
use crate::error::SignalFishError;
use crate::transport::{
    poll_accept_frame, text_frame_from_utf8, Transport, TransportCloseInfo, TransportFrame,
};

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

/// `WebSocket.readyState` value for an established connection. The C API
/// returns the browser's `readyState` through the `unsigned short *`
/// out-parameter of `emscripten_websocket_get_ready_state`; the browser
/// reports `CLOSING`(2)/`CLOSED`(3) once the connection is dying or dead.
const EM_READY_STATE_OPEN: u16 = 1;

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

    fn emscripten_websocket_get_ready_state(
        socket: EMSCRIPTEN_WEBSOCKET_T,
        ready_state: *mut u16,
    ) -> c_int;

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
    /// Shared admission ledger for buffered inbound frames. The owning
    /// transport keeps an [`Rc`] clone so drained frames release capacity
    /// without dereferencing this raw callback allocation.
    inbound_queue: Rc<Cell<InboundQueueBound>>,
}

/// Owns callback state without freeing it in `Drop`. Reclamation requires the
/// one-shot authorization produced by [`CleanupState`] after close was
/// attempted and socket deletion succeeded. Without that proof, dropping this
/// wrapper intentionally leaks the allocation so browser callbacks stay safe.
struct RegisteredCallbackState(Option<std::ptr::NonNull<CallbackState>>);

impl RegisteredCallbackState {
    fn new(
        tx: std_mpsc::Sender<IncomingEvent>,
        inbound_queue: Rc<Cell<InboundQueueBound>>,
    ) -> Self {
        let state = Box::new(CallbackState { tx, inbound_queue });
        Self(std::ptr::NonNull::new(Box::into_raw(state)))
    }

    fn as_ptr(&self) -> *mut CallbackState {
        self.0
            .map_or(std::ptr::null_mut(), std::ptr::NonNull::as_ptr)
    }

    const fn is_live(&self) -> bool {
        self.0.is_some()
    }

    fn reclaim(&mut self, _authorization: ReclaimAuthorization) {
        let Some(state_ptr) = self.0.take() else {
            return;
        };
        // SAFETY: The non-forgeable authorization proves successful callback
        // unregistration, and `take` makes this the allocation's sole reclaim.
        unsafe { drop(Box::from_raw(state_ptr.as_ptr())) };
    }
}

// ── Connect Options ─────────────────────────────────────────────────────────

/// Options controlling how an [`EmscriptenWebSocketTransport`] connection is
/// established.
///
/// Construct with [`new`](Self::new) (or [`Default`]) and adjust with the
/// `with_*` builders:
///
/// ```rust,ignore
/// use signal_fish_client::EmscriptenWebSocketConnectOptions;
///
/// // Raise the buffered-inbound bound for snapshot-heavy deployments.
/// let options =
///     EmscriptenWebSocketConnectOptions::new().with_max_inbound_queue_bytes(Some(16 << 20));
/// let transport = EmscriptenWebSocketTransport::connect_with_options(
///     "wss://server/v2/ws",
///     options,
/// )?;
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[deprecated(
    since = "0.8.0",
    note = "for standard Godot exports use GodotWebSocketTransport; this transport requires a custom Emscripten host that links the WebSocket library"
)]
pub struct EmscriptenWebSocketConnectOptions {
    /// Maximum inbound WebSocket input buffered between polling ticks, in
    /// bytes, inclusive.
    ///
    /// Callback-delivered frames queue here while the game loop is busy, so
    /// this bounds both any single frame and the total bytes waiting for
    /// `poll_recv`; zero-length frames reserve a small minimum charge so a
    /// count-based flood cannot bypass the bound. Exceeding it refuses the
    /// offending input and fuses the transport with one terminal receive
    /// error — mirroring how the native WebSocket transport surfaces
    /// over-limit inbound messages instead of growing memory without limit
    /// between drains.
    ///
    /// Defaults to 8 MiB, matching the native default. This is a protective
    /// client resource policy for memory-constrained browser tabs, not a
    /// protocol maximum: deployments with larger room snapshots must raise
    /// it. Every admitted frame reserves a 64-byte minimum charge
    /// approximating its queuing overhead, so bounds below that minimum
    /// cannot admit even an empty frame and fuse on first input — realistic
    /// limits are orders of magnitude larger. Set it to `None` to restore
    /// the previous unbounded buffering. `Some(0)` is invalid and makes
    /// connection setup fail before network I/O.
    pub max_inbound_queue_bytes: Option<usize>,
}

impl Default for EmscriptenWebSocketConnectOptions {
    fn default() -> Self {
        Self {
            max_inbound_queue_bytes: Some(DEFAULT_MAX_INBOUND_QUEUE_BYTES),
        }
    }
}

impl EmscriptenWebSocketConnectOptions {
    /// Create options with the default 8 MiB buffered-inbound bound.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_inbound_queue_bytes: Some(DEFAULT_MAX_INBOUND_QUEUE_BYTES),
        }
    }

    /// Set the inclusive bound on buffered inbound WebSocket input; `None`
    /// disables it.
    #[must_use]
    pub const fn with_max_inbound_queue_bytes(mut self, max_bytes: Option<usize>) -> Self {
        self.max_inbound_queue_bytes = max_bytes;
        self
    }
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
/// let transport = EmscriptenWebSocketTransport::connect("wss://server/v2/ws")?;
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
    /// Shared with the registered callback state; admits callback-delivered
    /// input and releases capacity as `poll_recv` drains frames.
    inbound_queue: Rc<Cell<InboundQueueBound>>,
    /// Registered callback state while the browser may still dereference it.
    /// Successful socket deletion yields the authorization that reclaims it;
    /// a terminal deletion failure deliberately leaks it rather than exposing
    /// callbacks to freed memory.
    callback_state: RegisteredCallbackState,
    closed: bool,
    /// Whether the browser's `onopen` callback has fired.
    opened: bool,
    /// Target-independent close/delete ownership state. Logical receive errors
    /// leave it unchanged so cleanup still attempts close before deletion.
    cleanup: CleanupState,
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
    /// Create a new WebSocket connection to the given URL with the default
    /// 8 MiB buffered-inbound bound.
    ///
    /// This function is synchronous — the WebSocket is created immediately,
    /// but the connection handshake completes asynchronously. The transport
    /// retains caller-owned messages until the browser reports that the
    /// connection is open. Peer-close metadata (`close_info`) is best-effort:
    /// it is populated by the next `poll_recv` that drains the queued close
    /// event (or by the terminal error poll when the browser's `onclose` is
    /// already queued behind an `onerror`), so closing or dropping the
    /// transport before that poll leaves `close_info` empty.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::InvalidConfig`] if the URL contains interior
    /// NUL bytes. Returns [`SignalFishError::Io`] if
    /// `emscripten_websocket_new` fails.
    pub fn connect(url: &str) -> Result<Self, SignalFishError> {
        Self::connect_with_options(url, EmscriptenWebSocketConnectOptions::default())
    }

    /// Create a new WebSocket connection to the given URL with explicit
    /// options.
    ///
    /// This function is synchronous — the WebSocket is created immediately,
    /// but the connection handshake completes asynchronously. The transport
    /// retains caller-owned messages until the browser reports that the
    /// connection is open.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::InvalidConfig`] if the URL contains interior
    /// NUL bytes or `max_inbound_queue_bytes` is `Some(0)` — neither attempts
    /// socket creation. Returns [`SignalFishError::Io`] if
    /// `emscripten_websocket_new` fails.
    pub fn connect_with_options(
        url: &str,
        options: EmscriptenWebSocketConnectOptions,
    ) -> Result<Self, SignalFishError> {
        if options.max_inbound_queue_bytes == Some(0) {
            return Err(SignalFishError::InvalidConfig {
                field: "max_inbound_queue_bytes",
                problem: "must be greater than zero or None".into(),
            });
        }

        let c_url = std::ffi::CString::new(url).map_err(|e| SignalFishError::InvalidConfig {
            field: "url",
            problem: e.to_string(),
        })?;

        let (tx, rx) = std_mpsc::channel();
        let inbound_queue = Rc::new(Cell::new(InboundQueueBound::new(
            options.max_inbound_queue_bytes,
        )));

        let attrs = EmscriptenWebSocketCreateAttributes {
            url: c_url.as_ptr(),
            protocols: std::ptr::null(),
            create_on_main_thread: 1,
        };

        // SAFETY: `attrs` contains a valid NUL-terminated URL pointer and a null
        // protocols pointer. The Emscripten runtime owns the returned socket handle.
        let socket = unsafe { emscripten_websocket_new(&attrs) };
        if socket <= 0 {
            return Err(SignalFishError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                format!("emscripten_websocket_new failed with code {socket}"),
            )));
        }

        let mut callback_state = RegisteredCallbackState::new(tx, Rc::clone(&inbound_queue));
        let state_ptr = callback_state.as_ptr();
        let mut cleanup = CleanupState::new();

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
        // 2. A successful `emscripten_websocket_delete` unregisters ALL callbacks
        //    from the socket handle. Only that success proves no later callback
        //    can dereference `state_ptr`.
        //
        // 3. Only after confirmed deletion do we reclaim the raw allocation.
        //    A deletion failure intentionally leaks the state;
        //    retaining memory is the only sound fallback while callbacks may live.
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
                    let close_result = emscripten_websocket_close(socket, 1000, std::ptr::null());
                    cleanup.record_close_result(close_result == EMSCRIPTEN_RESULT_SUCCESS);
                    let Some(delete_authorization) = cleanup.delete_authorization() else {
                        return Err(SignalFishError::Io(std::io::Error::other(format!(
                            "emscripten_websocket_set_{name}_callback_on_thread failed: \
                             {result}; cleanup close={close_result}, deletion not authorized"
                        ))));
                    };
                    let delete_result = emscripten_websocket_delete(socket);
                    if let Some(authorization) = cleanup.record_delete_result(
                        delete_authorization,
                        delete_result == EMSCRIPTEN_RESULT_SUCCESS,
                    ) {
                        callback_state.reclaim(authorization);
                    }
                    return Err(SignalFishError::Io(std::io::Error::other(format!(
                        "emscripten_websocket_set_{name}_callback_on_thread failed: {result}; \
                         cleanup close={close_result}, delete={delete_result}"
                    ))));
                }
            }
        }

        Ok(Self {
            socket,
            incoming_rx: rx,
            inbound_queue,
            callback_state,
            closed: false,
            opened: false,
            cleanup,
            close_info: None,
            #[cfg(debug_assertions)]
            reported_non_noop_waker: false,
            _not_send: std::marker::PhantomData,
        })
    }

    /// Record structured metadata for a peer-initiated close event.
    fn record_peer_close(&mut self, code: u16, was_clean: bool, reason: Option<String>) {
        self.cleanup.record_peer_close();
        self.close_info = Some(TransportCloseInfo {
            code: Some(code),
            reason,
            clean: Some(was_clean),
            initiated_by_peer: true,
        });
    }

    /// Whether the browser socket is still open for sends at this instant.
    ///
    /// The transport-local `closed` flag only updates when `poll_recv` drains
    /// the queued close/error event, so between ticks it can lag the browser.
    /// Synchronous `emscripten_websocket_send_*` results cannot be trusted on
    /// a dying socket, so sends consult the authoritative ready state.
    fn socket_open_for_sends(&self) -> bool {
        let mut ready_state: u16 = 0;
        // SAFETY: `ready_state` is a valid `u16` out-parameter for this
        // synchronous call, and `self.socket` is a live handle while the
        // callback state is registered (`poll_send` returns early once the
        // transport is closed, before deletion).
        let result = unsafe { emscripten_websocket_get_ready_state(self.socket, &mut ready_state) };
        result == EMSCRIPTEN_RESULT_SUCCESS && ready_state == EM_READY_STATE_OPEN
    }

    /// Close the native socket once. A failed close remains retryable because
    /// deletion has not yet proved that callbacks can no longer run.
    fn close_native_socket(&mut self) -> Result<(), c_int> {
        if !self.cleanup.needs_close() || !self.callback_state.is_live() {
            return Ok(());
        }
        // SAFETY: `self.socket` remains live while `callback_state` is `Some`.
        // A synchronous close callback can safely access that still-live state.
        let result = unsafe { emscripten_websocket_close(self.socket, 1000, std::ptr::null()) };
        self.cleanup
            .record_close_result(result == EMSCRIPTEN_RESULT_SUCCESS);
        if result == EMSCRIPTEN_RESULT_SUCCESS {
            Ok(())
        } else {
            Err(result)
        }
    }

    /// Obtain proof of the preceding close attempt/observation before calling
    /// the native deletion helper.
    fn delete_after_close_attempt(&mut self) -> Result<(), c_int> {
        if !self.callback_state.is_live() {
            return Ok(());
        }
        let Some(authorization) = self.cleanup.delete_authorization() else {
            tracing::error!("callback deletion requested before native close");
            return Err(-1);
        };
        self.delete_socket_and_reclaim_callbacks(authorization)
    }

    /// Unregister every callback and reclaim its state only after Emscripten
    /// confirms deletion. On failure the pointer stays owned and live so a
    /// later cleanup path can retry without risking use-after-free.
    fn delete_socket_and_reclaim_callbacks(
        &mut self,
        delete_authorization: DeleteAuthorization,
    ) -> Result<(), c_int> {
        if !self.callback_state.is_live() {
            return Ok(());
        }
        debug_assert!(self.cleanup.needs_delete());
        // SAFETY: The registered callback allocation remains live in its
        // owning wrapper while deletion runs.
        let delete_result = unsafe { emscripten_websocket_delete(self.socket) };
        if delete_result == EMSCRIPTEN_RESULT_SUCCESS {
            let Some(authorization) = self
                .cleanup
                .record_delete_result(delete_authorization, true)
            else {
                tracing::error!(
                    "successful socket deletion lacked close/reclamation authorization; callback state remains leaked"
                );
                return Err(-1);
            };
            self.callback_state.reclaim(authorization);
            Ok(())
        } else {
            debug_assert!(self
                .cleanup
                .record_delete_result(delete_authorization, false)
                .is_none());
            Err(delete_result)
        }
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
// The `CallbackState` remains live until a successful
// `emscripten_websocket_delete` unregisters all callbacks. Cleanup then reclaims
// it exactly once; a terminal deletion failure intentionally leaks it.

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
    // SAFETY: Per the callback contract above, `user_data` is the live
    // `CallbackState` allocation for this synchronous invocation.
    let state = unsafe { &*(user_data as *const CallbackState) };
    let _ = state.tx.send(IncomingEvent::Open);
    1 // EM_TRUE
}

/// Release queue capacity for frames that left the buffer, whether drained
/// by [`Transport::poll_recv`] or rejected after admission by UTF-8
/// validation. Uses the same charge [`on_message_callback`] admitted so the
/// ledger never drifts.
fn release_queued_bytes(queue: &Cell<InboundQueueBound>, payload_len: usize) {
    let mut bound = queue.get();
    bound.record_drained(charged_frame_bytes(payload_len));
    queue.set(bound);
}

// SAFETY: See the callback SAFETY block comment above for pointer guarantees.
extern "C" fn on_message_callback(
    _event_type: c_int,
    event: *const EmscriptenWebSocketMessageEvent,
    user_data: *mut c_void,
) -> EM_BOOL {
    // SAFETY: Per the callback contract above, `user_data` is the live
    // `CallbackState` allocation for this synchronous invocation.
    let state = unsafe { &*(user_data as *const CallbackState) };
    // SAFETY: Per the callback contract above, `event` is valid for the
    // duration of this synchronous invocation and is not retained.
    let event = unsafe { &*event };

    let is_text = event.is_text != 0;
    // Text payloads include a trailing NUL terminator that is not part of
    // the message content.
    let content_len = if is_text {
        event.num_bytes.saturating_sub(1)
    } else {
        event.num_bytes
    } as usize;

    // Bound buffered inbound input before copying payload memory so an
    // oversized or flooding server cannot grow this transport's heap between
    // polling ticks. Empty frames carry the minimum charge, so a zero-length
    // flood hits the same bound as payloads. The first refusal enqueues
    // exactly one terminal error event; afterwards messages drop silently
    // because the ledger reports `AlreadyFused`.
    let mut bound = state.inbound_queue.get();
    let admission = bound.admit(charged_frame_bytes(content_len));
    state.inbound_queue.set(bound);
    if let Err(refusal) = admission {
        if !matches!(refusal, QueueRefusal::AlreadyFused) {
            let _ = state.tx.send(IncomingEvent::Error(refusal.message()));
        }
        return 1; // EM_TRUE
    }

    if is_text {
        // Text message — decode the payload bytes (which exclude the NUL
        // terminator) as UTF-8. Corrupt text frames fuse the transport
        // instead of being silently dropped: the transport contract requires
        // corruption to surface, and the native WebSocket transport fuses on
        // the engine-reported error while the Godot adapter surfaces the
        // corruption and the driver ends the stream.
        // SAFETY: For a non-empty payload, Emscripten guarantees `event.data`
        // points to `event.num_bytes` valid bytes for this callback.
        // `content_len` excludes the NUL terminator. Empty payloads do not
        // dereference data.
        let bytes = unsafe { copy_event_payload(event.data, content_len) };
        match text_frame_from_utf8(&bytes) {
            Ok(text) => {
                let _ = state
                    .tx
                    .send(IncomingEvent::Message(TransportFrame::Text(text)));
            }
            Err(error) => {
                release_queued_bytes(&state.inbound_queue, content_len);
                // Fusion is recorded in the ledger itself so frames arriving
                // before the client drains the error drop at the callback
                // instead of being admitted behind a terminal event they can
                // never reach.
                let mut bound = state.inbound_queue.get();
                bound.fuse();
                state.inbound_queue.set(bound);
                let _ = state.tx.send(IncomingEvent::Error(error.to_string()));
            }
        }
    } else {
        // SAFETY: For a non-empty payload, Emscripten guarantees `event.data`
        // points to `event.num_bytes` valid bytes for this callback. Empty
        // binary frames may carry a null data pointer and are copied directly
        // to an empty Vec without constructing a slice.
        let bytes = unsafe { copy_event_payload(event.data, content_len) };
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
    // SAFETY: Per the callback contract above, `user_data` is the live
    // `CallbackState` allocation for this synchronous invocation.
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
    // SAFETY: Per the callback contract above, `user_data` is the live
    // `CallbackState` allocation for this synchronous invocation.
    let state = unsafe { &*(user_data as *const CallbackState) };
    // SAFETY: Per the callback contract above, `event` is valid for the
    // duration of this synchronous invocation and is not retained.
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
        poll_accept_frame(self.opened, frame, |frame_ref| {
            // The close/error callbacks queue into the event channel and are
            // only drained by `poll_recv`, which runs after outbound
            // admission in a polling-client tick. Query the live ready state
            // here so a socket the browser already closed fails its sends
            // instead of silently discarding them: the JS shim returns
            // success unconditionally, and the browser drops `send()` output
            // on a closing or closed WebSocket.
            if !self.socket_open_for_sends() {
                return Err(SignalFishError::TransportSend(
                    "the WebSocket is no longer open (closing or closed)".into(),
                ));
            }
            let result = match frame_ref {
                TransportFrame::Text(message) => {
                    // Keep only the Display text: the boxed `NulError` itself
                    // embeds the whole frame byte vector, which would leak
                    // application payload through the public `Debug` output.
                    let c_msg = std::ffi::CString::new(message.as_str()).map_err(|error| {
                        SignalFishError::TransportSend(error.to_string().into())
                    })?;
                    // SAFETY: `self.socket` is a live Emscripten WebSocket handle and
                    // `c_msg` remains allocated and NUL-terminated for the duration
                    // of this synchronous FFI call.
                    unsafe { emscripten_websocket_send_utf8_text(self.socket, c_msg.as_ptr()) }
                }
                TransportFrame::Binary(bytes) => {
                    let length = u32::try_from(bytes.len()).map_err(|_| {
                        SignalFishError::TransportSend(
                            "binary frame exceeds Emscripten u32 length".into(),
                        )
                    })?;
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
                Ok(())
            } else {
                Err(SignalFishError::TransportSend(
                    format!("Emscripten WebSocket send failed: {result}").into(),
                ))
            }
        })
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
                    let drained = match &frame {
                        TransportFrame::Text(message) => message.len(),
                        TransportFrame::Binary(payload) => payload.len(),
                    };
                    release_queued_bytes(&self.inbound_queue, drained);
                    return std::task::Poll::Ready(Some(Ok(frame)));
                }
                Ok(IncomingEvent::Open) => {
                    self.opened = true;
                }
                Ok(IncomingEvent::Error(error)) => {
                    self.closed = true;
                    // The browser fires exactly one `onclose` after `onerror`
                    // and both events are typically queued together, but this
                    // terminal return stops all further draining. Consume the
                    // queued tail now (non-blocking) so the structured close
                    // metadata survives instead of being stranded behind the
                    // error; later-queued events are unreachable after a
                    // terminal receive either way.
                    while let Ok(event) = self.incoming_rx.try_recv() {
                        if let IncomingEvent::Close {
                            code,
                            was_clean,
                            reason,
                        } = event
                        {
                            self.record_peer_close(code, was_clean, reason);
                            break;
                        }
                    }
                    return std::task::Poll::Ready(Some(Err(SignalFishError::TransportReceive(
                        error.into(),
                    ))));
                }
                Ok(IncomingEvent::Close {
                    code,
                    was_clean,
                    reason,
                }) => {
                    self.closed = true;
                    self.record_peer_close(code, was_clean, reason);
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
        self.closed = true;
        let close_error = self.close_native_socket().err();
        let delete_result = self.delete_after_close_attempt();
        let result = match (close_error, delete_result) {
            (_, Err(delete_result)) => Err(SignalFishError::TransportSend(
                format!("Emscripten WebSocket callback deletion failed: {delete_result}").into(),
            )),
            (Some(close_result), Ok(())) => Err(SignalFishError::TransportSend(
                format!("Emscripten WebSocket close failed: {close_result}").into(),
            )),
            (None, Ok(())) => Ok(()),
        };
        std::task::Poll::Ready(result)
    }

    fn is_ready(&self) -> bool {
        self.opened
    }

    fn close_info(&self) -> Option<TransportCloseInfo> {
        self.close_info.clone()
    }

    fn abort(&mut self) {
        self.closed = true;
        if let Err(result) = self.close_native_socket() {
            tracing::warn!("emscripten_websocket_close returned {result}");
        }
        if let Err(result) = self.delete_after_close_attempt() {
            tracing::warn!(
                "emscripten_websocket_delete returned {result}; callback state remains live for Drop retry"
            );
        }
    }
}
// ── Drop Implementation ─────────────────────────────────────────────────────

impl Drop for EmscriptenWebSocketTransport {
    fn drop(&mut self) {
        if let Err(result) = self.close_native_socket() {
            tracing::warn!("emscripten_websocket_close returned {result} during Drop");
        }
        if let Err(result) = self.delete_after_close_attempt() {
            tracing::error!(
                "emscripten_websocket_delete returned {result} during Drop; intentionally leaking callback state to prevent use-after-free"
            );
        }
    }
}
