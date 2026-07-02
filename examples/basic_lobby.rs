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

use signal_fish_client::protocol::LobbyState;
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
    let mut lobby_start = LobbyStartState::default();

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
                        lobby_start.reset_for_room(room_supports_authority, locally_authority);

                        // Mark ourselves as ready.
                        client.set_ready()?;
                        tracing::info!("Set ready");
                    }

                    // ── Reconnected: adopt the server's authoritative state ──
                    SignalFishEvent::Reconnected {
                        supports_authority: room_supports_authority,
                        is_authority: locally_authority,
                        current_players,
                        ready_players,
                        lobby_state,
                        missed_events,
                        ..
                    } => {
                        tracing::info!("Reconnected to room");
                        // The payload carries the server's current truth, so adopt
                        // readiness and authority directly. Historical missed
                        // events can only confirm the game already started.
                        let current_all_ready = !current_players.is_empty()
                            && current_players
                                .iter()
                                .all(|p| p.is_ready || ready_players.contains(&p.id));
                        lobby_start.adopt_reconnected_state(
                            room_supports_authority,
                            locally_authority,
                            current_all_ready,
                            &lobby_state,
                            &missed_events,
                        );
                        lobby_start.maybe_start_game(&client)?;
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
                        lobby_start.apply_lobby_state(&lobby_state, ready);
                        lobby_start.maybe_start_game(&client)?;
                    }

                    // ── Authority changes ────────────────────────────
                    // In an authority room, who may start can change mid-lobby —
                    // e.g. we become the authority *after* everyone is already
                    // ready, in which case no new `LobbyStateChanged` arrives, so
                    // we must re-evaluate the start decision here too.
                    SignalFishEvent::AuthorityChanged {
                        you_are_authority, ..
                    } => {
                        lobby_start.apply_authority_changed(you_are_authority);
                        tracing::info!("Authority changed (you_are_authority={you_are_authority})");
                        lobby_start.maybe_start_game(&client)?;
                    }

                    SignalFishEvent::GameStarting {
                        peer_connections,
                    } => {
                        lobby_start.mark_game_starting();
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
                    SignalFishEvent::Disconnected { reason, .. } => {
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

#[derive(Debug, Default)]
struct LobbyStartState {
    supports_authority: bool,
    is_authority: bool,
    all_ready: bool,
    start_request_sent: bool,
    game_start_confirmed: bool,
}

impl LobbyStartState {
    fn reset_for_room(&mut self, supports_authority: bool, is_authority: bool) {
        self.supports_authority = supports_authority;
        self.is_authority = is_authority;
        self.all_ready = false;
        self.start_request_sent = false;
        self.game_start_confirmed = false;
    }

    /// Adopt the reconnect snapshot as the current room truth.
    ///
    /// `missed_events` are historical, so they must not override current
    /// readiness or authority from the snapshot. We only use them to notice a
    /// terminal game-start/finalized event that happened while offline.
    fn adopt_reconnected_state(
        &mut self,
        supports_authority: bool,
        is_authority: bool,
        all_ready: bool,
        lobby_state: &LobbyState,
        missed_events: &[SignalFishEvent],
    ) {
        self.supports_authority = supports_authority;
        self.is_authority = is_authority;
        self.all_ready = all_ready;
        self.start_request_sent = false;
        self.game_start_confirmed = Self::is_terminal_start_state(lobby_state)
            || missed_events.iter().any(Self::is_terminal_start_event);
    }

    fn apply_lobby_state(&mut self, lobby_state: &LobbyState, all_ready: bool) {
        self.all_ready = all_ready;
        if Self::is_terminal_start_state(lobby_state) {
            self.mark_game_starting();
        }
    }

    fn apply_authority_changed(&mut self, you_are_authority: bool) {
        self.is_authority = you_are_authority;
    }

    fn mark_game_starting(&mut self) {
        self.game_start_confirmed = true;
    }

    fn is_terminal_start_state(lobby_state: &LobbyState) -> bool {
        matches!(lobby_state, LobbyState::Finalized)
    }

    fn is_terminal_start_event(event: &SignalFishEvent) -> bool {
        match event {
            SignalFishEvent::GameStarting { .. } => true,
            SignalFishEvent::LobbyStateChanged { lobby_state, .. } => {
                Self::is_terminal_start_state(lobby_state)
            }
            _ => false,
        }
    }

    fn should_start_game(&self) -> bool {
        self.all_ready
            && !self.start_request_sent
            && !self.game_start_confirmed
            && (!self.supports_authority || self.is_authority)
    }

    /// Send `StartGame` exactly once, and only when this client is allowed to.
    ///
    /// Protocol v2+ no longer auto-starts the game on readiness. In an authority
    /// room only the authority may start; without authority delegation any player
    /// may. `start_request_sent` is a per-connection latch so repeated live lobby
    /// events do not spam `StartGame`; reconnect resets it unless the server
    /// confirms the game is already starting or finalized.
    fn maybe_start_game(
        &mut self,
        client: &SignalFishClient,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.should_start_game() {
            client.start_game()?;
            self.start_request_sent = true;
            tracing::info!("All players ready — start requested");
            return Ok(());
        }
        if self.all_ready
            && !self.game_start_confirmed
            && !self.start_request_sent
            && self.supports_authority
            && !self.is_authority
        {
            tracing::info!("All players ready — waiting for the authority to start");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use signal_fish_client::protocol::PeerConnectionInfo;

    #[test]
    fn reconnect_snapshot_ready_can_trigger_start_when_allowed() {
        let mut state = LobbyStartState::default();
        state.adopt_reconnected_state(false, false, true, &LobbyState::Lobby, &[]);

        assert!(state.should_start_game());
    }

    #[test]
    fn historical_ready_event_does_not_override_reconnect_snapshot() {
        let mut state = LobbyStartState::default();
        state.adopt_reconnected_state(
            false,
            false,
            false,
            &LobbyState::Lobby,
            &[SignalFishEvent::LobbyStateChanged {
                lobby_state: LobbyState::Lobby,
                ready_players: vec![],
                all_ready: true,
            }],
        );

        assert!(!state.should_start_game());
    }

    #[test]
    fn historical_authority_event_does_not_override_reconnect_snapshot() {
        let mut state = LobbyStartState::default();
        state.adopt_reconnected_state(
            true,
            false,
            true,
            &LobbyState::Lobby,
            &[SignalFishEvent::AuthorityChanged {
                authority_player: None,
                you_are_authority: true,
            }],
        );

        assert!(!state.should_start_game());
    }

    #[test]
    fn reconnect_snapshot_authority_can_enable_start() {
        let mut state = LobbyStartState::default();
        state.adopt_reconnected_state(true, true, true, &LobbyState::Lobby, &[]);
        assert!(state.should_start_game());
    }

    #[test]
    fn missed_game_starting_suppresses_duplicate_start() {
        let mut state = LobbyStartState::default();
        state.adopt_reconnected_state(
            false,
            false,
            true,
            &LobbyState::Lobby,
            &[SignalFishEvent::GameStarting {
                peer_connections: Vec::<PeerConnectionInfo>::new(),
            }],
        );

        assert!(!state.should_start_game());
    }

    #[test]
    fn finalized_reconnect_suppresses_duplicate_start() {
        let mut state = LobbyStartState::default();
        state.adopt_reconnected_state(false, false, true, &LobbyState::Finalized, &[]);

        assert!(!state.should_start_game());
    }

    #[test]
    fn reconnect_resets_unconfirmed_start_request() {
        let mut state = LobbyStartState::default();
        state.reset_for_room(false, false);
        state.apply_lobby_state(&LobbyState::Lobby, true);
        state.start_request_sent = true;
        assert!(!state.should_start_game());

        state.adopt_reconnected_state(false, false, true, &LobbyState::Lobby, &[]);

        assert!(state.should_start_game());
    }

    #[test]
    fn live_game_starting_latches_before_later_authority_change() {
        let mut state = LobbyStartState::default();
        state.reset_for_room(true, false);
        state.apply_lobby_state(&LobbyState::Lobby, true);
        assert!(!state.should_start_game());

        state.mark_game_starting();
        state.apply_authority_changed(true);

        assert!(!state.should_start_game());
    }
}
