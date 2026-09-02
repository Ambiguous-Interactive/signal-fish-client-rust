---
name: error-handling
description: Preserve the SignalFishError model and propagation rules. Use when adding errors, mapping transport or protocol failures, changing thiserror variants, or documenting error behavior.
---

# Error Handling

Reference for thiserror patterns, SignalFishError design, and error propagation in this codebase.

## SignalFishError Overview

Defined in `src/error.rs` using `thiserror`. This enum is exhaustive.

```rust
use crate::error_codes::ErrorCode;
use crate::RoomRole;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SignalFishError {
    /// Failed to send a message through the transport. The boxed cause is
    /// the backend's original error (`#[source]`): `Error::source()` reaches
    /// it for programmatic handling. The built-in native WebSocket boxes the
    /// backend's own `tungstenite::Error`, whose chain reaches the underlying
    /// `std::io::Error` one hop down; custom transports box whatever error
    /// they produce. Display keeps the cause's own message. Construct from
    /// any `Error + Send + Sync + 'static`, or from a string detail
    /// (`"refused".into()`).
    #[error("transport send error: {0}")]
    TransportSend(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// Failed to receive a message from the transport; boxed cause like
    /// `TransportSend`.
    #[error("transport receive error: {0}")]
    TransportReceive(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// The transport connection was closed unexpectedly.
    #[error("transport connection closed")]
    TransportClosed,

    /// Failed to serialize or deserialize a protocol message.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Not connected to server.
    #[error("not connected to server")]
    NotConnected,

    /// The bounded outgoing command queue is full. Fail-fast send-side
    /// backpressure: the message was refused, not queued, nothing dropped.
    /// Callers retry, pace via send_game_data_reliable / send_signal_reliable,
    /// or raise SignalFishConfig::command_channel_capacity.
    #[error("outgoing command queue full (capacity {capacity}): ...")]
    SendBufferFull { capacity: usize },

    /// Directed room operations are refused until the server's
    /// `Authenticated` confirmation arrives.
    #[error(
        "not yet authenticated: wait for SignalFishEvent::Authenticated before room operations"
    )]
    NotAuthenticated,

    /// Not in a room.
    #[error("not in a room")]
    NotInRoom,

    /// Attempted to join or reconnect while already in a room.
    #[error("already in a room")]
    AlreadyInRoom,

    /// Attempted a room operation while a prior room transition awaits a
    /// matching typed terminal response.
    #[error("a room join, leave, or reconnect operation is already pending")]
    RoomOperationPending,

    /// Attempted an operation that is not valid for the current room role.
    #[error("operation requires the {required} room role, but the current role is {actual}")]
    WrongRoomRole {
        required: RoomRole,
        actual: RoomRole,
    },

    /// Attempted an operation reserved for the room's current authority.
    #[error("operation requires the room's current authority role")]
    AuthorityRequired,

    /// The server returned an error message.
    #[error("server error: {message}")]
    ServerError {
        message: String,
        error_code: Option<ErrorCode>,
    },

    /// A protocol-v3-only operation attempted before v3 was negotiated.
    /// mode is "relay-only" (negotiated below v3) or "pre-negotiation".
    #[error("operation requires a negotiated protocol v3 session ...")]
    ProtocolUnsupported { mode: &'static str },

    /// Signaling was attempted before the current room's first SessionPlan.
    #[error("no authoritative SessionPlan authorizes this WebRTC signal")]
    SessionPlanUnavailable,

    /// A generation-bound driver signal raced with a replacement plan.
    #[error("WebRTC signal belongs to stale session generation ...")]
    StaleSessionGeneration {
        attempted: Option<SessionGeneration>,
        current: Option<SessionGeneration>,
    },

    /// Binary payload requested while JSON was negotiated.
    #[error("binary game data requires ...")]
    BinaryFormatNotNegotiated,

    /// Caller payload nesting exceeds the maximum depth of `max_depth`
    /// nested containers (currently 128, matching serde_json's own
    /// deserialization recursion limit).
    #[error(
        "payload nesting exceeds the maximum depth of {max_depth} nested containers; \
         flatten the payload"
    )]
    PayloadTooDeep { max_depth: usize },

    /// Native WebSocket token-binding-v2 setup or outbound protection
    /// failed. The structured reason contains no secret or proof material.
    #[error("token binding error: {0}")]
    TokenBinding(TokenBindingFailure),

    /// The WebSocket handshake did not complete within the deadline given to
    /// `WebSocketTransport::connect_with_timeout`.
    #[error(
        "the WebSocket handshake did not complete within its deadline; retry or raise the connect_with_timeout duration"
    )]
    Timeout,

    /// A caller-supplied configuration value (or required build feature) was
    /// rejected before any network I/O. `problem` is non-secret and safe for
    /// ambient logs.
    #[error("invalid configuration: {field}: {problem}")]
    InvalidConfig {
        field: &'static str,
        problem: String,
    },

    /// An I/O error occurred.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Specialized Result type for Signal Fish operations.
