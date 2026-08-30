//! # Mesh Session Example (protocol v3)
//!
//! Demonstrates the **batteries-included** mesh path: implement the
//! [`WebRtcDriver`] trait against your WebRTC stack, hand it to
//! [`MeshController`], and the SDK drives the entire signaling handshake for you
//! — obeying the server's `initiate` directives, relaying offers/answers/ICE,
//! reporting transport status, and surfacing a clean [`MeshEvent`] stream.
//!
//! This example is fully self-contained: a scripted in-process "server" plays
//! the canonical v3 flow (authenticate → negotiate v3 → join → ready →
//! finalize → authoritative session plan → relayed signal), and a tiny
//! in-memory driver completes the handshake, so the whole stack runs
//! end-to-end with no network. A pinned test runs this demo under a timeout,
//! so a protocol-flow regression here fails CI instead of hanging silently.
//!
//! In a real game, replace [`DemoDriver`] with a wrapper around a real WebRTC
//! backend (str0m, webrtc-rs, or the browser's `RTCPeerConnection` via web-sys)
//! — the integration points are marked with `// REAL DRIVER:` comments.
//!
//! ## Running
//!
//! ```sh
//! cargo run --example mesh_session --features mesh,tokio-runtime
//! ```

use std::collections::VecDeque;

use signal_fish_client::protocol::{
    GameDataEncoding, IceServer, LobbyState, MessageTransport, PlayerInfo, ProtocolInfoPayload,
    RoomJoinedPayload, SessionPeer, SessionPlanPayload, Topology, TransportKind,
};
use signal_fish_client::transport::TransportFrame;
use signal_fish_client::webrtc::{DriverEvent, MeshController, MeshEvent, WebRtcDriver};
use signal_fish_client::{
    ClientMessage, JoinRoomParams, PeerSignal, PlayerId, ServerMessage, SessionGeneration,
    SignalFishConfig, SignalFishError, SignalFishEvent, Transport,
};

// ─────────────────────────────────────────────────────────────────────
// Step 1: Implement WebRtcDriver against your WebRTC stack.
// ─────────────────────────────────────────────────────────────────────

/// A minimal in-memory driver that completes a handshake without real WebRTC.
///
/// It models the realistic shape: the initiator emits an offer on `connect`; the
/// answerer emits an answer (and "opens" the channel) when it receives an offer;
/// the initiator "opens" the channel when it receives the answer.
#[derive(Default)]
struct DemoDriver {
    outbox: VecDeque<DriverEvent>,
}

impl WebRtcDriver for DemoDriver {
    fn set_ice_servers(&mut self, servers: &[IceServer]) {
        // REAL DRIVER: configure your RTCPeerConnection with these STUN/TURN servers.
        println!("  driver: using {} ICE server(s)", servers.len());
    }

    fn connect(&mut self, peer: PlayerId, generation: Option<SessionGeneration>, initiate: bool) {
        // REAL DRIVER: create an RTCPeerConnection for `peer`. If `initiate`,
        // create an offer and surface it via `poll` as DriverEvent::Signal.
        println!("  driver: connect to {peer} (initiate={initiate})");
        if initiate {
            self.outbox.push_back(DriverEvent::Signal {
                peer,
                generation,
                signal: PeerSignal::Offer("<sdp-offer>".into()),
            });
        }
    }

    fn on_signal(
        &mut self,
        peer: PlayerId,
        generation: Option<SessionGeneration>,
        signal: PeerSignal,
    ) {
        // REAL DRIVER: apply the remote description / add the ICE candidate.
        println!("  driver: got {signal:?} from {peer}");
        match signal {
            PeerSignal::Offer(_) => {
                self.outbox.push_back(DriverEvent::Signal {
                    peer,
                    generation,
                    signal: PeerSignal::Answer("<sdp-answer>".into()),
                });
                self.outbox
                    .push_back(DriverEvent::Connected { peer, generation });
            }
            PeerSignal::Answer(_) => self
                .outbox
                .push_back(DriverEvent::Connected { peer, generation }),
            PeerSignal::IceCandidate(_) => {}
        }
    }

    fn send(&mut self, peer: PlayerId, data: &[u8]) {
        // REAL DRIVER: send `data` over `peer`'s data channel.
        println!("  driver: send {} bytes to {peer}", data.len());
    }

    fn disconnect(&mut self, peer: PlayerId) {
        // REAL DRIVER: close the RTCPeerConnection for `peer`.
        println!("  driver: disconnect {peer}");
    }

    fn poll(&mut self) -> Option<DriverEvent> {
        // REAL DRIVER: pump your WebRTC stack's I/O here and return outputs
        // (locally-produced signals, connection-state changes, received data).
        self.outbox.pop_front()
    }
}

