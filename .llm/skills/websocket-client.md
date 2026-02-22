# WebSocket Client

Reference for tokio-tungstenite usage, connection lifecycle, and reconnection patterns.

## Feature Flag

The WebSocket transport is gated behind `transport-websocket` (enabled by default):

```toml
[features]
default = ["transport-websocket"]
transport-websocket = ["dep:tokio-tungstenite", "dep:futures-util"]
```

Guard with `#[cfg(feature = "transport-websocket")]` in code.

## Connecting

```rust
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{SinkExt, StreamExt};

let url = "wss://signal.example.com/ws";
let (ws_stream, _response) = connect_async(url).await
    .map_err(|e| SignalFishError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
```

## Sending Messages

```rust
// Text frame (Signal Fish uses JSON text frames)
sink.send(Message::Text(json_string)).await
    .map_err(|e| SignalFishError::TransportSend(e.to_string()))?;

// Ping (for keepalive)
sink.send(Message::Ping(vec![])).await?;

// Close
sink.send(Message::Close(None)).await?;
```

## Receiving Messages

```rust
loop {
    match stream.next().await {
        Some(Ok(Message::Text(text))) => {
            // Handle text frame
            handle_message(text).await?;
        }
        Some(Ok(Message::Binary(_))) => {
            // Signal Fish protocol uses text only; log and ignore
            tracing::warn!("received unexpected binary frame");
        }
        Some(Ok(Message::Ping(data))) => {
            // tungstenite auto-responds to Pings in most configs
            // Explicit pong if needed:
            sink.send(Message::Pong(data)).await?;
        }
        Some(Ok(Message::Pong(_))) => { /* ignore */ }
        Some(Ok(Message::Close(_))) => {
            tracing::info!("server closed WebSocket");
            break;
        }
        Some(Ok(Message::Frame(_))) => { /* raw frame, ignore */ }
        Some(Err(e)) => {
            return Err(SignalFishError::TransportReceive(e.to_string()));
        }
        None => break, // stream exhausted
    }
}
```

## WebSocket Transport Implementation

Key details from `src/transports/websocket.rs` (see source for full implementation):

```rust
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::protocol::Message,
};
use tokio::net::TcpStream;
use futures_util::{SinkExt, StreamExt};

/// Type alias for the underlying WebSocket stream (not split).
pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct WebSocketTransport {
    stream: WsStream,  // unsplit — send and recv share the same stream
    closed: bool,
}

impl WebSocketTransport {
    pub async fn connect(url: &str) -> Result<Self, SignalFishError> {
        let (stream, _response) = tokio_tungstenite::connect_async(url).await
            .map_err(|e| {
                let kind = match &e {
                    tokio_tungstenite::tungstenite::Error::Io(io) => io.kind(),
                    _ => std::io::ErrorKind::Other,
                };
                SignalFishError::Io(std::io::Error::new(kind, e))  // NOT TransportSend
            })?;
        Ok(Self { stream, closed: false })
    }
}

#[async_trait]
impl Transport for WebSocketTransport {
    async fn send(&mut self, message: String) -> Result<(), SignalFishError> {
        if self.closed { return Err(SignalFishError::TransportClosed); }
        self.stream.send(Message::Text(message.into())).await
            .map_err(|e| SignalFishError::TransportSend(e.to_string()))
    }

    async fn recv(&mut self) -> Option<Result<String, SignalFishError>> {
        loop {
            match self.stream.next().await {
                Some(Ok(Message::Text(t))) => return Some(Ok(t.to_string())),
                Some(Ok(Message::Close(_))) | None => return None,
                Some(Ok(_)) => continue, // skip ping/pong/binary/frame
                Some(Err(e)) => return Some(Err(SignalFishError::TransportReceive(e.to_string()))),
            }
        }
    }

    async fn close(&mut self) -> Result<(), SignalFishError> {
        if self.closed { return Ok(()); }
        self.closed = true;
        self.stream.close(None).await
            .map_err(|e| SignalFishError::TransportSend(e.to_string()))
    }
}
```

## TLS Configuration

`tokio-tungstenite` uses `native-tls` or `rustls` depending on features:

```toml
# Use rustls (recommended for cross-platform)
tokio-tungstenite = { version = "0.28", features = ["rustls-tls-webpki-roots"] }

# Use native-tls (OS certificate store)
tokio-tungstenite = { version = "0.28", features = ["native-tls"] }
```

For `wss://` URLs, TLS is applied automatically. For `ws://`, no TLS.

## Reconnection Pattern

```rust
async fn connect_with_retry(url: &str, max_attempts: u32) -> Result<WebSocketTransport, SignalFishError> {
    let mut delay = Duration::from_millis(500);
    for attempt in 1..=max_attempts {
        match WebSocketTransport::connect(url).await {
            Ok(t) => return Ok(t),
            Err(e) if attempt < max_attempts => {
                tracing::warn!(attempt, %e, "WebSocket connect failed, retrying");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(30)); // exponential backoff
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}
```

## Connection Headers

Pass custom headers (e.g., auth tokens):

```rust
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

let mut request = url.into_client_request()?;
request.headers_mut().insert(
    "Authorization",
    HeaderValue::from_str(&format!("Bearer {token}"))?,
);
let (ws, _) = connect_async(request).await?;
```

## Keepalive / Ping

Signal Fish server may close idle connections. Send periodic pings:

```rust
let ping_interval = tokio::time::interval(Duration::from_secs(20));
tokio::select! {
    msg = stream.next() => { /* handle */ }
    _ = ping_interval.tick() => {
        sink.send(Message::Ping(vec![])).await.ok();
    }
}
```

## Common Errors

| Error | Cause | Fix |
|-------|-------|-----|
| `ConnectionRefused` | Server not running | Check URL and server status |
| `HandshakeError` | TLS cert issue | Check TLS feature flag and cert validity |
| `AlreadyClosed` | Sending after close | Check transport state before send |
| `SendAfterClosing` | Race condition | Use shutdown signal to coordinate |
