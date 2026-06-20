//! The pluggable WebRTC driver seam and the [`MeshController`] that drives it
//! (protocol v3, `mesh` feature).
//!
//! The SDK is signaling-only: it never bundles a WebRTC stack. [`WebRtcDriver`]
//! is the seam a consumer implements against their WebRTC backend (str0m,
//! webrtc-rs, or the browser's `RTCPeerConnection` via web-sys). The
//! [`MeshController`] then drives the **entire signaling choreography** against
//! that driver — obeying the server's `SessionPlan`/`NewPeer` directives,
//! relaying offers/answers/ICE, reporting transport status, and surfacing a
//! clean [`MeshEvent`] stream — so the consumer only implements the WebRTC
//! primitives and reads bytes from peers.
//!
//! The driver is **synchronous and poll-based** so it fits both the async and
//! the WASM/polling runtimes and matches sans-I/O backends like str0m. The
//! client still "obeys the server": the controller passes the server-assigned
//! `initiate` flag straight through and never computes who offers.

use crate::protocol::{IceServer, PlayerId};
use crate::signal::PeerSignal;

/// A pluggable WebRTC backend the [`MeshController`] drives through the signaling
/// handshake.
///
/// Implement this against your WebRTC stack. The controller calls the `&mut self`
/// methods to drive connection setup, then repeatedly calls [`poll`](Self::poll)
/// to drain outbound signals, connection-state changes, and received data. All
/// methods are non-blocking; do real I/O inside [`poll`](Self::poll).
pub trait WebRtcDriver {
    /// Apply the ICE (STUN/TURN) servers for subsequent connections. Called when
    /// a `SessionPlan` (or ICE pre-gather on join) provides them.
    fn set_ice_servers(&mut self, servers: &[IceServer]);

    /// Begin connecting to `peer`. If `initiate` is `true`, create an offer and
    /// emit it via [`poll`](Self::poll) as [`DriverEvent::Signal`]; otherwise
    /// wait for the remote offer. Obey `initiate` verbatim — it is the server's
    /// deterministic offerer assignment.
    fn connect(&mut self, peer: PlayerId, initiate: bool);

    /// Feed a remote signal (offer/answer/ICE candidate) received from `peer`.
    fn on_signal(&mut self, peer: PlayerId, signal: PeerSignal);

    /// Send application bytes to `peer` over its data channel (best-effort).
    fn send(&mut self, peer: PlayerId, data: &[u8]);

    /// Tear down the connection to `peer` (the peer left or was re-planned away).
    fn disconnect(&mut self, peer: PlayerId);

    /// Drain the next driver output, or `None` when idle. The controller calls
    /// this in a loop until it returns `None`.
    fn poll(&mut self) -> Option<DriverEvent>;
}

/// An output produced by a [`WebRtcDriver`], drained via [`WebRtcDriver::poll`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverEvent {
    /// A locally-produced signal to relay to `peer` (offer, answer, or a
    /// trickled ICE candidate). The controller forwards it to the server.
    Signal {
        /// The peer this signal is destined for.
        peer: PlayerId,
        /// The signal payload.
        signal: PeerSignal,
    },
    /// The data channel to `peer` opened.
    Connected {
        /// The peer whose channel opened.
        peer: PlayerId,
    },
    /// The data channel to `peer` closed or failed.
    Disconnected {
        /// The peer whose channel closed.
        peer: PlayerId,
    },
    /// Application bytes arrived from `peer`.
    Data {
        /// The sending peer.
        peer: PlayerId,
        /// The received bytes.
        data: Vec<u8>,
    },
}

