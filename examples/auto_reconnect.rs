//! # Automatic Reconnection Example
//!
//! Opt-in reconnection ([`SignalFishConfig::with_reconnect_policy`]) rebuilds
//! the connection from a caller-supplied **factory**: a synchronous function
//! that returns a fresh, *unconnected* transport per attempt. The built-in
//! [`WebSocketTransport::connect_lazy`] constructor exists exactly for this:
//! it returns the transport immediately and starts the WebSocket handshake on
//! the first driver poll, with a built-in 10-second handshake deadline so a
//! server that never completes the upgrade is observed as a retryable
//! failure instead of a hang.
//!
//! ## Running
//!
//! ```sh
//! # Start a Signal Fish server on localhost:3536, then:
//! cargo run --example auto_reconnect
//!
//! # Override the server URL:
//! SIGNAL_FISH_URL=ws://my-server:3536/v2/ws cargo run --example auto_reconnect
//! ```
//!
//! Try killing and restarting the server while the example runs: every
//! retryable disconnect prints a `Reconnecting` event and the client
//! re-authenticates and re-joins its room automatically.
//!
//! On protocol v3 (`.enable_v3()`) the driver additionally auto-rejoins a
//! player room after reconnecting; drop this template's
//! `join_room`-on-`Authenticated` call in that configuration, or the seat is
//! requested twice per round.

use signal_fish_client::{
    JoinRoomParams, ReconnectPolicy, SignalFishClient, SignalFishConfig, SignalFishEvent,
    WebSocketTransport,
};

/// Default server URL when `SIGNAL_FISH_URL` is not set.
const DEFAULT_URL: &str = "ws://localhost:3536/v2/ws";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let url = std::env::var("SIGNAL_FISH_URL").unwrap_or_else(|_| DEFAULT_URL.to_string());

    // The factory is called at most once per reconnect attempt and must not
    // block. `connect_lazy` returns a fresh, *unconnected* transport
    // immediately; the driver starts its handshake on the first poll.
    let factory_url = url.clone();
    let config = SignalFishConfig::new("mb_app_reconnect_example").with_reconnect_policy(
        ReconnectPolicy::new(move || {
            Box::new(WebSocketTransport::connect_lazy(factory_url.clone()))
        }),
    );

    // The initial transport is lazy the same way; the driver connects it on
    // its first loop iteration.
    let initial = WebSocketTransport::connect_lazy(url);
    let (mut client, mut events) = SignalFishClient::start(initial, config);

    let mut joined = false;
    while let Some(event) = events.recv().await {
        match event {
            SignalFishEvent::Authenticated { .. } => {
                println!("authenticated; joining the room");
                client.join_room(JoinRoomParams::new("reconnect-demo", "Alice"))?;
            }
            SignalFishEvent::RoomJoined { room_code, .. } => {
                joined = true;
                println!("joined room (presence only: {} bytes)", room_code.len());
                client.set_ready()?;
            }
            SignalFishEvent::Reconnecting {
                attempt,
                next_backoff,
            } => {
                joined = false;
                println!(
                    "connection lost; reconnect attempt {attempt} in {:.1?}",
                    next_backoff
                );
            }
            SignalFishEvent::ReconnectAbandoned { attempts, .. } => {
                println!("server unreachable after {attempts} attempts; giving up");
                break;
            }
            SignalFishEvent::Disconnected { .. } => {
                if !joined {
                    // Every retryable round delivers `Disconnected` first —
                    // including one that a `Reconnecting` event immediately
                    // follows — so this arm stays quiet mid-room to avoid
                    // noise, and only speaks up when no room membership was
                    // active.
                    println!("disconnected without an active room membership");
                }
            }
            SignalFishEvent::PlayerJoined { player, .. } => {
                println!("player {} joined", player.id);
            }
            SignalFishEvent::Error {
                message,
                error_code,
            } => {
                println!("server error ({error_code:?}): {message}");
            }
            _ => {}
        }
    }

    client.shutdown().await;
    Ok(())
}
