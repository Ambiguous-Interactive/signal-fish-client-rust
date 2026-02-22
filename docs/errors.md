# Error Handling

The Signal Fish Client SDK uses two complementary error systems:

- **`SignalFishError`** — a Rust `Result`-based enum for errors returned by
  client methods (send failures, serialization issues, invalid state).
- **`ErrorCode`** — a protocol-level enum for structured error codes sent by
  the server inside events like `SignalFishEvent::Error` and
  `SignalFishEvent::AuthenticationError`.

---

## `SignalFishError`

All fallible client methods return `Result<T>`, which is an alias for
`std::result::Result<T, SignalFishError>`.

```rust
pub type Result<T> = std::result::Result<T, SignalFishError>;
```

`SignalFishError` derives `Debug` and `Error` (via `thiserror`). It has **9
variants**:

| Variant | Fields | When it occurs |
|---------|--------|----------------|
| `TransportSend` | `String` | Failed to send a message through the transport. |
| `TransportReceive` | `String` | Failed to receive a message from the transport. |
| `TransportClosed` | — | The transport connection was closed unexpectedly. |
| `Serialization` | `serde_json::Error` | Failed to serialize or deserialize a protocol message. Implements `From<serde_json::Error>`. |
| `NotConnected` | — | Attempted an operation requiring an active connection but the client is not connected. |
| `NotInRoom` | — | Attempted a room operation but the client is not in a room. |
| `ServerError` | `message: String`, `error_code: Option<String>` | The server returned an error message. |
| `Timeout` | — | An operation timed out. |
| `Io` | `std::io::Error` | An I/O error occurred. Implements `From<std::io::Error>`. |

### Handling errors from client methods

```rust
use signal_fish_client::{
    SignalFishClient, SignalFishConfig, SignalFishError, JoinRoomParams,
};

fn try_join(client: &SignalFishClient) {
    let params = JoinRoomParams::new("my-game", "Alice");
    match client.join_room(params) {
        Ok(()) => println!("Join request sent"),
        Err(SignalFishError::NotConnected) => {
            eprintln!("Cannot join — not connected to the server");
        }
        Err(SignalFishError::TransportSend(msg)) => {
            eprintln!("Transport send failed: {msg}");
        }
        Err(SignalFishError::Serialization(err)) => {
            eprintln!("Serialization error: {err}");
        }
        Err(e) => {
            eprintln!("Unexpected error: {e}");
        }
    }
}
```

!!! tip "The `?` operator works naturally"
    Because `SignalFishError` implements `std::error::Error`, you can propagate
    errors with `?` in any function that returns `Result<T, SignalFishError>` or
    a compatible error type.

---

## `ErrorCode`

`ErrorCode` is a protocol-level enum with **40 variants** representing
structured error codes returned by the Signal Fish server. It derives `Debug`,
`Clone`, `PartialEq`, `Eq`, `Serialize`, and `Deserialize`.

- Serializes as **`SCREAMING_SNAKE_CASE`** (e.g., `"ROOM_NOT_FOUND"`) to match
  the server's JSON wire format.
- Provides a `description()` method returning a human-readable
  `&'static str`.

```rust
use signal_fish_client::ErrorCode;

let code = ErrorCode::RoomNotFound;
println!("{}", code.description());
// "The requested room could not be found. It may have been closed or the code is incorrect."
```

### Authentication (11)

| Variant | Description |
|---------|-------------|
| `Unauthorized` | Access denied. Authentication credentials are missing or invalid. |
| `InvalidToken` | The authentication token is invalid, malformed, or has expired. |
| `AuthenticationRequired` | This operation requires authentication. |
| `InvalidAppId` | The provided application ID is not recognized. |
| `AppIdExpired` | The application ID has expired. |
| `AppIdRevoked` | The application ID has been revoked. |
| `AppIdSuspended` | The application ID has been suspended. |
| `MissingAppId` | Application ID is required but was not provided. |
| `AuthenticationTimeout` | Authentication took too long to complete. |
| `SdkVersionUnsupported` | The SDK version you are using is no longer supported. |
| `UnsupportedGameDataFormat` | The requested game data format is not supported. |

### Validation (6)

| Variant | Description |
|---------|-------------|
| `InvalidInput` | The provided input is invalid or malformed. |
| `InvalidGameName` | The game name is invalid. |
| `InvalidRoomCode` | The room code is invalid or malformed. |
| `InvalidPlayerName` | The player name is invalid. |
| `InvalidMaxPlayers` | The maximum player count is invalid. |
| `MessageTooLarge` | The message size exceeds the maximum allowed limit. |

### Room (7)

| Variant | Description |
|---------|-------------|
| `RoomNotFound` | The requested room could not be found. |
| `RoomFull` | The room has reached its maximum player capacity. |
| `AlreadyInRoom` | You are already in a room. Leave the current room first. |
| `NotInRoom` | You are not currently in any room. |
| `RoomCreationFailed` | Failed to create the room. |
| `MaxRoomsPerGameExceeded` | The maximum number of rooms for this game has been reached. |
| `InvalidRoomState` | The room is in an invalid state for this operation. |

### Authority (3)

| Variant | Description |
|---------|-------------|
| `AuthorityNotSupported` | Authority features are not enabled on this server. |
| `AuthorityConflict` | Another client has already claimed authority. |
| `AuthorityDenied` | You do not have permission to claim authority in this room. |