/// A high-level event surfaced by the [`MeshController`].
#[derive(Debug, Clone)]
pub enum MeshEvent {
    /// An underlying signaling event passed through verbatim (`RoomJoined`,
    /// `GameData`, `LobbyStateChanged`, etc.). Signaling events the controller
    /// consumes for choreography (`SessionPlan`, `SignalReceived`, …) are still
    /// passed through here so the consumer can observe them.
    ///
    /// Boxed because [`SignalFishEvent`](crate::SignalFishEvent) is large and
    /// `MeshEvent` is moved frequently; match it as `Signaling(ev) => match *ev`.
    Signaling(Box<crate::event::SignalFishEvent>),
    /// A peer's peer-to-peer data channel is now open.
    PeerConnected(PlayerId),
    /// A peer's peer-to-peer data channel closed.
    PeerDisconnected(PlayerId),
    /// Application bytes received from a peer over the peer-to-peer data channel.
    Data {
        /// The sending peer.
        from: PlayerId,
        /// The received bytes.
        data: Vec<u8>,
    },
}

#[cfg(feature = "tokio-runtime")]
pub use controller::MeshController;

#[cfg(feature = "tokio-runtime")]
mod controller {
    use std::time::Duration;

    use tokio::sync::mpsc;
    use tracing::{debug, warn};

    use super::{DriverEvent, MeshEvent, WebRtcDriver};
    use crate::client::{SignalFishClient, SignalFishConfig};
    use crate::event::SignalFishEvent;
    use crate::mesh::MeshSession;
    use crate::protocol::{PlayerId, TransportKind};
    use crate::signal::PeerSignal;
    use crate::transport::Transport;

    /// Default interval at which the controller pumps the driver for trickle ICE
    /// / data when no signaling event is arriving.
    const DEFAULT_PUMP_INTERVAL: Duration = Duration::from_millis(20);

    /// Drives a [`WebRtcDriver`] through the full v3 mesh signaling handshake on
    /// top of a [`SignalFishClient`], surfacing a [`MeshEvent`] stream.
    ///
    /// `MeshController::start` enables the mesh automatically (if the config did
    /// not already opt in), so the canonical usage is a few lines:
    ///
    /// ```rust,ignore
    /// let (mut mesh) = MeshController::start(transport, SignalFishConfig::new("app"), my_driver);
    /// while let Some(event) = mesh.recv().await {
    ///     match event {
    ///         MeshEvent::Signaling(sig) => match *sig {
    ///             SignalFishEvent::Authenticated { .. } =>
    ///                 mesh.join_room(JoinRoomParams::new("game", "Alice"))?,
    ///             SignalFishEvent::LobbyStateChanged { all_ready: true, .. } =>
    ///                 mesh.start_game()?,
    ///             _ => {}
    ///         },
    ///         MeshEvent::PeerConnected(peer) => { /* peer ready */ }
    ///         MeshEvent::Data { from, data } => { /* game packet */ }
    ///         _ => {}
    ///     }
    /// }
    /// ```
    ///
    /// `MeshController<D>` is [`Send`] when `D` is, so the `recv()` loop can run
    /// on a spawned task. A `!Send` driver (e.g. a browser `RTCPeerConnection`
    /// wrapper) must instead be driven on the current task.
    pub struct MeshController<D: WebRtcDriver> {
        client: SignalFishClient,
        events: mpsc::Receiver<SignalFishEvent>,
        driver: D,
        session: MeshSession,
        /// Peers the driver has been told to connect to (so they can be torn
        /// down on re-election, room-leave, or disconnect).
        known_peers: Vec<PlayerId>,
        /// Peers currently reporting an open data channel (for transport-status
        /// transitions: 0↔1 boundaries report `TransportStatus`).
        connected_peers: Vec<PlayerId>,
        pump_interval: Duration,
    }

    impl<D: WebRtcDriver> MeshController<D> {
        /// Start a mesh-driving client over `transport` using `driver`.
        ///
        /// If `config` has not opted into the mesh, this enables it (so the
        /// server can form a P2P session). The driver is engaged automatically as
        /// the server's `SessionPlan`/`NewPeer` directives arrive.
        pub fn start(transport: impl Transport, config: SignalFishConfig, driver: D) -> Self {
            let config = if config.protocol_version.is_none() {
                config.enable_mesh()
            } else {
                config
            };
            let (client, events) = SignalFishClient::start(transport, config);
            Self {
                client,
                events,
                driver,
                session: MeshSession::new(),
                known_peers: Vec::new(),
                connected_peers: Vec::new(),
                pump_interval: DEFAULT_PUMP_INTERVAL,
            }
        }

