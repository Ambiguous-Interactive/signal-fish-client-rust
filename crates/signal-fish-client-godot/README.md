# Signal Fish Client for Godot

`signal-fish-client-godot` provides the main-thread Godot 4.5
`WebSocketPeer` transport adapter for the framed-transport-agnostic
[`signal-fish-client`](https://crates.io/crates/signal-fish-client) SDK.

The adapter is versioned in lockstep with the core crate. See the
[Signal Fish Client guide](https://Ambiguous-Interactive.github.io/signal-fish-client-rust/)
for setup and migration instructions.

## Requirements

| Requirement | Version |
|-------------|---------|
| Rust (MSRV) | 1.94.0 |
| godot-rust (`godot` crate) | `>=0.4.5, <0.6` — pick one exact version so your `Gd<WebSocketPeer>` shares the adapter's type identity |
| Godot Engine | 4.5, native or official web export |

Add both crates as direct dependencies — the Quick Start imports
`signal_fish_client` types directly:

```toml
[dependencies]
signal-fish-client = { version = "0.10", default-features = false, features = ["polling-client"] }
signal-fish-client-godot = { version = "0.10" }
```

## Quick start

Drive the polling client from a Node's `_process` callback:

```rust,ignore
use godot::classes::Node;
use godot::prelude::*;
use signal_fish_client::{JoinRoomParams, SignalFishConfig, SignalFishEvent};
use signal_fish_client::polling_client::SignalFishPollingClient;
use signal_fish_client_godot::GodotWebSocketTransport;

#[derive(GodotClass)]
#[class(base = Node)]
struct SignalFishNode {
    base: Base<Node>,
    client: Option<SignalFishPollingClient<GodotWebSocketTransport>>,
}

#[godot_api]
impl INode for SignalFishNode {
    fn ready(&mut self) {
        let transport = GodotWebSocketTransport::connect("wss://example.com/v2/ws")
            .expect("failed to create WebSocket");
        let config = SignalFishConfig::new("mb_app_abc123");
        self.client = Some(SignalFishPollingClient::new(transport, config));
    }

    fn process(&mut self, _delta: f64) {
        let Some(client) = &mut self.client else { return };
        for event in client.poll() {
            if let SignalFishEvent::Authenticated { .. } = event {
                let _ = client.join_room(JoinRoomParams::new("my-game", "GodotPlayer"));
            }
        }
    }
}
```

The `expect` keeps the example short; surface the error to your UI and retry
instead of panicking in production code.

SDK-created peers raise Godot's inbound buffer to 8 MiB and its independent
queued-packet cap from 4,096 to 65,536 before connecting. Godot's native and web
backends can silently drop newly arriving frames when either limit fills, so
applications with unusually large bursts of tiny frames should construct and
configure their own peer. Outbound keeps Godot's legacy 65,535-byte default:
keep single game payloads under ~64 KiB, or construct your own peer with a
raised outbound buffer and wrap it with
[`GodotWebSocketTransport::from_peer`].