pub type Result<T> = std::result::Result<T, SignalFishError>;
```

Note: Server-level errors arrive as `SignalFishEvent::Error { message, error_code }`
or `SignalFishEvent::RoomJoinFailed { reason, error_code }`, not as
`SignalFishError`. `SignalFishError` is for transport and client-state errors.

## thiserror Attribute Reference

```rust
#[error("...")]          // Display implementation (required)
#[from]                  // impl From<SourceType> for SignalFishError
#[source]                // marks the underlying error (without From)
```

### `#[from]` vs `#[source]`

```rust
// #[from]: auto-generates From impl AND sets source()
#[error("serialization error: {0}")]
Serialization(#[from] serde_json::Error),
// Allows: serde_json_result?  (auto-converts)

// #[source]: sets source() without generating From impl
#[error("I/O error: {0}")]
Io(#[from] std::io::Error),
```

## The ? Operator

```rust
fn do_thing(&mut self) -> Result<(), SignalFishError> {
    // serde_json::Error auto-converts via #[from] Serialization variant
    let json = serde_json::to_string(&msg)?;

    // This example backend is not the SDK's poll-based Transport trait.
    self.backend.try_send(json)
        .map_err(|e| SignalFishError::TransportSend(Box::new(e)))?;

    Ok(())
}
```

## ErrorCode Enum

The post-0.7 protocol authority declares 48 emitted tokens. The public client
enum has 54 variants: `RoomSessionIncompatible` is current, while the six
values in `ErrorCode::NON_EMITTED` are retained for older-server decoding.
Conformance must use that explicit compatibility set rather than removing
public variants.

Defined in `src/error_codes.rs`. This enum is exhaustive. 54 variants:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    // Authentication (11)
    Unauthorized, InvalidToken, AuthenticationRequired, InvalidAppId,
    AppIdExpired, AppIdRevoked, AppIdSuspended, MissingAppId,
    AuthenticationTimeout, SdkVersionUnsupported, UnsupportedGameDataFormat,

    // Validation (6)
    InvalidInput, InvalidGameName, InvalidRoomCode, InvalidPlayerName,
    InvalidMaxPlayers, MessageTooLarge,

    // Room (7)
    RoomNotFound, RoomFull, AlreadyInRoom, NotInRoom, RoomCreationFailed,
    MaxRoomsPerGameExceeded, InvalidRoomState,

    // Authority (3)
    AuthorityNotSupported, AuthorityConflict, AuthorityDenied,

    // Rate limiting (2)
    RateLimitExceeded, TooManyConnections,

    // Reconnection (4)
    ReconnectionFailed, ReconnectionTokenInvalid, ReconnectionExpired,
    PlayerAlreadyConnected,

    // Spectator (4)
    SpectatorNotAllowed, TooManySpectators, NotASpectator, SpectatorJoinFailed,

    // Server (3)
    InternalError, StorageError, ServiceUnavailable,

    // Game start and finalized room sessions (3)
    GameStartNotReady, GameStartForbidden, RoomSessionIncompatible,

    // Signaling, protocol v3 (5)
    CrossRoomSignal, UnsupportedTransport, SignalTargetNotFound,
    SignalRateLimited, SignalTooLarge,

    // Connection lifecycle, protocol v3 (1)
    ConnectionIdleTimeout,

    // Delivery & liveness (5)
    SlowConsumer, ActivityTimeout, ServerDraining, InvalidDeliveryClass,
    UnsupportedProtocolVersion,
}
```

Serializes as `SCREAMING_SNAKE_CASE` (e.g. `"ROOM_NOT_FOUND"`).
Call `error_code.description()` for a human-readable explanation.

## Mapping External Errors

`SignalFishError::TokenBinding(TokenBindingFailure)` carries only static,
typed, non-secret reasons. Never attach a handshake/derived key, nonce, proof,
signature, fingerprint, URL credential, protected frame, or TLS client config
to `Debug`, `Display`, tracing, or an error source. Challenge fields remain
explicitly inspectable through the transport accessor, while its `Debug`
redacts the nonce.

```rust
// WebSocket transport errors → TransportSend / TransportReceive.
// Box the original error so source() stays structural; Display is unchanged.
stream.send(msg).await
    .map_err(|e| SignalFishError::TransportSend(Box::new(e)))?;