        /// Override the interval at which the driver is pumped for trickle ICE /
        /// data between signaling events. Defaults to 20ms.
        #[must_use]
        pub fn with_pump_interval(mut self, interval: Duration) -> Self {
            self.pump_interval = interval.max(Duration::from_millis(1));
            self
        }

        /// The current mesh session view (topology, peers, ICE, …).
        #[must_use]
        pub fn session(&self) -> &MeshSession {
            &self.session
        }

        /// Send application bytes to `peer` over its peer-to-peer data channel.
        pub fn send_to(&mut self, peer: PlayerId, data: &[u8]) {
            self.driver.send(peer, data);
        }

        /// Receive the next high-level mesh event. Returns `None` once the
        /// underlying transport closes.
        pub async fn recv(&mut self) -> Option<MeshEvent> {
            loop {
                // Surface any pending driver output first (relaying signals /
                // reporting status as side effects).
                if let Some(event) = self.drain_driver() {
                    return Some(event);
                }

                tokio::select! {
                    incoming = self.events.recv() => {
                        match incoming {
                            Some(event) => {
                                self.handle_event(&event);
                                return Some(MeshEvent::Signaling(Box::new(event)));
                            }
                            None => return None,
                        }
                    }
                    () = tokio::time::sleep(self.pump_interval) => {
                        // Loop back to drain the driver for trickle ICE / data.
                    }
                }
            }
        }

        /// Drive the driver in response to a signaling event (and fold the mesh
        /// session view).
        fn handle_event(&mut self, event: &SignalFishEvent) {
            self.session.apply(event);
            match event {
                SignalFishEvent::SessionPlan {
                    peers, ice_servers, ..
                } => {
                    if !ice_servers.is_empty() {
                        self.driver.set_ice_servers(ice_servers);
                    }
                    let new_ids: Vec<PlayerId> = peers.iter().map(|p| p.player_id).collect();
                    // Disconnect peers dropped from the new plan (host re-election
                    // or topology change).
                    for old in self.known_peers.clone() {
                        if !new_ids.contains(&old) {
                            self.driver.disconnect(old);
                            self.mark_disconnected(old);
                        }
                    }
                    // Connect peers newly named by this plan; survivors keep their
                    // existing connection.
                    for peer in peers {
                        if !self.known_peers.contains(&peer.player_id) {
                            self.driver.connect(peer.player_id, peer.initiate);
                        }
                    }
                    self.known_peers = new_ids;
                }
                SignalFishEvent::NewPeer {
                    peer_id,
                    you_initiate,
                } => {
                    if !self.known_peers.contains(peer_id) {
                        self.driver.connect(*peer_id, *you_initiate);
                        self.known_peers.push(*peer_id);
                    }
                }
                SignalFishEvent::SignalReceived { from, signal } => {
                    match PeerSignal::try_from(signal) {
                        Ok(sig) => self.driver.on_signal(*from, sig),
                        Err(_) => warn!("dropping unrecognized signal shape from {from}"),
                    }
                }
                SignalFishEvent::PlayerLeft { player_id } => {
                    self.driver.disconnect(*player_id);
                    self.mark_disconnected(*player_id);
                    self.known_peers.retain(|p| p != player_id);
                }
                // The session ended: tear down every peer connection.
                SignalFishEvent::RoomLeft | SignalFishEvent::Disconnected { .. } => {
                    for peer in std::mem::take(&mut self.known_peers) {
                        self.driver.disconnect(peer);
                    }
                    self.connected_peers.clear();
                }
                SignalFishEvent::RoomJoined { ice_servers, .. }
                | SignalFishEvent::Reconnected { ice_servers, .. }
                    if !ice_servers.is_empty() =>
                {
                    self.driver.set_ice_servers(ice_servers);
                }
                _ => {}
            }
        }

