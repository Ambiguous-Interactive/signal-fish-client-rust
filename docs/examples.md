# Example Walkthroughs

Hands-on examples that demonstrate the Signal Fish Client SDK in realistic
scenarios. Each walkthrough covers the **complete** source file, then breaks it
down step by step.

!!! tip "Running the examples"

    Every example lives in the `examples/` directory and can be launched with
    `cargo run --example <name>`.

---

## Basic Lobby

**Source:** `examples/basic_lobby.rs`

Demonstrates the **full WebSocket lifecycle** — connecting to a Signal Fish
server, authenticating, joining a room, reacting to lobby events, and shutting
down gracefully.

!!! info "This is a v2 / relay example"
    `basic_lobby` uses the default relay-floor configuration
    (`SignalFishConfig::new(...)`), so all traffic is relayed through the server.
    For peer-to-peer WebRTC mesh see the
    [Mesh Session example](#mesh-session-protocol-v3) and the
    [Mesh Guide](mesh-guide.md).

### Full source

```rust
use signal_fish_client::{
    JoinRoomParams, SignalFishClient, SignalFishConfig, SignalFishEvent, WebSocketTransport,
};

const DEFAULT_URL: &str = "ws://localhost:3536/ws";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let url = std::env::var("SIGNAL_FISH_URL").unwrap_or_else(|_| DEFAULT_URL.to_string());
    tracing::info!("Connecting to {url}");

    let transport = WebSocketTransport::connect(&url).await?;
    let config = SignalFishConfig::new("mb_app_abc123");
    let (mut client, mut event_rx) = SignalFishClient::start(transport, config);

    loop {
        tokio::select! {
            event = event_rx.recv() => {
                let Some(event) = event else {
                    tracing::info!("Event channel closed, exiting");
                    break;
                };

                match event {
                    SignalFishEvent::Connected => {
                        tracing::info!("Transport connected, awaiting authentication…");
                    }
                    SignalFishEvent::Authenticated { app_name, .. } => {
                        tracing::info!("Authenticated as app: {app_name}");
                        let params = JoinRoomParams::new("example-game", "RustPlayer")
                            .with_max_players(4);
                        client.join_room(params)?;
                        tracing::info!("Join-room request sent");
                    }
                    SignalFishEvent::RoomJoined {
                        room_code, player_id, current_players, ..
                    } => {
                        tracing::info!(
                            "Joined room {room_code} as player {player_id} ({} player(s) present)",
                            current_players.len()
                        );
                        client.set_ready()?;
                        tracing::info!("Set ready");
                    }
                    SignalFishEvent::PlayerJoined { player } => {
                        tracing::info!("Player joined: {} ({})", player.name, player.id);
                    }
                    SignalFishEvent::PlayerLeft { player_id } => {
                        tracing::info!("Player left: {player_id}");
                    }
                    SignalFishEvent::LobbyStateChanged { lobby_state, all_ready, .. } => {
                        tracing::info!("Lobby state → {lobby_state:?} (all_ready={all_ready})");
                    }
                    SignalFishEvent::GameStarting { peer_connections } => {
                        tracing::info!(
                            "Game starting with {} peer connection(s)!",
                            peer_connections.len()
                        );
                    }
                    SignalFishEvent::AuthenticationError { error, error_code } => {
                        tracing::error!("Auth failed [{error_code:?}]: {error}");
                        break;
                    }
                    SignalFishEvent::Error { message, error_code } => {
                        tracing::error!("Server error [{error_code:?}]: {message}");
                    }
                    SignalFishEvent::Disconnected { reason } => {
                        tracing::warn!("Disconnected: {}", reason.as_deref().unwrap_or("unknown"));
                        break;
                    }
                    other => {
                        tracing::debug!("Event: {other:?}");
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Ctrl+C received, shutting down…");
                break;
            }
        }
    }

    client.shutdown().await;
    tracing::info!("Client shut down. Goodbye!");
    Ok(())
}
```

### Step-by-step walkthrough

#### 1. Connect to the server via WebSocket

```rust,ignore
let transport = WebSocketTransport::connect(&url).await?;
```

`WebSocketTransport::connect` opens a WebSocket connection to the Signal Fish
server. The URL can be overridden with the **`SIGNAL_FISH_URL`** environment
variable; it defaults to `ws://localhost:3536/ws`.

#### 2. Configure and start the client

```rust,ignore
let config = SignalFishConfig::new("mb_app_abc123");
let (mut client, mut event_rx) = SignalFishClient::start(transport, config);
```

`SignalFishConfig` carries your **App ID** (issued by the server).
`SignalFishClient::start` consumes the transport and config, returning:

- a `SignalFishClient` handle for sending commands, and
- an `mpsc::Receiver<SignalFishEvent>` for incoming events.

#### 3. Event loop with `tokio::select!`

```rust,ignore
loop {
    tokio::select! {
        event = event_rx.recv() => { /* … */ }
        _ = tokio::signal::ctrl_c() => { break; }
    }
}
```

The outer `loop` + `tokio::select!` pattern lets the example react to **server
events** while also catching **Ctrl+C** for a clean exit.

#### 4. Handle authentication, room join, and lobby events

| Event | Action taken |
|---|---|
| `Connected` | Log that the transport is up; wait for authentication. |
| `Authenticated` | Build `JoinRoomParams` and call `client.join_room(…)`. |
| `RoomJoined` | Log the room code and player list, then call `client.set_ready()`. |
| `PlayerJoined` / `PlayerLeft` | Log the change. |
| `LobbyStateChanged` | Log the new lobby state and whether all players are ready. |
| `GameStarting` | Log the peer connections — the game is about to begin. |
| `AuthenticationError` / `Error` | Log the error (and break on auth failure). |
| `Disconnected` | Log the reason and exit the loop. |

#### 5. Graceful shutdown

```rust,ignore
client.shutdown().await;
```

`shutdown()` closes the transport and drains background tasks so the process
exits cleanly.

### Running it

```sh
cargo run --example basic_lobby
```

!!! info "Environment variable"

    Set **`SIGNAL_FISH_URL`** to point at your server if it is not running on
    the default `ws://localhost:3536/ws`.

    ```sh
    SIGNAL_FISH_URL=ws://my-server:3536/ws cargo run --example basic_lobby
    ```

---

## Custom Transport

**Source:** `examples/custom_transport.rs`

Demonstrates how to **implement the `Transport` trait** with a simple in-memory
loopback, then wire it into the SDK — perfect for **unit testing without a
network**.

!!! info "This is a v2 / relay example"
    Like `basic_lobby`, this example uses the default relay-floor configuration.
    The `Transport` trait being implemented here is the byte channel to the
    *signaling server* — distinct from the v3 `WebRtcDriver` peer-to-peer seam in
    the [Mesh Guide](mesh-guide.md).

### Simplified source

!!! note
    The actual [`examples/custom_transport.rs`](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/blob/main/examples/custom_transport.rs) uses the `tracing` crate for structured logging. The version below uses `println!` for simplicity.

```rust
use async_trait::async_trait;
use signal_fish_client::{
    SignalFishClient, SignalFishConfig, SignalFishError, SignalFishEvent, Transport,
};
use tokio::sync::mpsc;

pub struct LoopbackTransport {
    tx: mpsc::UnboundedSender<String>,
    rx: mpsc::UnboundedReceiver<String>,
}

pub struct LoopbackServer {
    pub rx: mpsc::UnboundedReceiver<String>,
    pub tx: mpsc::UnboundedSender<String>,
}

fn loopback_pair() -> (LoopbackTransport, LoopbackServer) {
    let (client_tx, server_rx) = mpsc::unbounded_channel();
    let (server_tx, client_rx) = mpsc::unbounded_channel();
    let transport = LoopbackTransport { tx: client_tx, rx: client_rx };
    let server = LoopbackServer { rx: server_rx, tx: server_tx };
    (transport, server)
}

#[async_trait]
impl Transport for LoopbackTransport {
    async fn send(&mut self, message: String) -> Result<(), SignalFishError> {
        self.tx.send(message).map_err(|e| SignalFishError::TransportSend(e.to_string()))
    }
    async fn recv(&mut self) -> Option<Result<String, SignalFishError>> {
        self.rx.recv().await.map(Ok)
    }
    async fn close(&mut self) -> Result<(), SignalFishError> {
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (transport, mut server) = loopback_pair();
    let config = SignalFishConfig::new("mb_app_test");
    let (mut client, mut event_rx) = SignalFishClient::start(transport, config);

    let auth_msg = server.rx.recv().await.expect("should receive Authenticate");
    println!("Server received: {auth_msg}");

    let auth_response = serde_json::json!({
        "type": "Authenticated",
        "data": {
            "app_name": "Test App",
            "organization": null,
            "rate_limits": { "per_minute": 60, "per_hour": 3600, "per_day": 86400 }
        }
    });
    server.tx.send(auth_response.to_string())?;

    let mut events_seen = 0;
    while let Some(event) = event_rx.recv().await {
        match &event {
            SignalFishEvent::Connected => println!("Event: Connected (synthetic)"),
            SignalFishEvent::Authenticated { app_name, .. } => {
                println!("Event: Authenticated — app_name={app_name}");
            }
            SignalFishEvent::Disconnected { reason } => {
                println!("Event: Disconnected — {}", reason.as_deref().unwrap_or("clean"));
                break;
            }
            _ => println!("Event: {event:?}"),
        }
        events_seen += 1;
        if events_seen >= 2 { break; }
    }

    client.shutdown().await;
    println!("Done — saw {events_seen} event(s). Custom transport works!");
    Ok(())
}
```

### Step-by-step walkthrough

#### 1. Define `LoopbackTransport` with mpsc channels

```rust,ignore
pub struct LoopbackTransport {
    tx: mpsc::UnboundedSender<String>,
    rx: mpsc::UnboundedReceiver<String>,
}
```

Two unbounded channels form a **bidirectional pipe**: one for messages the
client sends *to* the server, one for messages the server sends *back*.

The helper `loopback_pair()` wires the channels so that each side's `tx` feeds
the other side's `rx`.

#### 2. Implement the `Transport` trait

```rust,ignore
#[async_trait]
impl Transport for LoopbackTransport {
    async fn send(&mut self, message: String) -> Result<(), SignalFishError> {
        self.tx.send(message).map_err(|e| SignalFishError::TransportSend(e.to_string()))
    }
    async fn recv(&mut self) -> Option<Result<String, SignalFishError>> {
        self.rx.recv().await.map(Ok)
    }
    async fn close(&mut self) -> Result<(), SignalFishError> {
        Ok(())
    }
}
```

Only three methods are required:

| Method | Purpose |
|---|---|
| `send` | Push a serialized message toward the server. |
| `recv` | Await the next message from the server (returns `None` on close). |
| `close` | Perform any cleanup (no-op here). |

#### 3. Create a fake server that injects responses

```rust,ignore
let auth_msg = server.rx.recv().await.expect("should receive Authenticate");
let auth_response = serde_json::json!({ /* … */ });
server.tx.send(auth_response.to_string())?;
```

The "server" side reads the auto-sent `Authenticate` message, then replies with
a hand-crafted `Authenticated` JSON payload — no network required.

#### 4. Wire into the client and observe events

```rust,ignore
let (mut client, mut event_rx) = SignalFishClient::start(transport, config);
```

From the SDK's perspective the loopback behaves identically to a real WebSocket.
The example collects two events (`Connected` and `Authenticated`) before
shutting down.

!!! note "Use case: unit testing without a network"

    This pattern is the recommended way to **test application logic** that
    depends on Signal Fish without needing a live server. Inject whatever
    server messages you like and assert on the resulting events.

### Running it

```sh
cargo run --example custom_transport
```

Expected output (from the simplified source above; the actual example uses
`tracing` so output includes timestamps and log levels):

```text
Server received: {"type":"Authenticate","data":{"app_id":"mb_app_test","sdk_version":"<version>"}}
Event: Connected (synthetic)
Event: Authenticated — app_name=Test App
Done — saw 2 event(s). Custom transport works!
```

---

## Mesh Session (protocol v3)

**Source:** `examples/mesh_session.rs`

Demonstrates the **batteries-included mesh path**: implement the `WebRtcDriver`
trait against your WebRTC stack, hand it to `MeshController`, and the SDK drives
the entire v3 signaling handshake for you — obeying the server's `initiate`
directives, relaying offers/answers/ICE, reporting transport status, and
surfacing a clean `MeshEvent` stream. The example is fully self-contained: a
scripted in-process "server" plays the v3 handshake and a tiny in-memory driver
completes it, so the whole stack runs end-to-end with no network.

!!! info "Requires the `mesh` and `tokio-runtime` features"
    Run it with:
    ```sh
    cargo run --example mesh_session --features mesh,tokio-runtime
    ```
    See the [Mesh Guide](mesh-guide.md) for the full concepts behind this flow.

### Step-by-step walkthrough

#### 1. Implement `WebRtcDriver`

The example's `DemoDriver` models a realistic handshake without real WebRTC: the
initiator emits an offer on `connect`; the answerer emits an answer (and "opens"
the channel) when it receives an offer; the initiator "opens" the channel when it
receives the answer. The integration points are marked `// REAL DRIVER:`.

```rust,ignore
impl WebRtcDriver for DemoDriver {
    fn set_ice_servers(&mut self, servers: &[IceServer]) { /* configure STUN/TURN */ }

    fn connect(&mut self, peer: PlayerId, initiate: bool) {
        // Obey `initiate`: only the designated offerer creates an offer.
        if initiate {
            self.outbox.push_back(DriverEvent::Signal {
                peer,
                signal: PeerSignal::Offer("<sdp-offer>".into()),
            });
        }
    }

    fn on_signal(&mut self, peer: PlayerId, signal: PeerSignal) {
        match signal {
            PeerSignal::Offer(_) => {
                self.outbox.push_back(DriverEvent::Signal {
                    peer, signal: PeerSignal::Answer("<sdp-answer>".into()),
                });
                self.outbox.push_back(DriverEvent::Connected { peer });
            }
            PeerSignal::Answer(_) => self.outbox.push_back(DriverEvent::Connected { peer }),
            PeerSignal::IceCandidate(_) => {}
        }
    }

    fn send(&mut self, peer: PlayerId, data: &[u8]) { /* write to data channel */ }
    fn disconnect(&mut self, peer: PlayerId) { /* close the peer connection */ }
    fn poll(&mut self) -> Option<DriverEvent> { self.outbox.pop_front() }
}
```

A **real** driver wraps str0m, webrtc-rs, or the browser's `RtcPeerConnection`
(via web-sys) — see [Integrating a real backend](mesh-guide.md#integrating-a-real-backend).

#### 2. Start the `MeshController`

`MeshController::start` enables mesh automatically if the config didn't, then
drives the handshake. Note the config is the plain relay-floor
`SignalFishConfig::new("demo-app")` — `start` upgrades it to mesh for you.

```rust,ignore
let mut mesh = MeshController::start(
    transport,
    SignalFishConfig::new("demo-app"),
    DemoDriver::default(),
);
```

#### 3. Drive the event loop

A handful of lines drive the whole flow. The controller surfaces every underlying
event as `MeshEvent::Signaling`, plus the high-level `PeerConnected` /
`PeerDisconnected` / `Data` events.

```rust,ignore
while let Some(event) = mesh.recv().await {
    match event {
        MeshEvent::Signaling(sig) => match *sig {
            SignalFishEvent::Authenticated { .. } =>
                mesh.join_room(JoinRoomParams::new("demo-game", "Alice"))?,
            SignalFishEvent::LobbyStateChanged { all_ready: true, .. } =>
                mesh.start_game()?,
            SignalFishEvent::SessionPlan { peers, .. } =>
                println!("session plan: {} peer(s) to connect", peers.len()),
            _ => {}
        },
        MeshEvent::PeerConnected(peer) => {
            mesh.send_to(peer, b"hello peer"); // data channel is open
            break; // demo complete
        }
        MeshEvent::PeerDisconnected(peer) => println!("peer {peer} disconnected"),
        MeshEvent::Data { from, data } => println!("{} bytes from {from}", data.len()),
    }
}

mesh.shutdown().await;
```

The flow end to end: authenticate → join room → `start_game()` → the scripted
server sends a `SessionPlan` (`initiate=true`) → the driver offers → the
controller relays the offer → the peer's answer arrives → the driver opens the
channel → `MeshEvent::PeerConnected` fires and the example sends a packet.

### Running it

```sh
cargo run --example mesh_session --features mesh,tokio-runtime
```

---

## Godot Web Export (Polling Client)

Demonstrates how to use `SignalFishPollingClient` with
`EmscriptenWebSocketTransport` in a Godot 4.5 web export via gdext
(godot-rust). This is the recommended pattern for browser-based multiplayer
in Godot.

!!! note "Feature gate"
    This example requires the `transport-websocket-emscripten` feature and
    the `wasm32-unknown-emscripten` target. It cannot be run with
    `cargo run --example` — it must be compiled as part of a GDExtension
    library. See the [WebAssembly Guide](wasm.md) for full build
    instructions.

### Cargo.toml

```toml
[package]
name = "my-godot-game"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
godot = "0.3"
signal-fish-client = { version = "0.6.0", default-features = false, features = ["transport-websocket-emscripten"] }
serde_json = "1.0"  # Required for send_game_data(serde_json::Value)
```

### Source

```rust,ignore
use godot::prelude::*;
use signal_fish_client::{
    EmscriptenWebSocketTransport, JoinRoomParams,
    SignalFishConfig, SignalFishEvent, SignalFishPollingClient,
};

#[derive(GodotClass)]
#[class(base=Node)]
struct SignalFishNode {
    base: Base<Node>,
    client: Option<SignalFishPollingClient<EmscriptenWebSocketTransport>>,
}

#[godot_api]
impl INode for SignalFishNode {
    fn init(base: Base<Node>) -> Self {
        Self {
            base,
            client: None,
        }
    }

    fn ready(&mut self) {
        let transport = EmscriptenWebSocketTransport::connect("wss://server/ws")
            .expect("WebSocket creation failed");

        let config = SignalFishConfig::new("mb_app_abc123");
        self.client = Some(SignalFishPollingClient::new(transport, config));

        godot_print!("Signal Fish client initialized");
    }

    fn process(&mut self, _delta: f64) {
        let Some(client) = &mut self.client else { return };

        for event in client.poll() {
            match event {
                SignalFishEvent::Connected => {
                    godot_print!("Transport connected");
                }
                SignalFishEvent::Authenticated { app_name, .. } => {
                    godot_print!("Authenticated as {}", app_name);
                    let params = JoinRoomParams::new("my-game", "WebPlayer")
                        .with_max_players(4);
                    if let Err(e) = client.join_room(params) {
                        godot_print!("Failed to join room: {e}");
                    }
                }
                SignalFishEvent::RoomJoined { room_code, player_id, .. } => {
                    godot_print!("Joined room {} as {}", room_code, player_id);
                }
                SignalFishEvent::GameData { data, from_player, .. } => {
                    godot_print!("Game data from {}: {}", from_player, data);
                }
                SignalFishEvent::Disconnected { reason } => {
                    godot_print!(
                        "Disconnected: {}",
                        reason.as_deref().unwrap_or("unknown")
                    );
                    self.client = None;
                    return;
                }
                _ => {}
            }
        }
    }
}
```

### Step-by-step walkthrough

#### 1. Define the GDExtension node

```rust,ignore
#[derive(GodotClass)]
#[class(base=Node)]
struct SignalFishNode {
    base: Base<Node>,
    client: Option<SignalFishPollingClient<EmscriptenWebSocketTransport>>,
}
```

The `client` field is `Option` because the transport connection is
established in `ready()`, not at construction time. The generic parameter
`EmscriptenWebSocketTransport` satisfies the `Transport` bound.

#### 2. Connect in `ready()`

```rust,ignore
fn ready(&mut self) {
    let transport = EmscriptenWebSocketTransport::connect("wss://server/ws")
        .expect("WebSocket creation failed");
    let config = SignalFishConfig::new("mb_app_abc123");
    self.client = Some(SignalFishPollingClient::new(transport, config));
}
```

`EmscriptenWebSocketTransport::connect` is **synchronous** — no `.await`
needed. The polling client constructor queues an `Authenticate` message
automatically.

#### 3. Poll in `process()`

```rust,ignore
fn process(&mut self, _delta: f64) {
    let Some(client) = &mut self.client else { return };
    for event in client.poll() {
        // handle events
    }
}
```

Godot calls `_process` once per frame. `poll()` drains incoming messages,
flushes outgoing commands, and returns all events as a `Vec<SignalFishEvent>`.
When idle, `poll()` returns an empty vec — it is designed to be cheap.

#### 4. Build for web export

```sh
cargo +nightly build -Zbuild-std \
    --target wasm32-unknown-emscripten \
    --no-default-features \
    --features transport-websocket-emscripten \
    --release
```

The resulting `.wasm` file is used by Godot's HTML5 export template.