// ─────────────────────────────────────────────────────────────────────
// Step 2: A scripted loopback transport that plays the v3 server side.
// ─────────────────────────────────────────────────────────────────────

/// The player id the scripted server assigns to the local player ("Alice").
const ALICE: uuid::Uuid = uuid::Uuid::from_u128(0xA);
/// The scripted peer ("Bob") that shares the room and the session plan.
const BOB: uuid::Uuid = uuid::Uuid::from_u128(0xB);
/// The authoritative session-plan generation the scripted server publishes.
const GENERATION: uuid::Uuid = uuid::Uuid::from_u128(0x7);

struct ScriptedServer {
    incoming: VecDeque<String>,
    plan_sent: bool,
    aborted: bool,
}

impl ScriptedServer {
    fn queue(&mut self, message: ServerMessage) {
        let frame = serde_json::to_string(&message).unwrap_or_default();
        self.incoming.push_back(frame);
    }

    /// Canonical, room-ordered join baseline: the server picks the local
    /// player id, the roster already contains the future plan peer, and the
    /// protocol-v3 roster stamps each player with a paired `epoch`/`seq`.
    fn room_joined(&mut self) {
        let player = |id: uuid::Uuid, name: &str, is_authority: bool| PlayerInfo {
            id,
            name: name.into(),
            is_authority,
            is_ready: true,
            connected_at: "2026-01-01T00:00:00Z".into(),
            connection_info: None,
            epoch: Some(1),
            seq: Some(0),
        };
        self.queue(ServerMessage::RoomJoined(Box::new(RoomJoinedPayload {
            room_id: uuid::Uuid::from_u128(0x100),
            room_code: "DEMO1".into(),
            player_id: ALICE,
            game_name: "demo-game".into(),
            max_players: 4,
            supports_authority: true,
            current_players: vec![player(ALICE, "Alice", true), player(BOB, "Bob", false)],
            is_authority: true,
            lobby_state: LobbyState::Lobby,
            ready_players: vec![],
            relay_type: "websocket".into(),
            current_spectators: vec![],
            ice_servers: vec![],
            reconnection_token: None,
        })));
    }

    fn session_plan(&mut self) {
        self.queue(ServerMessage::SessionPlan(Box::new(SessionPlanPayload {
            generation: Some(GENERATION),
            topology: Topology::Mesh,
            transport: TransportKind::WebRtc,
            host: None,
            direct_endpoint: None,
            peers: vec![SessionPeer {
                player_id: BOB,
                player_name: "Bob".into(),
                is_authority: false,
                initiate: true,
            }],
            ice_servers: vec![IceServer {
                urls: vec!["stun:stun.l.google.com:19302".into()],
                username: None,
                credential: None,
            }],
            fallback: TransportKind::Relay,
        })));
        self.plan_sent = true;
    }
}

