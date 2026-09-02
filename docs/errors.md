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

```rust,ignore
pub type Result<T> = std::result::Result<T, SignalFishError>;
```

`SignalFishError` derives `Debug` and `Error` (via `thiserror`). It is an
exhaustive public enum:

| Variant | Fields | When it occurs |
|---------|--------|----------------|
| `TransportSend` | `Box<dyn Error + Send + Sync>` | Failed to send a message through the transport. The boxed cause is the backend's original error, so `Error::source()` reaches it: the built-in native WebSocket boxes the backend's own error whose chain reaches the underlying `std::io::Error`, and custom transports keep whatever error they produce. `Display` keeps the cause's own message. |
| `TransportReceive` | `Box<dyn Error + Send + Sync>` | Failed to receive a message from the transport. Like `TransportSend`, the boxed cause stays inspectable through `Error::source()`. |
| `TransportClosed` | — | The transport connection was closed unexpectedly. |
| `TokenBinding` | `TokenBindingFailure` | Native WebSocket token-binding negotiation, challenge validation, key derivation, canonicalization, encoding, or sequence handling failed. Reasons are typed and contain no key, nonce, proof, signature, fingerprint, URL credential, or payload material. Includes `MissingClientFingerprint`, raised by the opt-in `require_client_fingerprint` connect policy when no X.509 client signer is selected. See [WebSocket Token Binding](token-binding.md). |
| `Serialization` | `serde_json::Error` | Failed to serialize or deserialize a protocol message. Implements `From<serde_json::Error>`. |
| `NotConnected` | — | Attempted an operation requiring an active connection but the client is not connected. |
| `NotAuthenticated` | — | Attempted a directed room operation (`join_room`, `leave_room`, `reconnect`, `join_as_spectator`, or `leave_spectator`) before the server confirmed authentication. The command was **not** queued; retry once the `Authenticated` event arrives. Non-room commands keep their pre-authentication behavior. |
| `SendBufferFull` | `capacity: usize` | The bounded outgoing command queue is full — the caller is producing messages faster than the transport can drain them. The message was refused, **not** queued; nothing is silently dropped. Retry later, pace with a `*_reliable` variant, drain events promptly, or raise `command_channel_capacity`. See [Handling `SendBufferFull`](#handling-sendbufferfull). |
| `NotInRoom` | — | Attempted a room operation but the client is not in a room. |
| `AlreadyInRoom` | — | Attempted to join as a player/spectator or reconnect while already in a room. |
| `RoomOperationPending` | — | A previously admitted join, leave, or reconnect still awaits a matching typed terminal response. `ping` remains available; generic errors and absent responses stay fenced until transport teardown, after which a new connection may retry. The fence applies to undecodable responses too: an unknown `error_code` string from a newer server makes the whole frame surface as a `DecodeFailed` event, so a correlated result never releases the pending operation. One benign wire race is absorbed rather than fenced: when an authoritative spectator exit (`Disconnected`, `Removed`, or `RoomClosed`) tears the room down before the server's mandatory reply to an admitted voluntary `leave_spectator`, that one superseded reply is silently consumed — any duplicate or unrelated late result still violates. |
| `WrongRoomRole` | `required: RoomRole`, `actual: RoomRole` | Attempted a player-only command as a spectator, or `leave_spectator` as a player. |
| `AuthorityRequired` | — | Attempted `start_game` while another player is authority, or attempted to relinquish authority without currently holding it. |
| `ServerError` | `message: String`, `error_code: Option<ErrorCode>` | Reserved for compatibility — no current server/SDK combination constructs this variant. Server error messages surface as `SignalFishEvent::Error`, `AuthenticationError`, `RoomJoinFailed`, or `RoomOperationFailed` instead. |
| `ProtocolUnsupported` | `mode: &'static str` | A protocol-v3-only operation (classified latest/volatile JSON, binary game data, signaling, or transport-status reporting) was attempted before v3 was negotiated. `mode` is `"pre-negotiation"` (no `ProtocolInfo` yet — negotiation still in flight) or `"relay-only"` (a `ProtocolInfo` arrived but negotiated v2, the terminal relay floor). See [Protocol Versioning](protocol-versioning.md#the-fail-fast-guard). |
| `SessionPlanUnavailable` | — | No authoritative WebRTC plan currently authorizes the signal: no plan has arrived, the target is self, unknown, departed, or absent from the replace-on-plan peer set/current room roster, or the negotiated session transport is not WebRTC (a relay plan). The set may be extended by a valid compatibility `NewPeer`; the frame is refused locally. |
| `StaleSessionGeneration` | `attempted: Option<SessionGeneration>`, `current: Option<SessionGeneration>` | A generation-bound driver signal was produced after its session plan had been replaced. The client refuses it rather than relabeling stale signaling. |
| `BinaryFormatNotNegotiated` | — | A binary send was attempted after negotiation resolved to JSON. Request `MessagePack` and confirm `effective_game_data_format() == Some(MessagePack)`; unsupported requests resolve to JSON and are refused before transport admission. Before `ProtocolInfo`, v3-only sends return `ProtocolUnsupported` instead. |
| `PayloadTooDeep` | `max_depth: usize` | A caller-supplied JSON payload was refused because its container nesting exceeds the outbound depth bound (128 nested containers, matching `serde_json`'s deserialization recursion limit). Applies to JSON game data, raw WebRTC signals (`send_raw_signal`), and `ConnectionInfo::Custom`. Refused at the call site, before queuing; flatten the payload. |
| `Timeout` | — | The WebSocket handshake did not complete within its deadline. Only `WebSocketTransport::connect_with_timeout` emits this variant, when the connection is not established within the duration it is given. |
| `InvalidConfig` | `field: &'static str`, `problem: String` | A caller-supplied configuration value was rejected because the value itself is unusable, before any network I/O: a zero inbound-size limit, a URL that cannot be parsed into a WebSocket request or whose scheme is not exactly lowercase `ws`/`wss`, a URL containing interior NUL bytes (Emscripten), or `wss://` without the opt-in `tls` feature. The failure is determined by the value or the build, not by a network outcome — retrying without correcting it keeps failing. |
| `Io` | `std::io::Error` | An I/O error occurred. Implements `From<std::io::Error>`. |

Upgrading from 0.10? The transport error payloads and the `InvalidConfig`
reclassification are breaking; see [the 0.11
migration](migration-0.11.md#structured-transport-error-causes) for
before/after examples.

Local validation has stable precedence: connection state, server-confirmed
authentication for directed room operations, an admitted pending room
transition, membership/role, authority, protocol version, format/session plan,
caller-payload shape (`PayloadTooDeep`), and then bounded-queue capacity.
Invalid state therefore does not consume queue capacity and is not hidden by
`SendBufferFull`.

### Handling errors from client methods

```rust,ignore
use signal_fish_client::{
    SignalFishClient, SignalFishConfig, SignalFishError, JoinRoomParams,
};

fn try_join(client: &mut SignalFishClient) {
    let params = JoinRoomParams::new("my-game", "Alice");
    match client.join_room(params) {
        Ok(()) => println!("Join request sent"),
        Err(SignalFishError::NotConnected) => {
            eprintln!("Cannot join — not connected to the server");
        }
        Err(SignalFishError::TransportSend(cause)) => {
            eprintln!("Transport send failed: {cause}");
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

`ErrorCode` is a protocol-level enum with **54 variants** representing
structured error codes returned by compatible Signal Fish servers. The
post-0.7 protocol authority declares 48 of them; six variants remain decodable
for older servers and are listed by `ErrorCode::NON_EMITTED`. It derives `Debug`,
`Clone`, `PartialEq`, `Eq`, `Serialize`, and `Deserialize`.

- Serializes as **`SCREAMING_SNAKE_CASE`** (e.g., `"ROOM_NOT_FOUND"`) to match
  the server's JSON wire format.
- Provides a `description()` method returning a human-readable
  `&'static str`.

```rust,ignore
use signal_fish_client::ErrorCode;

let code = ErrorCode::RoomNotFound;
println!("{}", code.description());
// "The requested room could not be found. It may have been closed or the code is incorrect."
```

### Authentication (11)

| Variant | Description |
|---------|-------------|
| `Unauthorized` | Access denied. Authentication credentials are missing or invalid. |
| `InvalidToken` *(compatibility-only)* | The authentication token is invalid, malformed, or has expired. |
| `AuthenticationRequired` *(compatibility-only)* | This operation requires authentication. |
| `InvalidAppId` | The provided application ID is not recognized. Verify the app ID is correct and free of control characters (maximum 256 bytes). |
| `AppIdExpired` *(compatibility-only)* | The application ID has expired. |
| `AppIdRevoked` *(compatibility-only)* | The application ID has been revoked. |
| `AppIdSuspended` *(compatibility-only)* | The application ID has been suspended. |
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
| `RateLimitExceeded` | Too many requests in a short time. Room/spectator admission refusals leave the connection open; a refused handshake closes it. Wait out the window before retrying. |
| `TooManyConnections` | You have too many active connections. |

### Reconnection (4)

| Variant | Description |
|---------|-------------|
| `ReconnectionFailed` | Failed to reconnect — the session may have expired, the room closed, or the attempt landed on a socket the server had already scheduled to close. The token is not consumed by such a refusal; retry from a fresh connection while the window is open. |
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
| `StorageError` | A storage error occurred while processing your request. |
| `ServiceUnavailable` *(compatibility-only)* | The service is temporarily unavailable. |

### Game Start — protocol v2 (2)

The game now starts **explicitly** via `client.start_game()` rather than
automatically when everyone is ready (see [Concepts](concepts.md#protocol-versioning-and-topology)).
Applications migrating from readiness-based auto-start must call it after an
`all_ready` lobby update; see [the 0.8 migration](migration-0.8.md#explicit-game-start).

| Variant | Description |
|---------|-------------|
| `GameStartNotReady` | Cannot start the game: not every player in the room is ready yet. |
| `GameStartForbidden` | You are not permitted to start the game. Only the room's authority may start it. |

### Finalized Room Sessions (1)

| Variant | Description |
|---------|-------------|
| `RoomSessionIncompatible` | The room already finalized a peer-to-peer session whose sticky topology/transport pair was not negotiated by this connection. Reconnect with compatible capabilities or join another room; rooms finalized to the relay floor remain open to everyone. |

### Signaling — protocol v3 (5)

Returned only on a v3-negotiated connection, in response to a `send_signal`
(or `send_offer` / `send_answer` / `send_ice_candidate` / `send_raw_signal`)
that the server could not honor. See the [Mesh Guide](mesh-guide.md).

| Variant | Description |
|---------|-------------|
| `CrossRoomSignal` | The signal targets a peer that is not in your room. |
| `UnsupportedTransport` | The requested data-path transport is not supported or was not negotiated for this connection. |
| `SignalTargetNotFound` | The signal's target peer could not be found in the room. |
| `SignalRateLimited` | Too many signaling messages were sent in a short time. Slow down and try again. |
| `SignalTooLarge` | The signal payload exceeds the maximum size allowed by the server. |

### Connection Lifecycle — protocol v3 (1)

| Variant | Description |
|---------|-------------|
| `ConnectionIdleTimeout` | The connection was closed by the server after being idle for too long. |

### Delivery & Liveness (4)

| Variant | Description |
|---------|-------------|
| `SlowConsumer` | The server evicted this connection because its outbound queue stayed full past the slow-consumer grace window (5 s by default): the client was not draining messages fast enough. The farewell frame carrying this code is written best-effort into an already-congested socket, so it may never arrive — a bare disconnect can be the only observable signal. |
| `ActivityTimeout` | The server closed the connection after prolonged protocol inactivity. Send periodic pings to keep the connection alive; frames rejected for size or content do not refresh the window. |
| `ServerDraining` | The server is draining and will close the connection; preserve the current reconnect snapshot and honor retry guidance. Existing sockets close with code 4000 at the drain deadline. |
| `InvalidDeliveryClass` | A classified `GameData` request used an invalid class/key shape or unsupported delivery token. Latest requires a key; reliable and volatile forbid one. Prefer the invalid-state-proof `GameDataDelivery` API. |

### Protocol Negotiation (1)

| Variant | Description |
|---------|-------------|
| `UnsupportedProtocolVersion` | The client's highest supported protocol version is below the server's configured minimum, or a pre-v3 connection sent a frame class that requires a newer protocol surface. |

!!! note "The six new v3 *server* codes vs. `SignalFishError::ProtocolUnsupported`"
    The five v3 signaling codes plus `ConnectionIdleTimeout` are **server-sent**
    `ErrorCode`s that arrive inside `SignalFishEvent::Error`. They are distinct
    from the client-side `SignalFishError::ProtocolUnsupported`, which fails a
    v3-only send *locally* before it ever reaches the server.

---

## Error Handling Patterns

### Handling `SignalFishEvent::Error`

The `Error` event is emitted when the server sends a generic error message.
It may include an `ErrorCode` for programmatic handling.

```rust,ignore
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

`AuthenticationError` includes a non-optional `ErrorCode`. React to specific codes to
guide the user:

```rust,ignore
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

```rust,ignore
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

### Handling `SendBufferFull`

The synchronous send methods (`send_game_data`, `send_signal`, `join_room`, …)
fail fast with `SignalFishError::SendBufferFull` when the bounded outgoing
command queue (default **1024**, set via
`SignalFishConfig::command_channel_capacity`) is full. The message is refused,
not queued — congestion is surfaced, never hidden. Four remedies, in order of
preference:

1. **Pace with the waiting variants.** `send_game_data_reliable` /
   `send_signal_reliable` are async and wait for a free slot instead of
   failing — the right tool for high-rate streams (rollback inputs, state
   sync).
2. **Retry later.** Treat the error as "try again next frame"; watch
   `send_capacity()` to see the queue drain.
3. **Raise the capacity.** `SignalFishConfig::with_command_channel_capacity(n)`
   buys more burst headroom, at the cost of more queued latency when the
   transport truly cannot keep up.
4. **Drain events promptly.** The command queue only drains while the
   transport loop runs, and the loop pauses whenever the *event* channel is
   full (overflow pauses the loop instead of dropping the event). A task that
   awaits a waiting variant while it is also the sole event consumer can
   deadlock under simultaneous send + receive pressure — drain events from a
   separate task.

```rust,ignore
use signal_fish_client::{SignalFishClient, SignalFishError};

async fn stream_input(client: &mut SignalFishClient, input: serde_json::Value) {
    match client.send_game_data(input.clone()) {
        Ok(()) => {}
        Err(SignalFishError::SendBufferFull { capacity }) => {
            // `capacity` is the configured queue bound, not the current depth.
            // Switch to the pacing variant instead of dropping the payload.
            eprintln!("send queue full (configured capacity {capacity}); pacing");
            if let Err(e) = client.send_game_data_reliable(input).await {
                eprintln!("send failed: {e}");
            }
        }
        Err(e) => eprintln!("send failed: {e}"),
    }
}
```

!!! warning "Keep draining events while awaiting a reliable send"
    The command queue only drains while the transport loop runs, and the
    loop pauses whenever the *event* channel is full (overflow pauses the loop
    instead of dropping the event). A task that awaits
    `send_game_data_reliable` while it is also
    the only consumer of the event receiver can deadlock under simultaneous
    send + receive pressure — drain events from a separate task. See the
    [`send_game_data_reliable` rustdoc](https://docs.rs/signal-fish-client)
    for details.

### Distinguishing transport errors from server errors

Transport errors are returned by client methods via `SignalFishError`, while
server errors arrive asynchronously as `SignalFishEvent` variants. Handle both
layers for robust error recovery:

```rust,ignore
use signal_fish_client::{
    SignalFishClient, SignalFishError, SignalFishEvent, ErrorCode,
};

fn send_data(client: &mut SignalFishClient) {
    let payload = serde_json::json!({"action": "move", "x": 10, "y": 20});
    match client.send_game_data(payload) {
        Ok(()) => { /* sent successfully */ }
        Err(SignalFishError::TransportSend(cause)) => {
            eprintln!("Transport layer failed to send: {cause}");
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
