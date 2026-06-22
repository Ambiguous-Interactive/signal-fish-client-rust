//! # Mesh Session Example (protocol v3)
//!
//! Demonstrates the **batteries-included** mesh path: implement the
//! [`WebRtcDriver`] trait against your WebRTC stack, hand it to
//! [`MeshController`], and the SDK drives the entire signaling handshake for you
//! — obeying the server's `initiate` directives, relaying offers/answers/ICE,
//! reporting transport status, and surfacing a clean [`MeshEvent`] stream.
//!
//! This example is fully self-contained: a scripted in-process "server" plays
//! the v3 handshake, and a tiny in-memory driver completes it, so the whole
//! stack runs end-to-end with no network.
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

use async_trait::async_trait;
use signal_fish_client::protocol::{IceServer, PlayerId};
use signal_fish_client::webrtc::{DriverEvent, MeshController, MeshEvent, WebRtcDriver};
use signal_fish_client::{
    JoinRoomParams, PeerSignal, SignalFishConfig, SignalFishError, SignalFishEvent, Transport,
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

    fn connect(&mut self, peer: PlayerId, initiate: bool) {
        // REAL DRIVER: create an RTCPeerConnection for `peer`. If `initiate`,
        // create an offer and surface it via `poll` as DriverEvent::Signal.
        println!("  driver: connect to {peer} (initiate={initiate})");
        if initiate {
            self.outbox.push_back(DriverEvent::Signal {
                peer,
                signal: PeerSignal::Offer("<sdp-offer>".into()),
            });
        }
    }

    fn on_signal(&mut self, peer: PlayerId, signal: PeerSignal) {
        // REAL DRIVER: apply the remote description / add the ICE candidate.
        println!("  driver: got {signal:?} from {peer}");
        match signal {
            PeerSignal::Offer(_) => {
                self.outbox.push_back(DriverEvent::Signal {
                    peer,
                    signal: PeerSignal::Answer("<sdp-answer>".into()),
                });
                self.outbox.push_back(DriverEvent::Connected { peer });
            }
            PeerSignal::Answer(_) => self.outbox.push_back(DriverEvent::Connected { peer }),
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

struct ScriptedServer {
    incoming: VecDeque<String>,
    peer: PlayerId,
    started: bool,
}

#[async_trait]
impl Transport for ScriptedServer {
    async fn send(&mut self, message: String) -> Result<(), SignalFishError> {
        // The server reacts to the client's StartGame by finalizing the session.
        if message.contains("\"StartGame\"") && !self.started {
            self.started = true;
            self.incoming.push_back(session_plan_json(self.peer));
            // ...then the peer's answer arrives, completing the handshake.
            self.incoming
                .push_back(signal_json(self.peer, r#"{"Answer":"<remote-sdp>"}"#));
        }
        Ok(())
    }

    async fn recv(&mut self) -> Option<Result<String, SignalFishError>> {
        if let Some(msg) = self.incoming.pop_front() {
            Some(Ok(msg))
        } else {
            // No scripted messages remain — pending() never completes, keeping
            // the controller alive until shutdown.
            std::future::pending().await
        }
    }

    async fn close(&mut self) -> Result<(), SignalFishError> {
        Ok(())
    }
}

fn session_plan_json(peer: PlayerId) -> String {
    format!(
        r#"{{"type":"SessionPlan","data":{{"topology":"mesh","transport":"webrtc","peers":[{{"player_id":"{peer}","player_name":"Bob","is_authority":false,"initiate":true}}],"ice_servers":[{{"urls":["stun:stun.l.google.com:19302"]}}],"fallback":"relay"}}}}"#
    )
}

fn signal_json(from: PlayerId, signal: &str) -> String {
    format!(r#"{{"type":"Signal","data":{{"from":"{from}","signal":{signal}}}}}"#)
}

// ─────────────────────────────────────────────────────────────────────
// Step 3: Drive the mesh — a handful of lines.
// ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), SignalFishError> {
    let peer = uuid::Uuid::from_u128(0xB);
    let transport = ScriptedServer {
        // The server authenticates, advertises v3, then waits for StartGame.
        incoming: VecDeque::from(vec![
            r#"{"type":"Authenticated","data":{"app_name":"demo","rate_limits":{"per_minute":60,"per_hour":1000,"per_day":10000}}}"#.to_string(),
            r#"{"type":"ProtocolInfo","data":{"capabilities":[],"game_data_formats":[],"protocol_version":3,"min_protocol_version":2,"max_protocol_version":3}}"#.to_string(),
            r#"{"type":"LobbyStateChanged","data":{"lobby_state":"lobby","ready_players":[],"all_ready":true}}"#.to_string(),
        ]),
        peer,
        started: false,
    };

    // `MeshController::start` enables the mesh automatically.
    let mut mesh = MeshController::start(
        transport,
        SignalFishConfig::new("demo-app"),
        DemoDriver::default(),
    );

    while let Some(event) = mesh.recv().await {
        match event {
            MeshEvent::Signaling(sig) => match *sig {
                SignalFishEvent::Authenticated { .. } => {
                    println!("authenticated → joining room");
                    mesh.join_room(JoinRoomParams::new("demo-game", "Alice"))?;
                }
                SignalFishEvent::LobbyStateChanged {
                    all_ready: true, ..
                } => {
                    println!("everyone ready → starting game");
                    mesh.start_game()?;
                }
                SignalFishEvent::SessionPlan { peers, .. } => {
                    println!("session plan: {} peer(s) to connect", peers.len());
                }
                _ => {}
            },
            MeshEvent::PeerConnected(peer) => {
                println!("✅ peer {peer} connected over WebRTC — sending a packet");
                mesh.send_to(peer, b"hello peer");
                break; // demo complete
            }
            MeshEvent::PeerDisconnected(peer) => println!("peer {peer} disconnected"),
            MeshEvent::Data { from, data } => {
                println!("📦 {} bytes from {from}", data.len());
            }
        }
    }

    mesh.shutdown().await;
    println!("done");
    Ok(())
}