impl Transport for ScriptedServer {
    fn poll_send(
        &mut self,
        _cx: &mut std::task::Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        if self.aborted {
            return std::task::Poll::Ready(Err(SignalFishError::TransportClosed));
        }
        let Some(frame) = frame.take() else {
            return std::task::Poll::Ready(Ok(()));
        };
        let TransportFrame::Text(message) = frame else {
            return std::task::Poll::Ready(Err(SignalFishError::TransportSend(
                "scripted server does not accept binary frames".into(),
            )));
        };
        // React to the client's commands the way a real server would: answer
        // the join, finalize on the game-start request, and relay the peer's
        // answer back over the signaling path.
        match serde_json::from_str::<ClientMessage>(&message) {
            Ok(ClientMessage::JoinRoom { .. }) => {
                self.room_joined();
                self.queue(ServerMessage::LobbyStateChanged {
                    lobby_state: LobbyState::Lobby,
                    ready_players: vec![ALICE, BOB],
                    all_ready: true,
                });
            }
            Ok(ClientMessage::StartGame) => {
                self.queue(ServerMessage::LobbyStateChanged {
                    lobby_state: LobbyState::Finalized,
                    ready_players: vec![ALICE, BOB],
                    all_ready: true,
                });
                self.session_plan();
            }
            Ok(ClientMessage::Signal { to, .. }) if self.plan_sent && to == BOB => {
                // The server relays Alice's offer to Bob, who answers.
                self.queue(ServerMessage::Signal {
                    from: BOB,
                    generation: Some(GENERATION),
                    signal: serde_json::json!({ "Answer": "<remote-sdp>" }),
                });
            }
            _ => {}
        }
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_recv(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<TransportFrame, SignalFishError>>> {
        if self.aborted {
            return std::task::Poll::Ready(None);
        }
        if let Some(msg) = self.incoming.pop_front() {
            std::task::Poll::Ready(Some(Ok(TransportFrame::Text(msg))))
        } else {
            std::task::Poll::Pending
        }
    }

    fn poll_close(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        self.aborted = true;
        self.incoming.clear();
        std::task::Poll::Ready(Ok(()))
    }

    fn abort(&mut self) {
        self.aborted = true;
        self.incoming.clear();
    }
}

fn protocol_info() -> ServerMessage {
    ServerMessage::ProtocolInfo(ProtocolInfoPayload {
        platform: None,
        sdk_version: None,
        minimum_version: None,
        recommended_version: None,
        capabilities: vec![],
        notes: None,
        game_data_formats: vec![GameDataEncoding::Json, GameDataEncoding::MessagePack],
        player_name_rules: None,
        protocol_version: Some(3),
        min_protocol_version: Some(2),
        max_protocol_version: Some(3),
        transports: Some(vec![MessageTransport::Websocket]),
        max_outbound_message_size: Some(8_388_608),
    })
}

// ─────────────────────────────────────────────────────────────────────
// Step 3: Drive the mesh — a handful of lines.
// ─────────────────────────────────────────────────────────────────────

async fn run_demo() -> Result<(), SignalFishError> {
    let transport = ScriptedServer {
        // The server authenticates, then advertises the canonical protocol-v3
        // negotiation while it waits for the client's room commands.
        incoming: VecDeque::from(vec![
            serde_json::to_string(&ServerMessage::Authenticated {
                app_name: "demo".into(),
                organization: None,
                rate_limits: signal_fish_client::RateLimitInfo {
                    per_minute: 60,
                    per_hour: 1_000,
                    per_day: 10_000,
                },
            })
            .unwrap_or_default(),
            serde_json::to_string(&protocol_info()).unwrap_or_default(),
        ]),
        plan_sent: false,
        aborted: false,
    };

    // `MeshController::start` preserves compatible explicit choices and adds
    // the minimum WebRTC/P2P advertisement needed by the controller.
    let mut mesh = MeshController::start(
        transport,
        SignalFishConfig::new("demo-app"),
        DemoDriver::default(),
    );

    // Ready-state updates repeat — request the game start only once.
    let mut start_requested = false;
    // The demo completes only when the peer handshake lands.
    let mut completed = false;

    while let Some(event) = mesh.recv().await {
        match event {
            MeshEvent::Signaling(sig) => match *sig {
                SignalFishEvent::Authenticated { .. } => {
                    println!("authenticated → joining room");
                    mesh.join_room(JoinRoomParams::new("demo-game", "Alice"))?;
                }
                SignalFishEvent::LobbyStateChanged {
                    all_ready: true, ..
                } if !start_requested => {
                    println!("everyone ready → starting game");
                    start_requested = true;
                    mesh.start_game()?;
                }
                SignalFishEvent::SessionPlan { peers, .. } => {
                    println!("session plan: {} peer(s) to connect", peers.len());
                }
                // Terminal diagnostics must never vanish silently — the
                // scripted flow is broken if any of these arrive.
                SignalFishEvent::ProtocolViolation { kind, diagnostic } => {
                    eprintln!("protocol violation ({kind:?}): {diagnostic}");
                    break;
                }
                SignalFishEvent::DecodeFailed { .. } => {
                    eprintln!("received an undecodable server frame");
                    break;
                }
                SignalFishEvent::RoomJoinFailed { reason, .. } => {
                    eprintln!("join failed: {reason}");
                    break;
                }
                SignalFishEvent::Disconnected { .. } => {
                    eprintln!("signaling ended before the peer connected");
                    break;
                }
                other => println!("  event: {:?}", other),
            },
            MeshEvent::PeerConnected(peer) => {
                println!("✅ peer {peer} connected over WebRTC — sending a packet");
                mesh.send_to(peer, b"hello peer");
                completed = true;
                break; // demo complete
            }
            MeshEvent::PeerDisconnected(peer) => println!("peer {peer} disconnected"),
            MeshEvent::Data { from, data } => {
                println!("📦 {} bytes from {from}", data.len());
            }
        }
    }

    mesh.shutdown().await;
    if !completed {
        eprintln!("demo ended without a connected peer");
        return Err(SignalFishError::TransportClosed);
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), SignalFishError> {
    run_demo().await?;
    println!("done");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::run_demo;

    /// Rot detector: the demo must complete the whole scripted v3 handshake
    /// (plan accepted, peer connected) well inside this budget instead of
    /// hanging on a protocol-flow regression.
    #[tokio::test]
    async fn end_to_end_completes() {
        let finished = tokio::time::timeout(std::time::Duration::from_secs(15), run_demo())
            .await
            .map(|demo| demo.is_ok())
            .unwrap_or(false);
        assert!(
            finished,
            "mesh_session demo must complete end-to-end without hanging"
        );
    }
}