        /// Drain one surfacing driver output, performing the signaling side
        /// effects (relay signal / report status) for non-surfacing outputs.
        fn drain_driver(&mut self) -> Option<MeshEvent> {
            while let Some(driver_event) = self.driver.poll() {
                match driver_event {
                    DriverEvent::Signal { peer, signal } => {
                        // Relay the offer/answer/ICE to the peer via the server.
                        if let Err(e) = self.client.send_signal(peer, signal) {
                            debug!("could not relay signal to {peer}: {e}");
                        }
                    }
                    DriverEvent::Connected { peer } => {
                        self.mark_connected(peer);
                        return Some(MeshEvent::PeerConnected(peer));
                    }
                    DriverEvent::Disconnected { peer } => {
                        self.mark_disconnected(peer);
                        return Some(MeshEvent::PeerDisconnected(peer));
                    }
                    DriverEvent::Data { peer, data } => {
                        return Some(MeshEvent::Data { from: peer, data });
                    }
                }
            }
            None
        }

        fn mark_connected(&mut self, peer: PlayerId) {
            if self.connected_peers.contains(&peer) {
                return;
            }
            let was_empty = self.connected_peers.is_empty();
            self.connected_peers.push(peer);
            if was_empty {
                // First live P2P channel: tell the server WebRTC is up.
                let _ = self
                    .client
                    .report_transport_status(TransportKind::WebRtc, true);
            }
        }

        fn mark_disconnected(&mut self, peer: PlayerId) {
            let before = self.connected_peers.len();
            self.connected_peers.retain(|p| *p != peer);
            if self.connected_peers.is_empty() && before > 0 {
                // Last live P2P channel closed: we are back on the relay floor.
                let _ = self
                    .client
                    .report_transport_status(TransportKind::WebRtc, false);
            }
        }

        // ── Delegations to the inner client (room lifecycle) ─────────

        /// Join (or create) a room. See [`SignalFishClient::join_room`].
        ///
        /// # Errors
        ///
        /// See [`SignalFishClient::join_room`].
        pub fn join_room(&self, params: crate::client::JoinRoomParams) -> crate::error::Result<()> {
            self.client.join_room(params)
        }

        /// Signal readiness. See [`SignalFishClient::set_ready`].
        ///
        /// # Errors
        ///
        /// See [`SignalFishClient::set_ready`].
        pub fn set_ready(&self) -> crate::error::Result<()> {
            self.client.set_ready()
        }

        /// Start the game. See [`SignalFishClient::start_game`].
        ///
        /// # Errors
        ///
        /// See [`SignalFishClient::start_game`].
        pub fn start_game(&self) -> crate::error::Result<()> {
            self.client.start_game()
        }

        /// Leave the current room. See [`SignalFishClient::leave_room`].
        ///
        /// # Errors
        ///
        /// See [`SignalFishClient::leave_room`].
        pub fn leave_room(&self) -> crate::error::Result<()> {
            self.client.leave_room()
        }

        /// Access the underlying client for any operation not delegated here.
        #[must_use]
        pub fn client(&self) -> &SignalFishClient {
            &self.client
        }

        /// Gracefully shut down the controller and its client.
        pub async fn shutdown(mut self) {
            self.client.shutdown().await;
        }
    }
}

