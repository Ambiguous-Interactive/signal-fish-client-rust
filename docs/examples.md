# Basic Lobby Walkthrough

The repository's complete, compiling lobby example connects to a Signal Fish
server, joins a room, reacts to lobby events, and shuts down cleanly. This page
explains that flow without duplicating its source.

**Source:**
[`examples/basic_lobby.rs`](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/blob/main/examples/basic_lobby.rs)

!!! info "Protocol v2 relay"
    The example uses the default protocol-v2 relay configuration. Get this path
    working before choosing protocol v3 or WebRTC mesh.

## Run it

From a repository checkout:

```sh
cargo run --example basic_lobby
```

The default server URL is `ws://localhost:3536/v2/ws`. Override it when your
server runs elsewhere:

```sh
SIGNAL_FISH_URL=ws://my-server:3536/v2/ws cargo run --example basic_lobby
```

Need a server? The Signal Fish Server
[five-minute quick start](https://ambiguous-interactive.github.io/signal-fish-server/quickstart/)
provides a development setup.

## What the example does

### 1. Connect and start

```rust,ignore
tracing::info!("connecting…");
let transport = WebSocketTransport::connect(&url).await?;
let config = SignalFishConfig::new("mb_app_abc123");
let (mut client, mut events) = SignalFishClient::start(transport, config);
```

The App ID is a public application label, not a secret. An open development
server accepts a test label; a managed or production server may restrict the
allowed labels.

Starting the client queues authentication and returns a command handle plus an
event receiver. Keep receiving events while the client is active.

### 2. Wait for authentication

```rust,ignore
SignalFishEvent::Authenticated { app_name, .. } => {
    let params = JoinRoomParams::new("example-game", "RustPlayer")
        .with_max_players(4);
    client.join_room(params)?;
}
```

Room commands are valid only after `Authenticated`. Authentication failures
arrive as `AuthenticationError`; the example logs the server error code and
exits.

### 3. Join and become ready

```rust,ignore
SignalFishEvent::RoomJoined { room_code, .. } => {
    tracing::info!("joined room {room_code}");
    client.set_ready()?;
}
```

The example also reports players joining or leaving and observes
`LobbyStateChanged`. When everyone is ready, it sends one `start_game()`
request.

### 4. Handle connection state

The event loop reports `Connected`, errors, and disconnects. It also keeps the
authority/start-request state correct if a `Reconnected` event arrives. The
example exits on `Disconnected`; it does not initiate a reconnect itself.

### 5. Shut down

```rust,ignore
client.shutdown().await;
```

Pressing Ctrl+C leaves the event loop and asks the transport to close cleanly.
Shutdown waits up to `SignalFishConfig::shutdown_timeout`, then uses the
transport abort path if needed.

## Advanced examples

After the lobby flow works, use these compiling repository examples with the
canonical guide for that subsystem:

- [`custom_transport.rs`](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/blob/main/examples/custom_transport.rs)
  with the [custom transport guide](transport.md)
- [`mesh_session.rs`](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/blob/main/examples/mesh_session.rs)
  with the [mesh guide](mesh-guide.md)
- [`load_lab.rs`](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/blob/main/examples/load_lab.rs)
  with the [delivery and backpressure guide](delivery.md)
- the complete [`tests/godot-web-smoke`](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/tree/main/tests/godot-web-smoke)
  fixture with the [Godot and WebAssembly guide](wasm.md)

The focused guides own platform setup and protocol contracts so those details
have one maintained source of truth.
