# Error Handling

Reference for thiserror patterns, SignalFishError design, and error propagation in this codebase.

## SignalFishError Overview

Defined in `src/error.rs` using `thiserror`. Does NOT carry `#[non_exhaustive]`.

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SignalFishError {
    /// Failed to send a message through the transport.
    #[error("transport send error: {0}")]
    TransportSend(String),

    /// Failed to receive a message from the transport.
    #[error("transport receive error: {0}")]
    TransportReceive(String),

    /// The transport connection was closed unexpectedly.
    #[error("transport connection closed")]
    TransportClosed,

    /// Failed to serialize or deserialize a protocol message.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Not connected to server.
    #[error("not connected to server")]
    NotConnected,

    /// Not in a room.
    #[error("not in a room")]
    NotInRoom,

    /// The server returned an error message.
    #[error("server error: {message}")]
    ServerError {
        message: String,
        error_code: Option<String>,
    },

    /// An operation timed out.
    #[error("operation timed out")]
    Timeout,

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
async fn do_thing(&mut self) -> Result<(), SignalFishError> {
    // serde_json::Error auto-converts via #[from] Serialization variant
    let json = serde_json::to_string(&msg)?;

    // String errors need manual mapping
    self.transport.send(json).await
        .map_err(|e| SignalFishError::TransportSend(e.to_string()))?;

    Ok(())
}
```

## ErrorCode Enum

Defined in `src/error_codes.rs`. Does NOT carry `#[non_exhaustive]`. 40 variants:

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
    InternalError, DatabaseError, ServiceUnavailable,
}
```

Serializes as `SCREAMING_SNAKE_CASE` (e.g. `"ROOM_NOT_FOUND"`).
Call `error_code.description()` for a human-readable explanation.

## Mapping External Errors

```rust
// WebSocket transport errors → TransportSend / TransportReceive
stream.send(msg).await
    .map_err(|e| SignalFishError::TransportSend(e.to_string()))?;

stream.next().await
    .ok_or(SignalFishError::TransportClosed)?
    .map_err(|e| SignalFishError::TransportReceive(e.to_string()))?;
```

## Error Propagation Patterns

### Returning early on error

```rust
async fn process(&mut self) -> Result<(), SignalFishError> {
    match self.transport.recv().await {
        Some(Ok(s)) => { /* use s */ }
        Some(Err(e)) => return Err(e),
        None => return Err(SignalFishError::TransportClosed),
    }
    Ok(())
}
```

### Logging errors without propagating

```rust
if let Err(e) = optional_operation().await {
    tracing::warn!(error = %e, "non-fatal operation failed");
    // continue
}
```

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
    if let SignalFishEvent::Disconnected { reason } = event {
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