#[cfg(all(test, feature = "tokio-runtime"))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;
    use crate::client::SignalFishConfig;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    // ── A recording mock driver ─────────────────────────────────────

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum DriverCall {
        SetIceServers(usize),
        Connect(PlayerId, bool),
        OnSignal(PlayerId, PeerSignal),
        Send(PlayerId, Vec<u8>),
        Disconnect(PlayerId),
    }

    #[derive(Default)]
    struct MockDriver {
        calls: Vec<DriverCall>,
        /// Outputs to emit on subsequent `poll()` calls.
        outputs: VecDeque<DriverEvent>,
    }

    impl MockDriver {
        fn emit(&mut self, event: DriverEvent) {
            self.outputs.push_back(event);
        }
    }

    impl WebRtcDriver for MockDriver {
        fn set_ice_servers(&mut self, servers: &[IceServer]) {
            self.calls.push(DriverCall::SetIceServers(servers.len()));
        }
        fn connect(&mut self, peer: PlayerId, initiate: bool) {
            self.calls.push(DriverCall::Connect(peer, initiate));
            // A realistic driver: the initiator immediately produces an offer to
            // relay; the answerer waits for the remote offer.
            if initiate {
                self.outputs.push_back(DriverEvent::Signal {
                    peer,
                    signal: PeerSignal::Offer("local-sdp".into()),
                });
            }
        }
        fn on_signal(&mut self, peer: PlayerId, signal: PeerSignal) {
            self.calls.push(DriverCall::OnSignal(peer, signal.clone()));
            // Model the handshake completing: an answerer responds to an offer
            // and the channel opens; an initiator's channel opens on the answer.
            match signal {
                PeerSignal::Offer(_) => {
                    self.outputs.push_back(DriverEvent::Signal {
                        peer,
                        signal: PeerSignal::Answer("local-answer".into()),
                    });
                    self.outputs.push_back(DriverEvent::Connected { peer });
                }
                PeerSignal::Answer(_) => {
                    self.outputs.push_back(DriverEvent::Connected { peer });
                }
                PeerSignal::IceCandidate(_) => {}
            }
        }
        fn send(&mut self, peer: PlayerId, data: &[u8]) {
            self.calls.push(DriverCall::Send(peer, data.to_vec()));
        }
        fn disconnect(&mut self, peer: PlayerId) {
            self.calls.push(DriverCall::Disconnect(peer));
        }
        fn poll(&mut self) -> Option<DriverEvent> {
            self.outputs.pop_front()
        }
    }

    /// A shared-handle mock driver so the test can both inspect calls and inject
    /// outputs while the controller owns the driver.
    #[derive(Clone, Default)]
    struct SharedDriver(Arc<Mutex<MockDriver>>);

    impl SharedDriver {
        fn calls(&self) -> Vec<DriverCall> {
            self.0.lock().unwrap().calls.clone()
        }
        fn emit(&self, event: DriverEvent) {
            self.0.lock().unwrap().emit(event);
        }
    }

    impl WebRtcDriver for SharedDriver {
        fn set_ice_servers(&mut self, servers: &[IceServer]) {
            self.0.lock().unwrap().set_ice_servers(servers);
        }
        fn connect(&mut self, peer: PlayerId, initiate: bool) {
            self.0.lock().unwrap().connect(peer, initiate);
        }
        fn on_signal(&mut self, peer: PlayerId, signal: PeerSignal) {
            self.0.lock().unwrap().on_signal(peer, signal);
        }
        fn send(&mut self, peer: PlayerId, data: &[u8]) {
            self.0.lock().unwrap().send(peer, data);
        }
        fn disconnect(&mut self, peer: PlayerId) {
            self.0.lock().unwrap().disconnect(peer);
        }
        fn poll(&mut self) -> Option<DriverEvent> {
            self.0.lock().unwrap().poll()
        }
    }

    // ── Mock transport (mirrors tests/common) ───────────────────────

    struct MockTransport {
        incoming: VecDeque<Option<Result<String, crate::error::SignalFishError>>>,
        sent: Arc<Mutex<Vec<String>>>,
        closed: Arc<AtomicBool>,
    }

    impl MockTransport {
        fn new(
            incoming: Vec<Option<Result<String, crate::error::SignalFishError>>>,
        ) -> (Self, Arc<Mutex<Vec<String>>>) {
            let sent = Arc::new(Mutex::new(Vec::new()));
            let closed = Arc::new(AtomicBool::new(false));
            (
                Self {
                    incoming: VecDeque::from(incoming),
                    sent: Arc::clone(&sent),
                    closed,
                },
                sent,
            )
        }
    }

    #[async_trait::async_trait]
    impl Transport for MockTransport {
        async fn send(&mut self, message: String) -> Result<(), crate::error::SignalFishError> {
            self.sent.lock().unwrap().push(message);
            Ok(())
        }
        async fn recv(&mut self) -> Option<Result<String, crate::error::SignalFishError>> {
            if let Some(item) = self.incoming.pop_front() {
                item
            } else {
                // No scripted messages remain — pending() never completes,
                // keeping the controller's task alive until shutdown aborts it.
                std::future::pending().await
            }
        }
        async fn close(&mut self) -> Result<(), crate::error::SignalFishError> {
            self.closed.store(true, Ordering::Relaxed);
            Ok(())
        }
    }

    use crate::protocol::ServerMessage;
    use crate::transport::Transport;

    fn uuid(n: u128) -> PlayerId {
        uuid::Uuid::from_u128(n)
    }

    fn authed() -> String {
        serde_json::to_string(&ServerMessage::Authenticated {
            app_name: "t".into(),
            organization: None,
            rate_limits: crate::protocol::RateLimitInfo {
                per_minute: 1,
                per_hour: 1,
                per_day: 1,
            },
        })
        .unwrap()
    }

    fn protocol_info_v3() -> String {
        r#"{"type":"ProtocolInfo","data":{"capabilities":[],"game_data_formats":[],"protocol_version":3,"min_protocol_version":2,"max_protocol_version":3}}"#.to_string()
    }

    fn session_plan(peer: PlayerId, initiate: bool) -> String {
        use crate::protocol::{SessionPeer, SessionPlanPayload, Topology, TransportKind};
        let payload = SessionPlanPayload {
            topology: Topology::Mesh,
            transport: TransportKind::WebRtc,
            host: None,
            peers: vec![SessionPeer {
                player_id: peer,
                player_name: "P".into(),
                is_authority: false,
                initiate,
            }],
            ice_servers: vec![crate::protocol::IceServer {
                urls: vec!["stun:x".into()],
                username: None,
                credential: None,
            }],
            fallback: TransportKind::Relay,
        };
        serde_json::to_string(&ServerMessage::SessionPlan(Box::new(payload))).unwrap()
    }

    fn signal_from(peer: PlayerId, signal: serde_json::Value) -> String {
        serde_json::to_string(&ServerMessage::Signal { from: peer, signal }).unwrap()
    }

    async fn recv_until_peer_connected(
        mesh: &mut MeshController<SharedDriver>,
    ) -> Option<PlayerId> {
        loop {
            if let MeshEvent::PeerConnected(p) = mesh.recv().await? {
                return Some(p);
            }
        }
    }

    #[tokio::test]
    async fn full_initiator_handshake_relays_offer_then_connects() {
        // End-to-end: SessionPlan(initiate) → driver offers → controller relays
        // it → peer's Answer arrives → driver opens the channel → controller
        // reports transport status and surfaces PeerConnected.
        let peer = uuid(2);
        let driver = SharedDriver::default();
        let (transport, sent) = MockTransport::new(vec![
            Some(Ok(authed())),
            Some(Ok(protocol_info_v3())),
            Some(Ok(session_plan(peer, true))),
            Some(Ok(signal_from(
                peer,
                serde_json::json!({ "Answer": "remote" }),
            ))),
        ]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());

        let connected = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            recv_until_peer_connected(&mut mesh),
        )
        .await
        .expect("timed out")
        .expect("stream closed");
        assert_eq!(connected, peer);

        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let calls = driver.calls();
        // Obeyed the server's `initiate` flag and applied the plan's ICE servers.
        assert!(calls.contains(&DriverCall::Connect(peer, true)));
        assert!(calls
            .iter()
            .any(|c| matches!(c, DriverCall::SetIceServers(n) if *n == 1)));
        // Fed the remote answer to the driver.
        assert!(calls.contains(&DriverCall::OnSignal(
            peer,
            PeerSignal::Answer("remote".into())
        )));

        // Relayed the driver's local offer to the server, and reported WebRTC
        // transport up once the channel opened. (Compute the booleans before the
        // await so no lock guard is held across it.)
        let relayed_offer = sent
            .lock()
            .unwrap()
            .iter()
            .any(|m| m.contains("\"Signal\"") && m.contains("local-sdp"));
        let reported_status =
            sent.lock().unwrap().iter().any(|m| {
                m.contains("TransportStatus") && m.contains("webrtc") && m.contains("true")
            });
        assert!(relayed_offer, "the local offer should be relayed");
        assert!(reported_status, "transport status should be reported up");
        mesh.shutdown().await;
    }

    #[tokio::test]
    async fn answerer_handshake_feeds_offer_and_relays_answer() {
        // The answerer side: a remote Offer arrives → driver answers → controller
        // relays the answer and surfaces PeerConnected.
        let peer = uuid(3);
        let driver = SharedDriver::default();
        let (transport, sent) = MockTransport::new(vec![
            Some(Ok(authed())),
            Some(Ok(protocol_info_v3())),
            Some(Ok(session_plan(peer, false))), // we are NOT the initiator
            Some(Ok(signal_from(
                peer,
                serde_json::json!({ "Offer": "remote-offer" }),
            ))),
        ]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());

        let connected = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            recv_until_peer_connected(&mut mesh),
        )
        .await
        .expect("timed out")
        .expect("stream closed");
        assert_eq!(connected, peer);

        let calls = driver.calls();
        // We were told NOT to initiate, and we fed the remote offer in.
        assert!(calls.contains(&DriverCall::Connect(peer, false)));
        assert!(calls.contains(&DriverCall::OnSignal(
            peer,
            PeerSignal::Offer("remote-offer".into())
        )));
        // Relayed our answer to the server (allow the client's transport loop to
        // flush the queued Signal first).
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(sent
            .lock()
            .unwrap()
            .iter()
            .any(|m| m.contains("\"Signal\"") && m.contains("local-answer")));
        mesh.shutdown().await;
    }

    #[tokio::test]
    async fn player_left_disconnects_driver() {
        let peer = uuid(7);
        let driver = SharedDriver::default();
        let player_left =
            serde_json::to_string(&ServerMessage::PlayerLeft { player_id: peer }).unwrap();
        let (transport, _sent) = MockTransport::new(vec![
            Some(Ok(authed())),
            Some(Ok(protocol_info_v3())),
            Some(Ok(session_plan(peer, false))),
            Some(Ok(player_left)),
        ]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());

        for _ in 0..8 {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(100), mesh.recv()).await;
            if driver.calls().contains(&DriverCall::Disconnect(peer)) {
                break;
            }
        }
        assert!(driver.calls().contains(&DriverCall::Disconnect(peer)));
        mesh.shutdown().await;
    }

    #[tokio::test]
    async fn data_from_driver_is_surfaced() {
        let peer = uuid(5);
        let driver = SharedDriver::default();
        driver.emit(DriverEvent::Data {
            peer,
            data: vec![1, 2, 3],
        });
        let (transport, _sent) = MockTransport::new(vec![Some(Ok(authed()))]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());

        let mut got = None;
        for _ in 0..6 {
            if let Ok(Some(MeshEvent::Data { from, data })) =
                tokio::time::timeout(std::time::Duration::from_millis(100), mesh.recv()).await
            {
                got = Some((from, data));
                break;
            }
        }
        assert_eq!(got, Some((peer, vec![1, 2, 3])));
        mesh.shutdown().await;
    }

    #[tokio::test]
    async fn send_to_forwards_to_driver() {
        let peer = uuid(6);
        let driver = SharedDriver::default();
        let (transport, _sent) = MockTransport::new(vec![Some(Ok(authed()))]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());
        mesh.send_to(peer, &[9, 9]);
        assert!(driver.calls().contains(&DriverCall::Send(peer, vec![9, 9])));
        mesh.shutdown().await;
    }

    #[tokio::test]
    async fn start_enables_mesh_when_config_did_not() {
        let driver = SharedDriver::default();
        let (transport, sent) = MockTransport::new(vec![Some(Ok(authed()))]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());
        // Drain a couple of events to flush the Authenticate.
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), mesh.recv()).await;
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), mesh.recv()).await;
        assert!(sent
            .lock()
            .unwrap()
            .iter()
            .any(|m| m.contains("Authenticate") && m.contains("\"protocol_version\":3")));
        mesh.shutdown().await;
    }

    #[tokio::test]
    async fn peer_disconnect_reports_status_false_and_surfaces_event() {
        let peer = uuid(11);
        let driver = SharedDriver::default();
        let (transport, sent) = MockTransport::new(vec![
            Some(Ok(authed())),
            Some(Ok(protocol_info_v3())),
            Some(Ok(session_plan(peer, true))),
            Some(Ok(signal_from(peer, serde_json::json!({ "Answer": "r" })))),
        ]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());

        // Drive the handshake to PeerConnected.
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            recv_until_peer_connected(&mut mesh),
        )
        .await
        .expect("timed out")
        .expect("stream closed");

        // The driver now reports the channel closed.
        driver.emit(DriverEvent::Disconnected { peer });
        let mut got = false;
        for _ in 0..6 {
            if let Ok(Some(MeshEvent::PeerDisconnected(p))) =
                tokio::time::timeout(std::time::Duration::from_millis(100), mesh.recv()).await
            {
                assert_eq!(p, peer);
                got = true;
                break;
            }
        }
        assert!(got, "PeerDisconnected should surface");
        // The last peer closing reports WebRTC transport down (1→0 transition).
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let reported_false =
            sent.lock().unwrap().iter().any(|m| {
                m.contains("TransportStatus") && m.contains("webrtc") && m.contains("false")
            });
        assert!(reported_false, "transport status false should be reported");
        mesh.shutdown().await;
    }

    #[tokio::test]
    async fn new_peer_drives_connect() {
        let peer = uuid(12);
        let driver = SharedDriver::default();
        let new_peer =
            format!(r#"{{"type":"NewPeer","data":{{"peer_id":"{peer}","you_initiate":true}}}}"#);
        let (transport, _sent) = MockTransport::new(vec![
            Some(Ok(authed())),
            Some(Ok(protocol_info_v3())),
            Some(Ok(new_peer)),
        ]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());
        for _ in 0..6 {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(100), mesh.recv()).await;
            if driver.calls().contains(&DriverCall::Connect(peer, true)) {
                break;
            }
        }
        assert!(driver.calls().contains(&DriverCall::Connect(peer, true)));
        mesh.shutdown().await;
    }

    #[tokio::test]
    async fn room_left_tears_down_driver_peers() {
        let peer = uuid(13);
        let driver = SharedDriver::default();
        let room_left = serde_json::to_string(&ServerMessage::RoomLeft).unwrap();
        let (transport, _sent) = MockTransport::new(vec![
            Some(Ok(authed())),
            Some(Ok(protocol_info_v3())),
            Some(Ok(session_plan(peer, false))),
            Some(Ok(room_left)),
        ]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());
        for _ in 0..8 {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(100), mesh.recv()).await;
            if driver.calls().contains(&DriverCall::Disconnect(peer)) {
                break;
            }
        }
        assert!(
            driver.calls().contains(&DriverCall::Disconnect(peer)),
            "leaving the room must disconnect the driver's peers"
        );
        mesh.shutdown().await;
    }

    #[tokio::test]
    async fn ice_pregather_applies_servers_before_any_plan() {
        let driver = SharedDriver::default();
        let room_joined = r#"{"type":"RoomJoined","data":{"room_id":"00000000-0000-0000-0000-000000000000","room_code":"R","player_id":"00000000-0000-0000-0000-000000000000","game_name":"g","max_players":4,"supports_authority":false,"current_players":[],"is_authority":false,"lobby_state":"waiting","ready_players":[],"relay_type":"auto","ice_servers":[{"urls":["stun:pre"]}]}}"#;
        let (transport, _sent) = MockTransport::new(vec![
            Some(Ok(authed())),
            Some(Ok(protocol_info_v3())),
            Some(Ok(room_joined.to_string())),
        ]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());
        for _ in 0..6 {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(100), mesh.recv()).await;
            if driver
                .calls()
                .iter()
                .any(|c| matches!(c, DriverCall::SetIceServers(_)))
            {
                break;
            }
        }
        assert!(driver
            .calls()
            .iter()
            .any(|c| matches!(c, DriverCall::SetIceServers(n) if *n == 1)));
        mesh.shutdown().await;
    }
}
