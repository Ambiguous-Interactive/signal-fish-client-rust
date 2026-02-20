//! # Basic Lobby Example
//!
//! Demonstrates a complete Signal Fish client lifecycle:
//!
//! 1. Connect to a signaling server via WebSocket
//! 2. Authenticate with an App ID
//! 3. Join a room
//! 4. React to lobby events (players joining, ready state, game starting)
//! 5. Shut down gracefully on Ctrl+C or disconnect
//!
//! ## Running
//!
//! ```sh
//! # Start a Signal Fish server on localhost:3536, then:
//! cargo run --example basic_lobby
//!
//! # Override the server URL:
//! SIGNAL_FISH_URL=ws://my-server:3536/ws cargo run --example basic_lobby
//! ```

use signal_fish_client::{
    JoinRoomParams, SignalFishClient, SignalFishConfig, SignalFishEvent, WebSocketTransport,
};

/// Default server URL when `SIGNAL_FISH_URL` is not set.
const DEFAULT_URL: &str = "ws://localhost:3536/ws";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── Logging ─────────────────────────────────────────────────────
    // Initialize tracing. Set `RUST_LOG=debug` for verbose output.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // ── Configuration ───────────────────────────────────────────────
    let url = std::env::var("SIGNAL_FISH_URL").unwrap_or_else(|_| DEFAULT_URL.to_string());
    tracing::info!("Connecting to {url}");

    // ── Connect ─────────────────────────────────────────────────────
    // Establish a WebSocket connection to the signaling server.
    let transport = WebSocketTransport::connect(&url).await?;

    // Build client config with your application ID.
    // Replace "mb_app_abc123" with your actual app ID.
    let config = SignalFishConfig::new("mb_app_abc123");

    // Start the client. This spawns a background task that drives the
    // transport and emits events on `event_rx`.
    let (mut client, mut event_rx) = SignalFishClient::start(transport, config);

    // ── Event loop ──────────────────────────────────────────────────
    // Use `tokio::select!` to listen for both server events and Ctrl+C.
    loop {
        tokio::select! {
            // Branch 1: Incoming event from the server (or transport layer).
            event = event_rx.recv() => {
                let Some(event) = event else {
                    // Channel closed — transport loop exited.
                    tracing::info!("Event channel closed, exiting");
                    break;
                };

                match event {
                    // ── Synthetic: transport connected ───────────────
                    SignalFishEvent::Connected => {
                        tracing::info!("Transport connected, awaiting authentication…");
                    }

                    // ── Authentication succeeded ─────────────────────
                    SignalFishEvent::Authenticated { app_name, .. } => {
                        tracing::info!("Authenticated as app: {app_name}");

                        // Now that we're authenticated, join a room.
                        let params = JoinRoomParams::new("example-game", "RustPlayer")
                            .with_max_players(4);

                        client.join_room(params)?;
                        tracing::info!("Join-room request sent");
                    }

                    // ── Room lifecycle ────────────────────────────────
                    SignalFishEvent::RoomJoined {
                        room_code,
                        player_id,
                        current_players,
                        ..
                    } => {
                        tracing::info!(
                            "Joined room {room_code} as player {player_id} \
                             ({} player(s) present)",
                            current_players.len()
                        );

                        // Mark ourselves as ready.
                        client.set_ready()?;
                        tracing::info!("Set ready");
                    }

                    SignalFishEvent::PlayerJoined { player } => {
                        tracing::info!("Player joined: {} ({})", player.name, player.id);
                    }

                    SignalFishEvent::PlayerLeft { player_id } => {
                        tracing::info!("Player left: {player_id}");
                    }

                    SignalFishEvent::LobbyStateChanged {
                        lobby_state,
                        all_ready,
                        ..
                    } => {
                        tracing::info!("Lobby state → {lobby_state:?} (all_ready={all_ready})");
                    }

                    SignalFishEvent::GameStarting {
                        peer_connections,
                    } => {
                        tracing::info!(
                            "Game starting with {} peer connection(s)!",
                            peer_connections.len()
                        );
                    }

                    // ── Errors from the server ───────────────────────
                    SignalFishEvent::AuthenticationError { error, error_code } => {
                        tracing::error!("Auth failed [{error_code:?}]: {error}");
                        break;
                    }

                    SignalFishEvent::Error { message, error_code } => {
                        tracing::error!("Server error [{error_code:?}]: {message}");
                    }

                    // ── Disconnect ───────────────────────────────────
                    SignalFishEvent::Disconnected { reason } => {
                        tracing::warn!("Disconnected: {}", reason.as_deref().unwrap_or("unknown"));
                        break;
                    }

                    // ── Catch-all ────────────────────────────────────
                    other => {
                        tracing::debug!("Event: {other:?}");
                    }
                }
            }

            // Branch 2: Ctrl+C — shut down gracefully.
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Ctrl+C received, shutting down…");
                break;
            }
        }
    }

    // ── Cleanup ─────────────────────────────────────────────────────
    client.shutdown().await;
    tracing::info!("Client shut down. Goodbye!");
    Ok(())
}