### Rate Limiting (2)

| Variant | Description |
|---------|-------------|
| `RateLimitExceeded` | Too many requests in a short time. Slow down and try again later. |
| `TooManyConnections` | You have too many active connections. |

### Reconnection (4)

| Variant | Description |
|---------|-------------|
| `ReconnectionFailed` | Failed to reconnect to the room. |
| `ReconnectionTokenInvalid` | The reconnection token is invalid or malformed. |
| `ReconnectionExpired` | The reconnection window has expired. |
| `PlayerAlreadyConnected` | This player is already connected from another session. |

### Spectator (4)

| Variant | Description |
|---------|-------------|
| `SpectatorNotAllowed` | Spectator mode is not enabled for this room. |
| `TooManySpectators` | The room has reached its maximum spectator capacity. |
| `NotASpectator` | You are not a spectator in this room. |
| `SpectatorJoinFailed` | Failed to join as a spectator. |

### Server (3)

| Variant | Description |
|---------|-------------|
| `InternalError` | An internal server error occurred. |
| `DatabaseError` | A database error occurred while processing your request. |
| `ServiceUnavailable` | The service is temporarily unavailable. |

---

## Error Handling Patterns

### Handling `SignalFishEvent::Error`

The `Error` event is emitted when the server sends a generic error message.
It may include an `ErrorCode` for programmatic handling.

```rust
use signal_fish_client::{SignalFishEvent, ErrorCode};

match event {
    SignalFishEvent::Error { message, error_code } => {
        if let Some(code) = &error_code {
            eprintln!("[{code}] {message}");
        } else {
            eprintln!("Server error: {message}");
        }
    }
    _ => {}
}
```

### Handling `SignalFishEvent::AuthenticationError`

Authentication errors always include an `ErrorCode`. React to specific codes to
guide the user:

```rust
use signal_fish_client::{SignalFishEvent, ErrorCode};

match event {
    SignalFishEvent::AuthenticationError { error, error_code } => {
        match error_code {
            ErrorCode::InvalidToken => {
                eprintln!("Token expired or invalid — request a new token");
            }
            ErrorCode::InvalidAppId => {
                eprintln!("Check your app ID configuration");
            }
            ErrorCode::SdkVersionUnsupported => {
                eprintln!("Please upgrade to the latest SDK version");
            }
            _ => {
                eprintln!("Authentication failed: {error}");
            }
        }
    }
    _ => {}
}
```

### Retrying on `RateLimitExceeded`

When the server reports rate limiting, back off before retrying:

```rust
use signal_fish_client::{SignalFishEvent, ErrorCode};
use std::time::Duration;

async fn handle_event(event: SignalFishEvent) {
    match event {
        SignalFishEvent::Error { error_code, message } => {
            if error_code == Some(ErrorCode::RateLimitExceeded) {
                eprintln!("Rate limited: {message} — retrying after delay");
                tokio::time::sleep(Duration::from_secs(2)).await;
                // … retry the operation
            }
        }
        _ => {}
    }
}
```

!!! warning "Respect server rate limits"
    The `RateLimitInfo` provided in the `Authenticated` event tells you the
    per-minute, per-hour, and per-day limits for your application. Proactively
    throttling requests avoids `RateLimitExceeded` errors entirely.

### Distinguishing transport errors from server errors

Transport errors are returned by client methods via `SignalFishError`, while
server errors arrive asynchronously as `SignalFishEvent` variants. Handle both
layers for robust error recovery:

```rust
use signal_fish_client::{
    SignalFishClient, SignalFishError, SignalFishEvent, ErrorCode,
};

fn send_data(client: &SignalFishClient) {
    let payload = serde_json::json!({"action": "move", "x": 10, "y": 20});
    match client.send_game_data(payload) {
        Ok(()) => { /* sent successfully */ }
        Err(SignalFishError::TransportSend(msg)) => {
            eprintln!("Transport layer failed to send: {msg}");
        }
        Err(SignalFishError::TransportClosed) => {
            eprintln!("Connection lost — need to reconnect");
        }
        Err(SignalFishError::NotConnected) => {
            eprintln!("Client is not connected");
        }
        Err(SignalFishError::NotInRoom) => {
            eprintln!("Must join a room before sending game data");
        }
        Err(e) => {
            eprintln!("Send failed: {e}");
        }
    }
}

async fn handle_event(event: SignalFishEvent) {
    match event {
        // Server-side errors arrive as events
        SignalFishEvent::Error { message, error_code } => {
            match error_code {
                Some(ErrorCode::MessageTooLarge) => {
                    eprintln!("Payload too large: {message}");
                }
                Some(ErrorCode::NotInRoom) => {
                    eprintln!("Server says we are not in a room");
                }
                Some(code) => {
                    eprintln!("Server error [{code}]: {message}");
                }
                None => {
                    eprintln!("Server error: {message}");
                }
            }
        }
        _ => {}
    }
}
```

!!! info "Two error channels"
    | Channel | Type | When |
    |---------|------|------|
    | `Result<T>` from client methods | `SignalFishError` | Immediate local failures (serialization, transport, invalid state). |
    | Event receiver | `SignalFishEvent::Error`, `AuthenticationError`, `RoomJoinFailed`, etc. | Asynchronous errors reported by the server. |
