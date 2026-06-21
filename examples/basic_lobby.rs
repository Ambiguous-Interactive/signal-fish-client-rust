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

    // ── Lobby bookkeeping ───────────────────────────────────────────
    // The server can emit many `LobbyStateChanged` updates while readiness
    // stays true, and in an authority room only the authority may start the
    // game. We track just enough state to start the game exactly once, and only
    // when we're actually allowed to — otherwise the example would spam
    // `StartGame` and collect `GameStartNotReady` / `GameStartForbidden` errors.
    // The decision depends on three inputs that arrive from different events
    // (readiness, authority, and the one-shot latch), so it is centralized in
    // `maybe_start_game` and re-evaluated whenever any input changes.
    let mut supports_authority = false;
    let mut is_authority = false;
    let mut all_ready = false;
    let mut game_start_requested = false;

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
                        supports_authority: room_supports_authority,
                        is_authority: locally_authority,
                        ..
                    } => {
                        tracing::info!(
                            "Joined room {room_code} as player {player_id} \
                             ({} player(s) present)",
                            current_players.len()
                        );

                        // Remember who may start the game in this room. Reset the
                        // readiness and one-shot start latch for the fresh room.
                        supports_authority = room_supports_authority;
                        is_authority = locally_authority;
                        all_ready = false;
                        game_start_requested = false;

                        // Mark ourselves as ready.
                        client.set_ready()?;
                        tracing::info!("Set ready");
                    }

                    // ── Reconnected: adopt the server's authoritative state ──
                    SignalFishEvent::Reconnected {
                        supports_authority: room_supports_authority,
                        is_authority: locally_authority,
                        current_players,
                        ..
                    } => {
                        tracing::info!("Reconnected to room");
                        // The payload carries the server's current truth, so adopt
                        // it directly instead of waiting for a follow-up event.
                        // We deliberately do NOT reset `game_start_requested`: this
                        // is the same session, so if we already started the game we
                        // must not send a second `StartGame`.
                        supports_authority = room_supports_authority;
                        is_authority = locally_authority;
                        all_ready = !current_players.is_empty()
                            && current_players.iter().all(|p| p.is_ready);
                        maybe_start_game(
                            &client,
                            supports_authority,
                            is_authority,
                            all_ready,
                            &mut game_start_requested,
                        )?;
                    }

                    SignalFishEvent::PlayerJoined { player } => {
                        tracing::info!("Player joined: {} ({})", player.name, player.id);
                    }

                    SignalFishEvent::PlayerLeft { player_id } => {
                        tracing::info!("Player left: {player_id}");
                    }

                    SignalFishEvent::LobbyStateChanged {
                        lobby_state,
                        all_ready: ready,
                        ..
                    } => {
                        tracing::info!("Lobby state → {lobby_state:?} (all_ready={ready})");
                        all_ready = ready;
                        maybe_start_game(
                            &client,
                            supports_authority,
                            is_authority,
                            all_ready,
                            &mut game_start_requested,
                        )?;
                    }

                    // ── Authority changes ────────────────────────────
                    // In an authority room, who may start can change mid-lobby —
                    // e.g. we become the authority *after* everyone is already
                    // ready, in which case no new `LobbyStateChanged` arrives, so
                    // we must re-evaluate the start decision here too.
                    SignalFishEvent::AuthorityChanged {
                        you_are_authority, ..
                    } => {
                        is_authority = you_are_authority;
                        tracing::info!("Authority changed (you_are_authority={you_are_authority})");
                        maybe_start_game(
                            &client,
                            supports_authority,
                            is_authority,
                            all_ready,
                            &mut game_start_requested,
                        )?;
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

/// Send `StartGame` exactly once, and only when this client is allowed to.
///
/// Protocol v2+ no longer auto-starts the game on readiness — someone must
/// explicitly start it. In an authority room only the authority may start;
/// without authority delegation any player may. `game_start_requested` is a
/// one-shot latch so repeated `LobbyStateChanged` / `AuthorityChanged` events
/// (which fire while everyone stays ready) never send a second `StartGame`.
fn maybe_start_game(
    client: &SignalFishClient,
    supports_authority: bool,
    is_authority: bool,
    all_ready: bool,
    game_start_requested: &mut bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !all_ready || *game_start_requested {
        return Ok(());
    }
    if supports_authority && !is_authority {
        tracing::info!("All players ready — waiting for the authority to start");
        return Ok(());
    }
    client.start_game()?;
    *game_start_requested = true;
    tracing::info!("All players ready — start requested");
    Ok(())
}