stream.next().await
    .ok_or(SignalFishError::TransportClosed)?
    .map_err(|e| SignalFishError::TransportReceive(Box::new(e)))?;
```

Caller-configuration rejections that happen before any network I/O use
`SignalFishError::InvalidConfig { field, problem }` instead of an
`io::ErrorKind::InvalidInput` costume:

```rust
// A zero limit is caller error, not a network condition.
if options.max_inbound_message_size == Some(0) {
    return Err(SignalFishError::InvalidConfig {
        field: "max_inbound_message_size",
        problem: "must be greater than zero or None".into(),
    });
}
```

`problem` strings must stay non-secret and safe for ambient logs; never echo
URLs, credentials, or payload bytes into `field`/`problem`. The same rule
governs boxing causes: if an error's own `Debug` would embed payload bytes
(for example `std::ffi::NulError` retains the whole input vector), box its
`Display` text (`error.to_string().into()`) instead of the error value.

## Error Propagation Patterns

### Returning early on error

```rust
fn poll_process(
    &mut self,
    cx: &mut Context<'_>,
) -> Poll<Result<(), SignalFishError>> {
    match self.transport.poll_recv(cx) {
        Poll::Pending => Poll::Pending,
        Poll::Ready(Some(Ok(frame))) => {
            // Process the complete text or binary frame.
            let _ = frame;
            Poll::Ready(Ok(()))
        }
        Poll::Ready(Some(Err(error))) => Poll::Ready(Err(error)),
        Poll::Ready(None) => Poll::Ready(Err(SignalFishError::TransportClosed)),
    }
}
```

### Logging errors without propagating

```rust
if let Err(e) = optional_operation().await {
    tracing::warn!(error = %e, "non-fatal operation failed");
    // continue
}
```

### Never use panic!() in src/

`panic!()`, `unwrap()`, `expect()`, and `unreachable!()` are forbidden in
`src/` (non-test) code. Use `tracing::error!()` to log the condition, then
return an appropriate `SignalFishError` variant:

```rust
// WRONG — panics in production code
panic!("unexpected state: {state:?}");

// CORRECT — log and return an error
tracing::error!(?state, "unexpected state");
return Err(SignalFishError::TransportClosed);
```

Enforced by `scripts/check-no-panics.sh` and CI. The `#[cfg(test)]` escape
hatch applies only inside test modules.

## Server Errors as Events

Server-level errors arrive as events, not `SignalFishError`:

```rust
match event {
    SignalFishEvent::Error { message, error_code } => {
        if let Some(code) = error_code {
            eprintln!("Server error {code:?}: {message}");
        }
    }
    SignalFishEvent::RoomJoinFailed { reason, error_code } => { /* ... */ }
    SignalFishEvent::AuthenticationError { error, error_code } => { /* ... */ }
    _ => {}
}
```

## Testing Error Paths

```rust
#[tokio::test]
async fn test_transport_receive_error() {
    let (transport, _, _) = MockTransport::new(vec![
        Some(Err(SignalFishError::TransportReceive("boom".into()))),
    ]);
    let config = SignalFishConfig::new("mb_test");
    let (mut client, mut events) = SignalFishClient::start(transport, config);

    let _ = events.recv().await; // Connected
    let event = events.recv().await.unwrap();
    // Transport errors emit Disconnected with the error message as reason
    if let SignalFishEvent::Disconnected { reason, .. } = event {
        assert!(reason.unwrap().contains("boom"));
    }
    client.shutdown().await;
}

#[tokio::test]
async fn test_not_connected_after_shutdown() {
    // ...
    client.shutdown().await;
    let result = client.ping();
    assert!(matches!(result, Err(SignalFishError::NotConnected)));
}
```
