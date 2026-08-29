//! Common synchronous API implemented by both Signal Fish client drivers.

use crate::client::{ClientSnapshot, ClientStats, GameDataDelivery, JoinRoomParams, RoomRole};
use crate::error::Result;
use crate::protocol::{
    ConnectionInfo, GameDataEncoding, PlayerId, RoomId, SessionGeneration, Topology, TransportKind,
};
use crate::signal::PeerSignal;
use crate::transport::TransportDiagnostics;

/// Object-safe synchronous command and state surface shared by both clients.
///
/// Use this trait when application logic should be independent of whether the
/// connection is driven by `SignalFishClient` or `SignalFishPollingClient`.
/// Driver-specific
/// operations such as async waiting sends, `shutdown`, `poll`, and `close` are
/// intentionally excluded; `transport_diagnostics` is shared and therefore
/// included.
///
/// Signal methods here take a concrete [`PeerSignal`] rather than
/// `impl Into<PeerSignal>` because object safety forbids generic parameters;
/// the inherent driver methods keep the ergonomic conversion, and the
/// defaulted `send_offer`/`send_answer`/`send_ice_candidate` helpers accept
/// raw SDP/candidate strings directly.
pub trait SignalFishClientApi {
    /// Join or create a room.
    fn join_room(&mut self, params: JoinRoomParams) -> Result<()>;
    /// Leave the current room.
    fn leave_room(&mut self) -> Result<()>;
    /// Send wire-reliable JSON game data.
    fn send_game_data(&mut self, data: serde_json::Value) -> Result<()>;
    /// Send JSON game data with a selected delivery class.
    fn send_game_data_with_delivery(
        &mut self,
        data: serde_json::Value,
        delivery: GameDataDelivery,
    ) -> Result<()>;
    /// Send opaque binary game data.
    fn send_binary_game_data(&mut self, payload: Vec<u8>) -> Result<()>;
    /// Mark the local player ready.
    ///
    /// The wire `PlayerReady` message toggles readiness: call exactly once
    /// per room membership, since a repeated call un-readies the player.
    fn set_ready(&mut self) -> Result<()>;
    /// Request the protocol-v2 game start.
    fn start_game(&mut self) -> Result<()>;
    /// Request or relinquish authority.
    fn request_authority(&mut self, become_authority: bool) -> Result<()>;
    /// Provide peer connection information.
    fn provide_connection_info(&mut self, connection_info: ConnectionInfo) -> Result<()>;
    /// Reconnect using a server-issued token.
    fn reconnect(&mut self, player_id: PlayerId, room_id: RoomId, auth_token: String)
        -> Result<()>;
    /// Join a room as a spectator.
    fn join_as_spectator(
        &mut self,
        game_name: String,
        room_code: String,
        spectator_name: String,
    ) -> Result<()>;
    /// Leave spectator mode.
    fn leave_spectator(&mut self) -> Result<()>;
    /// Send an application heartbeat.
    fn ping(&mut self) -> Result<()>;
    /// Relay a typed WebRTC signal.
    fn send_signal(&mut self, to: PlayerId, signal: PeerSignal) -> Result<()>;
    /// Relay a typed WebRTC signal only while its originating generation remains current.
    fn send_signal_for_generation(
        &mut self,
        to: PlayerId,
        generation: Option<SessionGeneration>,
        signal: PeerSignal,
    ) -> Result<()>;
    /// Relay an unmodeled WebRTC signal.
    fn send_raw_signal(&mut self, to: PlayerId, signal: serde_json::Value) -> Result<()>;
    /// Relay an unmodeled signal only while its originating generation remains current.
    fn send_raw_signal_for_generation(
        &mut self,
        to: PlayerId,
        generation: Option<SessionGeneration>,
        signal: serde_json::Value,
    ) -> Result<()>;
    /// Report data-path transport status.
    fn report_transport_status(&mut self, transport: TransportKind, connected: bool) -> Result<()>;
    /// Remaining command-queue capacity.
    #[must_use = "this diagnostic view is discarded if not used"]
    fn send_capacity(&self) -> usize;
    /// Configured command-queue capacity.
    #[must_use = "this diagnostic view is discarded if not used"]
    fn max_send_capacity(&self) -> usize;
    /// Cumulative traffic statistics.
    #[must_use = "this diagnostic view is discarded if not used"]
    fn stats(&self) -> ClientStats;
    /// Coherent connection and room snapshot.
    #[must_use = "this diagnostic view is discarded if not used"]
    fn snapshot(&self) -> ClientSnapshot;
    /// Most recent backend buffering/admission diagnostics sample.
    ///
    /// Reads the same per-I/O-step sample as the inherent driver methods; see
    /// [`TransportDiagnostics`](crate::TransportDiagnostics) for field
    /// semantics.
    #[must_use = "this diagnostic view is discarded if not used"]
    fn transport_diagnostics(&self) -> TransportDiagnostics;

    /// Server-confirmed local role in the current room.
    fn room_role(&self) -> Option<RoomRole> {
        self.snapshot().room_role
    }

    /// Whether the client owns a nonterminal transport connection.
    fn is_connected(&self) -> bool {
        self.snapshot().connected
    }

    /// Whether the transport handshake has completed.
    fn is_transport_ready(&self) -> bool {
        self.snapshot().transport_ready
    }

    /// Whether authentication has completed.
    fn is_authenticated(&self) -> bool {
        self.snapshot().authenticated
    }

    /// Negotiated v3-or-newer protocol version.
    fn negotiated_protocol_version(&self) -> Option<u16> {
        self.snapshot().negotiated_protocol_version
    }

    /// Exact game-data preference supplied in [`SignalFishConfig`](crate::SignalFishConfig).
    fn requested_game_data_format(&self) -> Option<GameDataEncoding> {
        self.snapshot().requested_game_data_format
    }

    /// Server-selected game-data format, or `None` outside a negotiated
    /// connection.
    fn effective_game_data_format(&self) -> Option<GameDataEncoding> {
        self.snapshot().effective_game_data_format
    }

    /// Whether WebRTC plus a P2P topology were advertised and v3 was negotiated.
    /// This reports capability, not the selected session plan.
    fn supports_mesh(&self) -> bool;

    /// Topology selected by the latest authoritative session plan.
    fn session_topology(&self) -> Option<Topology> {
        self.snapshot().session_topology
    }

    /// Data-path transport selected by the latest authoritative session plan.
    fn session_transport(&self) -> Option<TransportKind> {
        self.snapshot().session_transport
    }

    /// Whether the latest authoritative plan selects a peer-to-peer topology.
    fn is_p2p_active(&self) -> bool {
        matches!(
            self.session_topology(),
            Some(Topology::Host | Topology::Mesh)
        )
    }

    /// Send an SDP offer.
    fn send_offer(&mut self, to: PlayerId, sdp: String) -> Result<()> {
        self.send_signal(to, PeerSignal::Offer(sdp))
    }

    /// Send an SDP answer.
    fn send_answer(&mut self, to: PlayerId, sdp: String) -> Result<()> {
        self.send_signal(to, PeerSignal::Answer(sdp))
    }

    /// Send a trickle ICE candidate.
    fn send_ice_candidate(&mut self, to: PlayerId, candidate: String) -> Result<()> {
        self.send_signal(to, PeerSignal::IceCandidate(candidate))
    }
}
