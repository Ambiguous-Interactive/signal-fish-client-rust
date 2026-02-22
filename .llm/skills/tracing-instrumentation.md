# Tracing Instrumentation

Reference for the tracing crate, #[instrument] macro, spans, events, and log levels in this codebase.

## Setup

```toml
[dependencies]
tracing = "0.1"

[dev-dependencies]
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

The crate uses `tracing` for structured diagnostics. Subscribers (what actually records/outputs logs) are chosen by the application, not the library.

## Core Macros

```rust
use tracing::{trace, debug, info, warn, error};

trace!("very verbose detail");
debug!(value = 42, "computed value");
info!(room_id = %room_id, "player joined lobby");
warn!(attempt = 3, "connection retry");
error!(error = %e, "fatal transport failure");
```

### Log Level Guidelines

| Level | When to Use |
|-------|-------------|
| `trace` | Per-message protocol details, polling loops |
| `debug` | Connection lifecycle, state changes |
| `info` | Significant user-visible events (lobby join, disconnect) |
| `warn` | Recoverable issues (retry, ignored message) |
| `error` | Unrecoverable failures |

## Field Syntax

```rust
// %value — use Display formatting
info!(player_id = %player_id, "player connected");

// ?value — use Debug formatting
debug!(message = ?msg, "received message");

// value = expr — attach any primitive
warn!(retry_count = attempt, delay_ms = delay.as_millis(), "retrying");

// Shorthand when field name == variable name
let room_id = "lobby-1";
info!(%room_id, "joined");  // equivalent to info!(room_id = %room_id)
```

## #[instrument] Macro

Automatically creates a span around a function:

```rust
use tracing::instrument;

#[instrument(skip(self), fields(game_name = %game_name))]
pub fn join_room(&self, params: JoinRoomParams) -> Result<(), SignalFishError> {
    debug!("sending join_room request");
    // ...
    info!("join_room queued");
    Ok(())
}
```

### instrument Options

```rust
// skip — don't include these args in the span
#[instrument(skip(self, transport))]

// skip_all — skip all arguments
#[instrument(skip_all)]

// fields — add computed fields
#[instrument(fields(otel.name = "join_room", game = %game_name))]

// name — override the span name
#[instrument(name = "client.join_room")]

// level — set span level
#[instrument(level = "debug")]

// err — log errors automatically
#[instrument(err)]

// ret — log return value
#[instrument(ret)]
```

## Manual Spans

For more control:

```rust
use tracing::{info_span, Instrument};

// Attach a span to a future
let span = info_span!("process_message", msg_type = %msg_type);
process(msg).instrument(span).await?;

// Enter a span synchronously
let span = tracing::debug_span!("serialize");
let _guard = span.enter();
let json = serde_json::to_string(&msg)?;
// _guard dropped here, span exits
```

## Structured Events in Protocol Processing

```rust
async fn handle_server_message(&self, raw: &str) -> Result<(), SignalFishError> {
    let msg: ServerMessage = serde_json::from_str(raw)
        .map_err(|e| {
            tracing::warn!(error = %e, raw = raw, "failed to parse server message");
            SignalFishError::from(e)
        })?;

    match &msg {
        ServerMessage::RoomJoined(payload) => {
            tracing::info!(room_code = %payload.room_code, player_id = %payload.player_id, "room joined");
        }
        ServerMessage::Error { message, error_code } => {
            tracing::warn!(?error_code, %message, "server error received");
        }
        _ => {
            tracing::debug!(message_type = ?msg, "received server message");
        }
    }
    Ok(())
}
```

## Enabling Logs in Tests

```rust
// In test setup (call once per test binary)
fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("signal_fish_client=debug")
        .with_test_writer()  // writes to test output (captured by cargo test)
        .try_init();
}

#[tokio::test]
async fn my_test() {
    init_tracing();
    // now tracing output appears in `cargo test -- --nocapture`
}
```

Or use a shared `once_cell`:

```rust
use std::sync::OnceLock;
static TRACING: OnceLock<()> = OnceLock::new();

fn init() {
    TRACING.get_or_init(|| {
        tracing_subscriber::fmt()
            .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".into()))
            .with_test_writer()
            .init();
    });
}
```

## Propagating Context (OpenTelemetry)

For distributed tracing (not currently a dependency, but a common addition):

```toml
# If adding in the future
opentelemetry = "0.27"
tracing-opentelemetry = "0.28"
```

The tracing ecosystem is OTel-compatible — existing spans propagate automatically once a subscriber is configured.

## Performance Notes

- `tracing` macros are zero-cost when no subscriber is active
- Avoid format strings in hot paths — use structured fields instead
- `%value` calls `Display`, `?value` calls `Debug` — prefer `%` for types with a clean Display
- `#[instrument]` on every async fn adds overhead; use selectively on public API methods

## Environment Variable Control

```shell
# Enable all debug from this crate
RUST_LOG=signal_fish_client=debug cargo test

# Enable trace for transport module only
RUST_LOG=signal_fish_client::transport=trace cargo run

# Multiple filters
RUST_LOG=signal_fish_client=debug,tokio_tungstenite=warn cargo run
```
