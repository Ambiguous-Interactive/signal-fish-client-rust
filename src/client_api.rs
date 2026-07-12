//! Common synchronous API implemented by both Signal Fish client drivers.

use crate::client::{ClientSnapshot, ClientStats, GameDataDelivery, JoinRoomParams};
use crate::error::Result;
use crate::protocol::{ConnectionInfo, PlayerId, RoomId, TransportKind};
use crate::signal::PeerSignal;

/// Object-safe synchronous command and state surface shared by both clients.
///
/// Use this trait when application logic should be independent of whether the
/// connection is driven by `SignalFishClient` or `SignalFishPollingClient`.
/// Driver-specific
/// operations such as async waiting sends, `shutdown`, `poll`, and `close` are
/// intentionally excluded.
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
    /// Relay an unmodeled WebRTC signal.
    fn send_raw_signal(&mut self, to: PlayerId, signal: serde_json::Value) -> Result<()>;
    /// Report data-path transport status.
    fn report_transport_status(&mut self, transport: TransportKind, connected: bool) -> Result<()>;
    /// Remaining command-queue capacity.
    fn send_capacity(&self) -> usize;
    /// Configured command-queue capacity.
    fn max_send_capacity(&self) -> usize;
    /// Cumulative traffic statistics.
    fn stats(&self) -> ClientStats;
    /// Coherent connection and room snapshot.
    fn snapshot(&self) -> ClientSnapshot;

    /// Whether the physical connection is active.
    fn is_connected(&self) -> bool {
        self.snapshot().connected
    }

    /// Whether authentication has completed.
    fn is_authenticated(&self) -> bool {
        self.snapshot().authenticated
    }

    /// Negotiated v3-or-newer protocol version.
    fn negotiated_protocol_version(&self) -> Option<u16> {
        self.snapshot().negotiated_protocol_version
    }

    /// Whether WebRTC mesh was advertised and protocol v3 was negotiated.
    fn supports_mesh(&self) -> bool;

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
