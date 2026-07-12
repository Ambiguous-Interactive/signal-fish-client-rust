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

    /// Register a [`MeshWaker`] the driver signals (via [`MeshWaker::wake`]) when
    /// it has output ready to be polled — e.g. a trickled ICE candidate or
    /// inbound data became available *between* signaling events.
    ///
    /// A driver that wakes on readiness is pumped on demand, so trickle ICE and
    /// data surface immediately instead of waiting up to one pump interval. The
    /// default implementation ignores the waker; such drivers are still pumped on
    /// every signaling event and on the controller's periodic timer (see
    /// [`MeshController::with_pump_interval`]), so this is purely a latency
    /// optimization and entirely optional to implement.
    #[cfg(feature = "tokio-runtime")]
    fn set_ready_waker(&mut self, _waker: MeshWaker) {}
}

/// A handle a [`WebRtcDriver`] uses to wake the [`MeshController`] when it has
/// output ready to be polled, eliminating up to one pump-interval of latency on
/// trickle ICE / inbound data. Obtained via [`WebRtcDriver::set_ready_waker`].
///
/// [`wake`](Self::wake) is cheap and safe to call from any thread and as often
/// as the driver likes (extra wakes at worst cause a redundant, cheap poll).
#[cfg(feature = "tokio-runtime")]
#[derive(Clone)]
pub struct MeshWaker(std::sync::Arc<tokio::sync::Notify>);

#[cfg(feature = "tokio-runtime")]
impl MeshWaker {
    /// Signal that the driver has output ready; the controller will pump it on
    /// the next loop turn (waking `recv()` if it is parked).
    pub fn wake(&self) {
        self.0.notify_one();
    }
}

#[cfg(feature = "tokio-runtime")]
impl std::fmt::Debug for MeshWaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MeshWaker")
    }
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

    use super::{DriverEvent, MeshEvent, MeshWaker, WebRtcDriver};
    use crate::client::{SignalFishClient, SignalFishConfig};
    use crate::event::SignalFishEvent;
    use crate::mesh::MeshSession;
    use crate::protocol::{PlayerId, TransportKind};
    use crate::signal::PeerSignal;
    use crate::transport::Transport;

    /// Default interval at which the controller pumps the driver for trickle ICE
    /// / data when no signaling event is arriving.
    const DEFAULT_PUMP_INTERVAL: Duration = Duration::from_millis(20);

    /// A peer the controller has told the driver to connect to, paired with the
    /// server's current offerer assignment. Tracking `initiate` lets a re-plan
    /// that *reassigns* who offers (a host re-election or topology change)
    /// restart the handshake in the new role instead of leaving the driver
    /// stuck in the stale one.
    #[derive(Clone, Copy)]
    struct KnownPeer {
        id: PlayerId,
        initiate: bool,
    }

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
        /// Peers the driver has been told to connect to (each with its current
        /// offerer role), so a role change can re-drive them and so they can be
        /// torn down on re-election, room-leave, or disconnect.
        known_peers: Vec<KnownPeer>,
        /// Peers currently reporting an open data channel (for transport-status
        /// transitions: 0↔1 boundaries report `TransportStatus`).
        connected_peers: Vec<PlayerId>,
        pump_interval: Duration,
        /// Signaled by the driver (via its [`MeshWaker`]) when it has output
        /// ready, so `recv` pumps on demand instead of waiting for the timer.
        ready: std::sync::Arc<tokio::sync::Notify>,
        /// An outbound signal the command queue refused (`SendBufferFull`),
        /// held for retry so congestion never drops it. Driver output is not
        /// popped past a pending signal, preserving signal order, and `recv`
        /// stays cancel-safe because the signal lives here rather than in a
        /// cancellable future. Cleared when the controller tears down the
        /// target peer (see `disconnect_peer`) — an abandoned handshake's
        /// signal must not be relayed stale.
        pending_signal: Option<(PlayerId, PeerSignal)>,
    }

    impl<D: WebRtcDriver> MeshController<D> {
        /// Start a mesh-driving client over `transport` using `driver`.
        ///
        /// If `config` has not opted into the mesh, this enables it (so the
        /// server can form a P2P session). The driver is engaged automatically as
        /// the server's `SessionPlan`/`NewPeer` directives arrive.
        pub fn start(
            transport: impl Transport + Send + 'static,
            config: SignalFishConfig,
            driver: D,
        ) -> Self {
            let config = if config.protocol_version.is_none() {
                config.enable_mesh()
            } else {
                config
            };
            let (client, events) = SignalFishClient::start(transport, config);
            // Hand the driver a waker so it can pump on demand (eliminating up to
            // one pump-interval of trickle-ICE / data latency). Drivers that do not
            // override `set_ready_waker` simply ignore it and fall back to the
            // periodic timer.
            let ready = std::sync::Arc::new(tokio::sync::Notify::new());
            let mut driver = driver;
            driver.set_ready_waker(MeshWaker(std::sync::Arc::clone(&ready)));
            Self {
                client,
                events,
                driver,
                session: MeshSession::new(),
                known_peers: Vec::new(),
                connected_peers: Vec::new(),
                pump_interval: DEFAULT_PUMP_INTERVAL,
                ready,
                pending_signal: None,
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
        ///
        /// # Cancel safety
        ///
        /// This method is cancel-safe: it never holds a popped driver output
        /// across an await point. An outbound signal the command queue cannot
        /// accept yet is buffered in the controller and relayed on a later
        /// iteration (or a later `recv` call), so cancelling `recv` — e.g.
        /// with `tokio::time::timeout` in a frame-budgeted game loop — never
        /// loses a signal.
        pub async fn recv(&mut self) -> Option<MeshEvent> {
            loop {
                // Surface any pending driver output first (relaying signals /
                // reporting status as side effects). This is deliberately
                // synchronous: it must not await while the controller — the
                // sole consumer of `self.events` — is not draining events,
                // or a full command queue + full event channel would
                // deadlock. A refused signal stays buffered; draining events
                // below is exactly what lets the transport loop make
                // progress and free queue capacity for the retry.
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
                    () = self.ready.notified() => {
                        // The driver signaled it has output ready: loop back to
                        // drain it immediately (no pump-interval latency).
                    }
                    () = tokio::time::sleep(self.pump_interval) => {
                        // Periodic safety-net pump for drivers that do not wake
                        // (or to catch readiness that raced the select).
                    }
                }
            }
        }

        /// Fold the mesh session view, then perform the driver choreography for
        /// `event`.
        fn handle_event(&mut self, event: &SignalFishEvent) {
            self.session.apply(event);
            self.choreograph(event);
        }

        /// Drive the driver in response to a single signaling event. The mesh
        /// session view is assumed to be already folded (by [`handle_event`], or
        /// by the recursive `MeshSession::apply` for events replayed out of a
        /// `Reconnected`'s `missed_events`).
        fn choreograph(&mut self, event: &SignalFishEvent) {
            match event {
                SignalFishEvent::SessionPlan {
                    peers, ice_servers, ..
                } => {
                    // The plan is authoritative even when empty (relay reset).
                    self.driver.set_ice_servers(ice_servers);
                    let new_ids: Vec<PlayerId> = peers.iter().map(|p| p.player_id).collect();
                    // Disconnect peers dropped from the new plan (host re-election
                    // or topology change), then forget them.
                    let dropped: Vec<PlayerId> = self
                        .known_peers
                        .iter()
                        .map(|k| k.id)
                        .filter(|id| !new_ids.contains(id))
                        .collect();
                    for old in dropped {
                        self.disconnect_peer(old);
                    }
                    self.known_peers.retain(|k| new_ids.contains(&k.id));
                    // Connect peers newly named by this plan; a survivor whose
                    // offerer role changed is restarted in the new role, and one
                    // whose role is unchanged keeps its existing connection.
                    for peer in peers {
                        self.ensure_peer(peer.player_id, peer.initiate);
                    }
                }
                SignalFishEvent::NewPeer {
                    peer_id,
                    you_initiate,
                } => {
                    self.ensure_peer(*peer_id, *you_initiate);
                }
                SignalFishEvent::SignalReceived { from, signal } => {
                    match PeerSignal::try_from(signal) {
                        Ok(sig) => self.driver.on_signal(*from, sig),
                        Err(_) => warn!("dropping unrecognized signal shape from {from}"),
                    }
                }
                SignalFishEvent::PlayerLeft { player_id, .. } => {
                    self.disconnect_peer(*player_id);
                    self.known_peers.retain(|k| k.id != *player_id);
                }
                // The session ended: tear down every peer connection. Route each
                // through `mark_disconnected` so the 1->0 transport-status edge
                // still fires a single `TransportStatus(WebRtc, false)` — matching
                // the per-peer `PlayerLeft` path. On `Disconnected` the underlying
                // send simply returns `NotConnected` and is harmlessly swallowed;
                // `mark_disconnected` empties `connected_peers` as it goes.
                SignalFishEvent::RoomLeft | SignalFishEvent::Disconnected { .. } => {
                    for peer in std::mem::take(&mut self.known_peers) {
                        self.disconnect_peer(peer.id);
                    }
                }
                SignalFishEvent::RoomJoined { ice_servers, .. } if !ice_servers.is_empty() => {
                    self.driver.set_ice_servers(ice_servers);
                }
                // Reconnect: apply ICE pre-gather, then defensively replay any
                // mesh events the server batched into `missed_events`. Today's
                // server rebuilds the session by re-sending a *live* `SessionPlan`
                // after `Reconnected` (so `missed_events` is empty), but replaying
                // here keeps the client correct against servers that instead carry
                // mesh state in `missed_events`. The fold is idempotent and a later
                // live plan replaces the peer set wholesale.
                SignalFishEvent::Reconnected {
                    ice_servers,
                    missed_events,
                    ..
                } => {
                    if !ice_servers.is_empty() {
                        self.driver.set_ice_servers(ice_servers);
                    }
                    for missed in missed_events {
                        match missed {
                            // Terminal / meta events never belong in a reconnect
                            // replay — by definition we are back in the room.
                            SignalFishEvent::RoomLeft
                            | SignalFishEvent::Disconnected { .. }
                            | SignalFishEvent::Reconnected { .. }
                            | SignalFishEvent::RoomJoined { .. } => {}
                            other => self.choreograph(other),
                        }
                    }
                }
                _ => {}
            }
        }

        /// Tear down `peer`'s driver connection, dropping any buffered
        /// outbound signal addressed to it: the signal belongs to the
        /// handshake being abandoned and would be stale (wrong role, or a
        /// peer no longer in the session) if relayed later. Used for every
        /// controller-initiated teardown; a driver-reported channel drop
        /// (`DriverEvent::Disconnected`) does not clear the buffer, because
        /// the handshake context is still live there.
        fn disconnect_peer(&mut self, peer: PlayerId) {
            self.driver.disconnect(peer);
            if self
                .pending_signal
                .as_ref()
                .is_some_and(|(to, _)| *to == peer)
            {
                self.pending_signal = None;
            }
            self.mark_disconnected(peer);
        }

        /// Ensure the driver holds the server's current offerer role for `peer`.
        ///
        /// A peer the controller has not connected yet is connected fresh. A
        /// known peer whose `initiate` assignment *changed* (a host re-election
        /// or topology change reassigned who offers) has its handshake cleanly
        /// restarted in the new role: leaving the stale role in place would let
        /// the two sides glare (both offer) or stall (both wait), because the SDK
        /// obeys the server verbatim and runs no perfect-negotiation rollback. A
        /// known peer whose role is unchanged keeps its live connection untouched
        /// (the common re-plan case — survivors are never needlessly re-driven).
        ///
        /// If a restarted peer's data channel was already open, the teardown's
        /// `1->0` edge reports `TransportStatus(false)` (and the re-handshake's
        /// `0->1` edge later reports it back up) — a real, observable data-path flap.
        fn ensure_peer(&mut self, peer: PlayerId, initiate: bool) {
            let current = self
                .known_peers
                .iter()
                .find(|k| k.id == peer)
                .map(|k| k.initiate);
            match current {
                None => {
                    self.driver.connect(peer, initiate);
                    self.known_peers.push(KnownPeer { id: peer, initiate });
                }
                Some(prev) if prev != initiate => {
                    debug!(
                        %peer,
                        initiate,
                        "server reassigned the offerer role; restarting handshake"
                    );
                    self.disconnect_peer(peer);
                    self.driver.connect(peer, initiate);
                    if let Some(k) = self.known_peers.iter_mut().find(|k| k.id == peer) {
                        k.initiate = initiate;
                    }
                }
                Some(_) => {}
            }
        }

        /// Attempt to relay the buffered outbound signal, if any.
        ///
        /// Returns `true` when the buffer is clear — the signal was relayed,
        /// there was nothing to relay, or the refusal was terminal
        /// (`NotConnected` / `ProtocolUnsupported`, logged and discarded
        /// because retrying cannot succeed). Returns `false` when the
        /// command queue is full and the signal must be retried later: a
        /// lost offer/answer/ICE candidate stalls the WebRTC handshake, so
        /// congestion buffers the signal instead of dropping it.
        fn relay_pending_signal(&mut self) -> bool {
            let Some((peer, signal)) = self.pending_signal.take() else {
                return true;
            };
            match self.client.send_signal(peer, signal.clone()) {
                Ok(()) => true,
                Err(crate::error::SignalFishError::SendBufferFull { .. }) => {
                    self.pending_signal = Some((peer, signal));
                    false
                }
                Err(e) => {
                    debug!("could not relay signal to {peer}: {e}");
                    true
                }
            }
        }

        /// Drain one surfacing driver output, performing the signaling side
        /// effects (relay signal / report status) for non-surfacing outputs.
        ///
        /// Synchronous by design (see [`recv`](Self::recv)): a signal the
        /// command queue refuses is buffered in `pending_signal` — and driver
        /// output is not popped past it, preserving signal order — rather
        /// than awaited on, so this never blocks event draining and never
        /// holds a popped signal across a cancellation point.
        fn drain_driver(&mut self) -> Option<MeshEvent> {
            loop {
                // A buffered signal must go out before any further driver
                // output is popped; if the queue is still full, return to
                // the select loop and retry on the next wake.
                if !self.relay_pending_signal() {
                    return None;
                }
                let driver_event = self.driver.poll()?;
                match driver_event {
                    DriverEvent::Signal { peer, signal } => {
                        // Buffer, then immediately attempt the relay on the
                        // next loop iteration.
                        self.pending_signal = Some((peer, signal));
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
        }

        fn mark_connected(&mut self, peer: PlayerId) {
            if self.connected_peers.contains(&peer) {
                return;
            }
            let was_empty = self.connected_peers.is_empty();
            self.connected_peers.push(peer);
            if was_empty {
                // First live P2P channel: tell the server WebRTC is up. The
                // report is informational and idempotent, so a refusal (e.g.
                // NotConnected during teardown) is logged, not retried.
                if let Err(e) = self
                    .client
                    .report_transport_status(TransportKind::WebRtc, true)
                {
                    debug!("could not report WebRTC transport up: {e}");
                }
            }
        }

        fn mark_disconnected(&mut self, peer: PlayerId) {
            let before = self.connected_peers.len();
            self.connected_peers.retain(|p| *p != peer);
            if self.connected_peers.is_empty() && before > 0 {
                // Last live P2P channel closed: we are back on the relay floor.
                // Informational and idempotent — a refusal is logged, not retried.
                if let Err(e) = self
                    .client
                    .report_transport_status(TransportKind::WebRtc, false)
                {
                    debug!("could not report WebRTC transport down: {e}");
                }
            }
        }

        // ── Delegations to the inner client (room lifecycle) ─────────

        /// Join (or create) a room. See [`SignalFishClient::join_room`].
        ///
        /// # Errors
        ///
        /// See [`SignalFishClient::join_room`].
        pub fn join_room(
            &mut self,
            params: crate::client::JoinRoomParams,
        ) -> crate::error::Result<()> {
            self.client.join_room(params)
        }

        /// Signal readiness. See [`SignalFishClient::set_ready`].
        ///
        /// # Errors
        ///
        /// See [`SignalFishClient::set_ready`].
        pub fn set_ready(&mut self) -> crate::error::Result<()> {
            self.client.set_ready()
        }

        /// Start the game. See [`SignalFishClient::start_game`].
        ///
        /// # Errors
        ///
        /// See [`SignalFishClient::start_game`].
        pub fn start_game(&mut self) -> crate::error::Result<()> {
            self.client.start_game()
        }

        /// Leave the current room. See [`SignalFishClient::leave_room`].
        ///
        /// # Errors
        ///
        /// See [`SignalFishClient::leave_room`].
        pub fn leave_room(&mut self) -> crate::error::Result<()> {
            self.client.leave_room()
        }

        /// Access the underlying client for any operation not delegated here.
        #[must_use]
        pub fn client(&self) -> &SignalFishClient {
            &self.client
        }

        /// Mutably access the underlying client for synchronous commands.
        pub fn client_mut(&mut self) -> &mut SignalFishClient {
            &mut self.client
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
    clippy::indexing_slicing,
    clippy::type_complexity
)]
mod tests {
    use super::*;
    use crate::client::SignalFishConfig;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    // The docs promise `MeshController<D>` is `Send` when `D` is, so the `recv()`
    // loop can run on a spawned task. Pin that with a compile-time assertion.
    const _: fn() = || {
        fn assert_send<T: Send>() {}
        assert_send::<MeshController<SharedDriver>>();
    };

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
        /// The waker handed by the controller (used by the latency test).
        waker: Option<super::MeshWaker>,
    }

    impl MockDriver {
        fn emit(&mut self, event: DriverEvent) {
            self.outputs.push_back(event);
        }
    }

    impl WebRtcDriver for MockDriver {
        fn set_ready_waker(&mut self, waker: super::MeshWaker) {
            self.waker = Some(waker);
        }
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
        /// Enqueue an output AND signal the controller's waker, so a parked
        /// `recv()` pumps it immediately rather than waiting for the timer.
        fn emit_and_wake(&self, event: DriverEvent) {
            let waker = {
                let mut g = self.0.lock().unwrap();
                g.emit(event);
                g.waker.clone()
            };
            if let Some(w) = waker {
                w.wake();
            }
        }
    }

    impl WebRtcDriver for SharedDriver {
        fn set_ready_waker(&mut self, waker: super::MeshWaker) {
            self.0.lock().unwrap().set_ready_waker(waker);
        }
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

    impl Transport for MockTransport {
        fn poll_send(
            &mut self,
            _cx: &mut std::task::Context<'_>,
            frame: &mut Option<crate::transport::TransportFrame>,
        ) -> std::task::Poll<Result<(), crate::error::SignalFishError>> {
            if let Some(frame) = frame.take() {
                let crate::transport::TransportFrame::Text(message) = frame else {
                    panic!("test mock expected an outbound text frame");
                };
                self.sent.lock().unwrap().push(message);
            }
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_recv(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<
            Option<Result<crate::transport::TransportFrame, crate::error::SignalFishError>>,
        > {
            if let Some(item) = self.incoming.pop_front() {
                std::task::Poll::Ready(
                    item.map(|result| result.map(crate::transport::TransportFrame::Text)),
                )
            } else {
                std::task::Poll::Pending
            }
        }
        fn poll_close(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), crate::error::SignalFishError>> {
            self.closed.store(true, Ordering::Relaxed);
            std::task::Poll::Ready(Ok(()))
        }
    }

    use crate::event::SignalFishEvent;
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

    fn room_baseline(peer: PlayerId) -> String {
        let payload = crate::protocol::RoomJoinedPayload {
            room_id: uuid(100),
            room_code: "ROOM".into(),
            player_id: uuid(1),
            game_name: "test".into(),
            max_players: 4,
            supports_authority: false,
            current_players: vec![crate::protocol::PlayerInfo {
                id: peer,
                name: "P".into(),
                is_authority: false,
                is_ready: false,
                connected_at: "2026-01-01T00:00:00Z".into(),
                connection_info: None,
                epoch: Some(1),
                seq: Some(0),
            }],
            is_authority: false,
            lobby_state: crate::protocol::LobbyState::Lobby,
            ready_players: vec![],
            relay_type: "websocket".into(),
            current_spectators: vec![],
            ice_servers: vec![],
            reconnection_token: None,
        };
        serde_json::to_string(&ServerMessage::RoomJoined(Box::new(payload))).unwrap()
    }

    fn signal_from(peer: PlayerId, signal: serde_json::Value) -> String {
        serde_json::to_string(&ServerMessage::Signal { from: peer, signal }).unwrap()
    }

    /// A `SessionPlan` over several peers with an explicit ICE-server set (use an
    /// empty slice to model a plan that carries no ICE servers).
    fn session_plan_multi(peers: &[(PlayerId, bool)], ice_urls: &[&str]) -> String {
        use crate::protocol::{
            IceServer, SessionPeer, SessionPlanPayload, Topology, TransportKind,
        };
        let payload = SessionPlanPayload {
            topology: Topology::Mesh,
            transport: TransportKind::WebRtc,
            host: None,
            peers: peers
                .iter()
                .map(|(id, initiate)| SessionPeer {
                    player_id: *id,
                    player_name: "P".into(),
                    is_authority: false,
                    initiate: *initiate,
                })
                .collect(),
            ice_servers: ice_urls
                .iter()
                .map(|u| IceServer {
                    urls: vec![(*u).into()],
                    username: None,
                    credential: None,
                })
                .collect(),
            fallback: TransportKind::Relay,
        };
        serde_json::to_string(&ServerMessage::SessionPlan(Box::new(payload))).unwrap()
    }

    fn new_peer_msg(peer: PlayerId, you_initiate: bool) -> String {
        format!(
            r#"{{"type":"NewPeer","data":{{"peer_id":"{peer}","you_initiate":{you_initiate}}}}}"#
        )
    }

    /// A `Reconnected` message carrying `missed_events` (the nested events a
    /// server may batch in lieu of re-sending a live plan).
    fn reconnected_with_missed(missed: Vec<ServerMessage>) -> String {
        use crate::protocol::ReconnectedPayload;
        let payload = ReconnectedPayload {
            room_id: uuid(0),
            room_code: "R".into(),
            player_id: uuid(0),
            game_name: "g".into(),
            max_players: 4,
            supports_authority: false,
            current_players: vec![],
            is_authority: false,
            lobby_state: crate::protocol::LobbyState::Waiting,
            ready_players: vec![],
            relay_type: "auto".into(),
            current_spectators: vec![],
            ice_servers: vec![],
            missed_events: missed,
            replay: None,
            sender_watermarks: vec![],
            reconnection_token: None,
        };
        serde_json::to_string(&ServerMessage::Reconnected(Box::new(payload))).unwrap()
    }

    /// Pump `mesh.recv()` (draining driver output and inbound messages) until
    /// `pred` holds over the driver's recorded calls, or a bounded number of
    /// iterations elapse. Returns whether `pred` ultimately held.
    async fn pump_until(
        mesh: &mut MeshController<SharedDriver>,
        driver: &SharedDriver,
        pred: impl Fn(&[DriverCall]) -> bool,
    ) -> bool {
        for _ in 0..40 {
            if pred(&driver.calls()) {
                return true;
            }
            let _ = tokio::time::timeout(std::time::Duration::from_millis(40), mesh.recv()).await;
        }
        pred(&driver.calls())
    }

    fn count_calls(driver: &SharedDriver, pred: impl Fn(&DriverCall) -> bool) -> usize {
        driver.calls().iter().filter(|c| pred(c)).count()
    }

    /// Drive `mesh.recv()` a bounded number of times to drain all scripted
    /// inbound messages (each parked `recv` past the script times out quickly).
    async fn drain(mesh: &mut MeshController<SharedDriver>, iterations: usize) {
        for _ in 0..iterations {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(20), mesh.recv()).await;
        }
    }

    fn sent_count(sent: &Arc<Mutex<Vec<String>>>, needles: &[&str]) -> usize {
        sent.lock()
            .unwrap()
            .iter()
            .filter(|m| needles.iter().all(|n| m.contains(n)))
            .count()
    }

    async fn wait_for_sent_count(
        sent: &Arc<Mutex<Vec<String>>>,
        needles: &[&str],
        expected: usize,
    ) {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while sent_count(sent, needles) < expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for {expected} sent message(s) containing {needles:?}; got {}",
                sent_count(sent, needles)
            )
        });
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

    /// Transport whose sends each require a semaphore permit, so a test can
    /// hold the client's bounded command queue full deterministically.
    struct GatedTransport {
        incoming: VecDeque<Option<Result<String, crate::error::SignalFishError>>>,
        sent: Arc<Mutex<Vec<String>>>,
        permits: Arc<tokio::sync::Semaphore>,
        entered: Arc<std::sync::atomic::AtomicUsize>,
        pending_acquire: Option<
            std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                tokio::sync::OwnedSemaphorePermit,
                                tokio::sync::AcquireError,
                            >,
                        > + Send,
                >,
            >,
        >,
    }

    impl Transport for GatedTransport {
        fn poll_send(
            &mut self,
            cx: &mut std::task::Context<'_>,
            frame: &mut Option<crate::transport::TransportFrame>,
        ) -> std::task::Poll<Result<(), crate::error::SignalFishError>> {
            if self.pending_acquire.is_none() {
                self.entered
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                self.pending_acquire = Some(Box::pin(Arc::clone(&self.permits).acquire_owned()));
            }
            let poll = std::future::Future::poll(
                self.pending_acquire
                    .as_mut()
                    .expect("acquire future was just installed")
                    .as_mut(),
                cx,
            );
            let permit = match poll {
                std::task::Poll::Pending => return std::task::Poll::Pending,
                std::task::Poll::Ready(Ok(permit)) => permit,
                std::task::Poll::Ready(Err(_)) => {
                    self.pending_acquire = None;
                    return std::task::Poll::Ready(Err(
                        crate::error::SignalFishError::TransportClosed,
                    ));
                }
            };
            self.pending_acquire = None;
            permit.forget();
            if let Some(frame) = frame.take() {
                let crate::transport::TransportFrame::Text(message) = frame else {
                    panic!("test mock expected an outbound text frame");
                };
                self.sent.lock().unwrap().push(message);
            }
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_recv(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<
            Option<Result<crate::transport::TransportFrame, crate::error::SignalFishError>>,
        > {
            if let Some(item) = self.incoming.pop_front() {
                std::task::Poll::Ready(
                    item.map(|result| result.map(crate::transport::TransportFrame::Text)),
                )
            } else {
                std::task::Poll::Pending
            }
        }
        fn poll_close(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), crate::error::SignalFishError>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// Regression test for the two #47-class hazards in the controller's
    /// signal relay: a driver signal refused by a full command queue must be
    /// buffered (not dropped), survive `recv()` cancellation, and go out
    /// exactly once when the congestion clears.
    #[tokio::test]
    async fn congestion_buffers_driver_signal_and_relays_exactly_once() {
        let peer = uuid(9);
        // One permit: exactly the Authenticate send is allowed through.
        let permits = Arc::new(tokio::sync::Semaphore::new(1));
        let entered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sent = Arc::new(Mutex::new(Vec::new()));
        let transport = GatedTransport {
            incoming: VecDeque::from(vec![
                Some(Ok(authed())),
                Some(Ok(protocol_info_v3())),
                Some(Ok(session_plan(peer, true))),
            ]),
            sent: Arc::clone(&sent),
            permits: Arc::clone(&permits),
            entered: Arc::clone(&entered),
            pending_acquire: None,
        };
        let driver = SharedDriver::default();
        let config = SignalFishConfig::new("app").with_command_channel_capacity(1);
        let mut mesh = MeshController::start(transport, config, driver.clone());

        // Pump until the SessionPlan is folded: the driver has been told to
        // connect and holds an Offer ready to relay (not yet drained).
        assert!(
            pump_until(&mut mesh, &driver, |calls| calls
                .iter()
                .any(|c| matches!(c, DriverCall::Connect(p, true) if *p == peer)))
            .await,
            "driver never told to connect"
        );

        // Saturate the capacity-1 command queue: the first filler is pulled
        // by the loop, which then parks inside the permit-less transport
        // send; the second filler occupies the queue's single slot.
        mesh.client_mut()
            .send_game_data(serde_json::json!({ "filler": 1 }))
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while entered.load(std::sync::atomic::Ordering::Acquire) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("transport loop never parked in the gated send");
        mesh.client_mut()
            .send_game_data(serde_json::json!({ "filler": 2 }))
            .unwrap();

        // Pump once (cancelled by timeout): the Offer is popped, refused by
        // the full queue, and buffered — cancellation must not lose it.
        let _ = tokio::time::timeout(std::time::Duration::from_millis(80), mesh.recv()).await;
        assert_eq!(
            sent_count(&sent, &[r#""type":"Signal""#]),
            0,
            "signal must not reach the wire while the queue is congested"
        );

        // Clear the congestion: the buffered signal must be relayed.
        permits.add_permits(64);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while sent_count(&sent, &[r#""type":"Signal""#]) == 0 {
                let _ =
                    tokio::time::timeout(std::time::Duration::from_millis(40), mesh.recv()).await;
            }
        })
        .await
        .expect("buffered signal was never relayed after congestion cleared");

        // A few extra pumps must not re-relay it.
        drain(&mut mesh, 3).await;
        assert_eq!(
            sent_count(&sent, &[r#""type":"Signal""#]),
            1,
            "the buffered signal must be relayed exactly once"
        );

        mesh.shutdown().await;
    }

    /// A buffered signal whose target peer is torn down (here: `PlayerLeft`
    /// while the command queue is congested) must be discarded, not relayed
    /// stale after the congestion clears.
    #[tokio::test]
    async fn peer_teardown_discards_buffered_signal_for_that_peer() {
        let peer = uuid(9);
        let player_left = serde_json::to_string(&ServerMessage::PlayerLeft {
            player_id: peer,
            epoch: Some(1),
            final_seq: Some(0),
        })
        .expect("PlayerLeft serializes");
        // One permit: exactly the Authenticate send is allowed through.
        let permits = Arc::new(tokio::sync::Semaphore::new(1));
        let entered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sent = Arc::new(Mutex::new(Vec::new()));
        let transport = GatedTransport {
            incoming: VecDeque::from(vec![
                Some(Ok(authed())),
                Some(Ok(protocol_info_v3())),
                Some(Ok(room_baseline(peer))),
                Some(Ok(session_plan(peer, true))),
                // Queued in the event channel behind the SessionPlan; the
                // controller folds it while the Offer sits buffered below.
                Some(Ok(player_left)),
            ]),
            sent: Arc::clone(&sent),
            permits: Arc::clone(&permits),
            entered: Arc::clone(&entered),
            pending_acquire: None,
        };
        let driver = SharedDriver::default();
        let config = SignalFishConfig::new("app").with_command_channel_capacity(1);
        let mut mesh = MeshController::start(transport, config, driver.clone());

        // Fold the SessionPlan (driver told to connect; Offer queued in the
        // driver, not yet drained). The PlayerLeft event is still undelivered.
        assert!(
            pump_until(&mut mesh, &driver, |calls| calls
                .iter()
                .any(|c| matches!(c, DriverCall::Connect(p, true) if *p == peer)))
            .await,
            "driver never told to connect"
        );

        // Saturate the capacity-1 command queue (loop parks in the gated
        // send; the second filler occupies the queue slot).
        mesh.client_mut()
            .send_game_data(serde_json::json!({ "filler": 1 }))
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while entered.load(std::sync::atomic::Ordering::Acquire) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("transport loop never parked in the gated send");
        mesh.client_mut()
            .send_game_data(serde_json::json!({ "filler": 2 }))
            .unwrap();

        // Pump: the Offer is popped, refused (queue full), and buffered; the
        // select then delivers PlayerLeft, whose teardown must clear the
        // buffered signal for that peer.
        let saw_player_left = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(MeshEvent::Signaling(ev)) = mesh.recv().await {
                    if matches!(*ev, SignalFishEvent::PlayerLeft { .. }) {
                        break;
                    }
                }
            }
        })
        .await;
        assert!(saw_player_left.is_ok(), "never surfaced PlayerLeft");
        assert!(
            driver
                .calls()
                .iter()
                .any(|c| matches!(c, DriverCall::Disconnect(p) if *p == peer)),
            "driver never told to disconnect the departed peer"
        );

        // Clear the congestion and keep pumping: the stale Offer must never
        // reach the wire.
        permits.add_permits(64);
        drain(&mut mesh, 5).await;
        assert_eq!(
            sent_count(&sent, &[r#""type":"Signal""#]),
            0,
            "a buffered signal for a torn-down peer must be discarded, not relayed"
        );

        mesh.shutdown().await;
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
        wait_for_sent_count(&sent, &["\"Signal\"", "local-sdp"], 1).await;
        wait_for_sent_count(&sent, &["TransportStatus", "webrtc", "true"], 1).await;
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
        // Relayed our answer to the server.
        wait_for_sent_count(&sent, &["\"Signal\"", "local-answer"], 1).await;
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
        let player_left = serde_json::to_string(&ServerMessage::PlayerLeft {
            player_id: peer,
            epoch: Some(1),
            final_seq: Some(0),
        })
        .unwrap();
        let (transport, _sent) = MockTransport::new(vec![
            Some(Ok(authed())),
            Some(Ok(protocol_info_v3())),
            Some(Ok(room_baseline(peer))),
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
        wait_for_sent_count(&sent, &["TransportStatus", "webrtc", "false"], 1).await;
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

    // ── Adversarial choreography tests (B1) ─────────────────────────

    #[tokio::test]
    async fn room_left_reports_transport_status_false_when_channel_open() {
        // Regression for the teardown asymmetry: once a peer's channel is open
        // (TransportStatus(true) was reported), leaving the room must route the
        // teardown through `mark_disconnected` so the 1->0 edge still reports
        // TransportStatus(false). Previously RoomLeft cleared `connected_peers`
        // directly and the server was never told WebRTC went down.
        let peer = uuid(31);
        let driver = SharedDriver::default();
        let room_left = serde_json::to_string(&ServerMessage::RoomLeft).unwrap();
        let (transport, sent) = MockTransport::new(vec![
            Some(Ok(authed())),
            Some(Ok(protocol_info_v3())),
            Some(Ok(session_plan(peer, true))),
            Some(Ok(signal_from(peer, serde_json::json!({ "Answer": "r" })))),
            Some(Ok(room_left)),
        ]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());

        // Drive the handshake to an open channel (reports TransportStatus(true)).
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            recv_until_peer_connected(&mut mesh),
        )
        .await
        .expect("timed out")
        .expect("stream closed");
        wait_for_sent_count(&sent, &["TransportStatus", "webrtc", "true"], 1).await;
        assert_eq!(
            sent_count(&sent, &["TransportStatus", "webrtc", "true"]),
            1,
            "precondition: channel-up reports status true once"
        );

        // Now reach the queued RoomLeft.
        pump_until(&mut mesh, &driver, |c| {
            c.contains(&DriverCall::Disconnect(peer))
        })
        .await;
        wait_for_sent_count(&sent, &["TransportStatus", "webrtc", "false"], 1).await;
        assert_eq!(
            sent_count(&sent, &["TransportStatus", "webrtc", "false"]),
            1,
            "RoomLeft with a live channel must report exactly one TransportStatus(false)"
        );
        mesh.shutdown().await;
    }

    #[tokio::test]
    async fn transport_status_flapping_reports_only_boundary_edges() {
        // Two answerer peers (no auto-handshake), then a hand-driven flap of
        // Connected/Disconnected. Only the 0->1 and 1->0 boundary edges report
        // status: two `true`s (each fresh 0->1) and one `false` (the single
        // 1->0), never one-per-peer.
        let a = uuid(41);
        let b = uuid(42);
        let driver = SharedDriver::default();
        let (transport, sent) = MockTransport::new(vec![
            Some(Ok(authed())),
            Some(Ok(protocol_info_v3())),
            Some(Ok(session_plan_multi(&[(a, false), (b, false)], &[]))),
        ]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());
        pump_until(&mut mesh, &driver, |c| {
            c.contains(&DriverCall::Connect(a, false)) && c.contains(&DriverCall::Connect(b, false))
        })
        .await;

        for ev in [
            DriverEvent::Connected { peer: a },    // 0->1 : true
            DriverEvent::Connected { peer: b },    // 1->2 : no report
            DriverEvent::Disconnected { peer: a }, // 2->1 : no report
            DriverEvent::Disconnected { peer: b }, // 1->0 : false
            DriverEvent::Connected { peer: a },    // 0->1 : true
        ] {
            driver.emit(ev);
        }
        for _ in 0..30 {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(15), mesh.recv()).await;
        }
        wait_for_sent_count(&sent, &["TransportStatus", "webrtc", "true"], 2).await;
        wait_for_sent_count(&sent, &["TransportStatus", "webrtc", "false"], 1).await;
        assert_eq!(
            sent_count(&sent, &["TransportStatus", "webrtc", "true"]),
            2,
            "exactly two 0->1 edges report true"
        );
        assert_eq!(
            sent_count(&sent, &["TransportStatus", "webrtc", "false"]),
            1,
            "exactly one 1->0 edge reports false"
        );
        mesh.shutdown().await;
    }

    #[tokio::test]
    async fn replan_keeps_survivors_drops_removed_and_obeys_initiate() {
        // Plan 1: {A(initiate), B(answer)}. Plan 2: {B(answer), C(initiate)}.
        // A is dropped (disconnect), B survives (connected once, never dropped),
        // C is newly connected. Every `initiate` flag is copied verbatim.
        let a = uuid(1);
        let b = uuid(2);
        let c = uuid(3);
        let driver = SharedDriver::default();
        let (transport, _sent) = MockTransport::new(vec![
            Some(Ok(authed())),
            Some(Ok(protocol_info_v3())),
            Some(Ok(session_plan_multi(
                &[(a, true), (b, false)],
                &["stun:1"],
            ))),
            Some(Ok(session_plan_multi(
                &[(b, false), (c, true)],
                &["stun:2"],
            ))),
        ]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());
        pump_until(&mut mesh, &driver, |c2| {
            c2.contains(&DriverCall::Connect(c, true))
        })
        .await;

        let calls = driver.calls();
        assert!(
            calls.contains(&DriverCall::Connect(a, true)),
            "A initiate=true verbatim"
        );
        assert!(
            calls.contains(&DriverCall::Connect(b, false)),
            "B initiate=false verbatim"
        );
        assert!(
            calls.contains(&DriverCall::Connect(c, true)),
            "C initiate=true verbatim"
        );
        assert!(
            calls.contains(&DriverCall::Disconnect(a)),
            "A dropped on re-plan"
        );
        assert_eq!(
            count_calls(
                &driver,
                |c2| matches!(c2, DriverCall::Connect(p, _) if *p == b)
            ),
            1,
            "survivor B connected exactly once"
        );
        assert_eq!(
            count_calls(
                &driver,
                |c2| matches!(c2, DriverCall::Disconnect(p) if *p == b)
            ),
            0,
            "survivor B never disconnected"
        );
        mesh.shutdown().await;
    }

    #[tokio::test]
    async fn plan_then_new_peer_for_same_peer_connects_once() {
        // A SessionPlan names a peer, then a NewPeer arrives for the SAME peer:
        // it must not be connected twice (idempotent on known peers).
        let p = uuid(10);
        let driver = SharedDriver::default();
        let (transport, _sent) = MockTransport::new(vec![
            Some(Ok(authed())),
            Some(Ok(protocol_info_v3())),
            Some(Ok(session_plan_multi(&[(p, true)], &["stun:a"]))),
            Some(Ok(new_peer_msg(p, true))),
        ]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());
        drain(&mut mesh, 12).await;
        assert_eq!(
            count_calls(
                &driver,
                |c| matches!(c, DriverCall::Connect(x, _) if *x == p)
            ),
            1,
            "SessionPlan + NewPeer for the same peer connects exactly once"
        );
        mesh.shutdown().await;
    }

    #[tokio::test]
    async fn repeated_identical_new_peer_connects_once() {
        // A NewPeer repeated with the SAME `you_initiate` is idempotent — the
        // live connection is kept, never re-driven. (A NewPeer that *changes*
        // the role is a different case, covered by
        // `new_peer_role_change_restarts_handshake`.)
        let p = uuid(4);
        let driver = SharedDriver::default();
        let (transport, _sent) = MockTransport::new(vec![
            Some(Ok(authed())),
            Some(Ok(protocol_info_v3())),
            Some(Ok(new_peer_msg(p, true))),
            Some(Ok(new_peer_msg(p, true))),
            Some(Ok(new_peer_msg(p, true))),
        ]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());
        drain(&mut mesh, 12).await;
        assert_eq!(
            count_calls(
                &driver,
                |c| matches!(c, DriverCall::Connect(x, _) if *x == p)
            ),
            1,
            "repeated identical NewPeer for the same peer connects exactly once"
        );
        assert_eq!(
            count_calls(
                &driver,
                |c| matches!(c, DriverCall::Disconnect(x) if *x == p)
            ),
            0,
            "an unchanged peer is never torn down"
        );
        mesh.shutdown().await;
    }

    #[tokio::test]
    async fn replan_role_flip_restarts_handshake_in_new_role() {
        // Regression for "stale initiate after replan": a *surviving* peer whose
        // `initiate` assignment FLIPS across a re-plan (a host re-election or
        // topology change reassigns who offers) must have its handshake
        // restarted in the new role. A stale offerer role would otherwise glare
        // (both sides offer) or stall (both wait). Plan 1 makes us the answerer
        // (initiate=false); Plan 2 flips us to the offerer (initiate=true) for
        // the SAME peer.
        let p = uuid(60);
        let driver = SharedDriver::default();
        let (transport, _sent) = MockTransport::new(vec![
            Some(Ok(authed())),
            Some(Ok(protocol_info_v3())),
            Some(Ok(session_plan(p, false))),
            Some(Ok(session_plan(p, true))),
        ]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());
        let flipped = pump_until(&mut mesh, &driver, |c| {
            c.contains(&DriverCall::Connect(p, true))
        })
        .await;
        assert!(
            flipped,
            "a survivor whose initiate flipped must be reconnected in the new role"
        );
        let calls = driver.calls();
        assert!(
            calls.contains(&DriverCall::Connect(p, false)),
            "the first plan connects us as the answerer"
        );
        assert!(
            calls.contains(&DriverCall::Disconnect(p)),
            "the stale role is torn down before restarting"
        );
        assert!(
            calls.contains(&DriverCall::Connect(p, true)),
            "the flipped plan reconnects us as the offerer"
        );
        assert_eq!(
            count_calls(
                &driver,
                |c| matches!(c, DriverCall::Connect(x, _) if *x == p)
            ),
            2,
            "exactly one restart: connect(answerer) then connect(offerer)"
        );
        mesh.shutdown().await;
    }

    #[tokio::test]
    async fn new_peer_role_change_restarts_handshake() {
        // The same stale-role hazard via NewPeer: a later NewPeer for a known
        // peer that CHANGES `you_initiate` must restart the handshake in the new
        // role. (`MeshSession` already adopts the latest flag — latest wins — so
        // the controller must drive the driver to match.)
        let p = uuid(61);
        let driver = SharedDriver::default();
        let (transport, _sent) = MockTransport::new(vec![
            Some(Ok(authed())),
            Some(Ok(protocol_info_v3())),
            Some(Ok(new_peer_msg(p, false))),
            Some(Ok(new_peer_msg(p, true))),
        ]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());
        let flipped = pump_until(&mut mesh, &driver, |c| {
            c.contains(&DriverCall::Connect(p, true))
        })
        .await;
        assert!(flipped, "a NewPeer role change must restart the handshake");
        let calls = driver.calls();
        assert!(calls.contains(&DriverCall::Connect(p, false)));
        assert!(calls.contains(&DriverCall::Disconnect(p)));
        assert_eq!(
            count_calls(
                &driver,
                |c| matches!(c, DriverCall::Connect(x, _) if *x == p)
            ),
            2,
            "exactly one restart on the role change"
        );
        mesh.shutdown().await;
    }

    #[tokio::test]
    async fn replan_role_flip_while_channel_open_rechurns_transport_status() {
        // The key consequence of restart-on-flip: if the peer's channel was
        // already OPEN, the restart's teardown must report the 1->0
        // TransportStatus edge (so the server learns WebRTC went down before the
        // role swap), and a re-handshake would report 0->1 again. Drive P to an
        // open channel as the initiator, then flip it to the answerer and assert
        // exactly one TransportStatus(true) followed by exactly one (false).
        let p = uuid(62);
        let driver = SharedDriver::default();
        let (transport, sent) = MockTransport::new(vec![
            Some(Ok(authed())),
            Some(Ok(protocol_info_v3())),
            Some(Ok(session_plan(p, true))),
            Some(Ok(signal_from(p, serde_json::json!({ "Answer": "r" })))),
            Some(Ok(session_plan(p, false))),
        ]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());

        // Drive the handshake to an open channel (reports TransportStatus(true)).
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            recv_until_peer_connected(&mut mesh),
        )
        .await
        .expect("timed out")
        .expect("stream closed");
        wait_for_sent_count(&sent, &["TransportStatus", "webrtc", "true"], 1).await;
        assert_eq!(
            sent_count(&sent, &["TransportStatus", "webrtc", "true"]),
            1,
            "precondition: the open channel reports status true once"
        );

        // Reach the queued role-flip plan; the restart tears the open channel
        // down (1->0) and reconnects P as the answerer.
        pump_until(&mut mesh, &driver, |c| {
            c.contains(&DriverCall::Connect(p, false))
        })
        .await;
        wait_for_sent_count(&sent, &["TransportStatus", "webrtc", "false"], 1).await;
        let calls = driver.calls();
        assert!(
            calls.contains(&DriverCall::Disconnect(p)),
            "the open channel's stale role is torn down"
        );
        assert!(
            calls.contains(&DriverCall::Connect(p, false)),
            "P is reconnected as the answerer"
        );
        assert_eq!(
            sent_count(&sent, &["TransportStatus", "webrtc", "false"]),
            1,
            "the restart's 1->0 edge reports exactly one TransportStatus(false)"
        );
        mesh.shutdown().await;
    }

    #[tokio::test]
    async fn replan_simultaneously_flips_survivor_drops_one_and_adds_one() {
        // One re-plan exercising all three paths at once: A survives with a
        // FLIPPED role (restart), B is dropped (disconnect), C is newly added.
        // Pins the interaction of the drop loop, the `retain`, and the
        // `ensure_peer` loop within a single SessionPlan.
        let a = uuid(70);
        let b = uuid(71);
        let c = uuid(72);
        let driver = SharedDriver::default();
        let (transport, _sent) = MockTransport::new(vec![
            Some(Ok(authed())),
            Some(Ok(protocol_info_v3())),
            Some(Ok(session_plan_multi(
                &[(a, false), (b, true)],
                &["stun:1"],
            ))),
            Some(Ok(session_plan_multi(
                &[(a, true), (c, false)],
                &["stun:2"],
            ))),
        ]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());
        pump_until(&mut mesh, &driver, |calls| {
            calls.contains(&DriverCall::Connect(c, false))
        })
        .await;
        let calls = driver.calls();
        // A: flipped survivor → restarted (answerer then offerer), exactly twice.
        assert!(
            calls.contains(&DriverCall::Connect(a, false)),
            "A first connected as answerer"
        );
        assert!(
            calls.contains(&DriverCall::Disconnect(a)),
            "A's stale role torn down"
        );
        assert!(
            calls.contains(&DriverCall::Connect(a, true)),
            "A reconnected as offerer"
        );
        assert_eq!(
            count_calls(
                &driver,
                |c2| matches!(c2, DriverCall::Connect(p, _) if *p == a)
            ),
            2,
            "A connected exactly twice (one restart)"
        );
        // B: dropped from the new plan → disconnected, connected only by plan 1.
        assert!(
            calls.contains(&DriverCall::Disconnect(b)),
            "B dropped on re-plan"
        );
        assert_eq!(
            count_calls(
                &driver,
                |c2| matches!(c2, DriverCall::Connect(p, _) if *p == b)
            ),
            1,
            "B connected only by the first plan"
        );
        // C: newly named → connected exactly once.
        assert_eq!(
            count_calls(
                &driver,
                |c2| matches!(c2, DriverCall::Connect(p, _) if *p == c)
            ),
            1,
            "C newly connected exactly once"
        );
        mesh.shutdown().await;
    }

    #[tokio::test]
    async fn unrecognized_signal_shape_is_dropped_without_panic() {
        // A signal whose JSON shape is not a PeerSignal must be dropped (warn) and
        // never reach the driver or panic; a subsequent valid signal still does.
        let p = uuid(5);
        let driver = SharedDriver::default();
        let (transport, _sent) = MockTransport::new(vec![
            Some(Ok(authed())),
            Some(Ok(protocol_info_v3())),
            Some(Ok(session_plan(p, false))),
            Some(Ok(signal_from(p, serde_json::json!({ "Bogus": "x" })))),
            Some(Ok(signal_from(p, serde_json::json!({ "Offer": "ok" })))),
        ]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());
        pump_until(&mut mesh, &driver, |c| {
            c.contains(&DriverCall::OnSignal(p, PeerSignal::Offer("ok".into())))
        })
        .await;
        let calls = driver.calls();
        assert!(
            !calls
                .iter()
                .any(|c| matches!(c, DriverCall::OnSignal(_, PeerSignal::Offer(o)) if o == "x")),
            "the bogus signal must be dropped"
        );
        assert!(calls.contains(&DriverCall::OnSignal(p, PeerSignal::Offer("ok".into()))));
        mesh.shutdown().await;
    }

    #[tokio::test]
    async fn empty_ice_in_later_plan_resets_driver_ice() {
        // A later authoritative plan with an empty ICE list clears prior state.
        let p = uuid(9);
        let q = uuid(99);
        let driver = SharedDriver::default();
        let (transport, _sent) = MockTransport::new(vec![
            Some(Ok(authed())),
            Some(Ok(protocol_info_v3())),
            Some(Ok(session_plan_multi(&[(p, false)], &["stun:real"]))),
            Some(Ok(session_plan_multi(&[(p, false), (q, false)], &[]))),
        ]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());
        pump_until(&mut mesh, &driver, |c| {
            c.contains(&DriverCall::Connect(q, false))
        })
        .await;
        assert_eq!(
            count_calls(&driver, |c| matches!(c, DriverCall::SetIceServers(0))),
            1,
            "controller must push one authoritative empty ICE reset"
        );
        assert_eq!(
            count_calls(&driver, |c| matches!(c, DriverCall::SetIceServers(1))),
            1,
            "the one non-empty ICE set is applied exactly once"
        );
        mesh.shutdown().await;
    }

    #[tokio::test]
    async fn new_peer_only_peer_is_torn_down_by_later_plan_omitting_it() {
        // A peer that arrived only via NewPeer (never in a plan) must still be torn
        // down by a later SessionPlan that omits it (no known/connected desync).
        let p = uuid(15);
        let q = uuid(16);
        let driver = SharedDriver::default();
        let (transport, _sent) = MockTransport::new(vec![
            Some(Ok(authed())),
            Some(Ok(protocol_info_v3())),
            Some(Ok(new_peer_msg(p, true))),
            Some(Ok(session_plan_multi(&[(q, true)], &["stun:a"]))),
        ]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());
        let ok = pump_until(&mut mesh, &driver, |c| {
            c.contains(&DriverCall::Disconnect(p)) && c.contains(&DriverCall::Connect(q, true))
        })
        .await;
        assert!(
            ok,
            "a NewPeer-only peer absent from a later plan must be disconnected"
        );
        mesh.shutdown().await;
    }

    #[tokio::test]
    async fn reconnect_replays_missed_events_and_drives_driver() {
        // A reconnect whose `missed_events` batch a SessionPlan + NewPeer (instead
        // of the server re-sending a live plan) must still drive the driver to
        // connect those peers — keeping the client correct across server impls.
        use crate::protocol::{SessionPeer, SessionPlanPayload, Topology, TransportKind};
        let a = uuid(51);
        let b = uuid(52);
        let driver = SharedDriver::default();
        let plan = ServerMessage::SessionPlan(Box::new(SessionPlanPayload {
            topology: Topology::Mesh,
            transport: TransportKind::WebRtc,
            host: None,
            peers: vec![SessionPeer {
                player_id: a,
                player_name: "A".into(),
                is_authority: false,
                initiate: true,
            }],
            ice_servers: vec![],
            fallback: TransportKind::Relay,
        }));
        let new_peer = ServerMessage::NewPeer {
            peer_id: b,
            you_initiate: false,
        };
        let (transport, _sent) = MockTransport::new(vec![
            Some(Ok(authed())),
            Some(Ok(protocol_info_v3())),
            Some(Ok(reconnected_with_missed(vec![plan, new_peer]))),
        ]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone());
        let ok = pump_until(&mut mesh, &driver, |c| {
            c.contains(&DriverCall::Connect(a, true)) && c.contains(&DriverCall::Connect(b, false))
        })
        .await;
        assert!(
            ok,
            "missed SessionPlan/NewPeer must drive connect with verbatim initiate flags"
        );
        // The replayed plan is also reflected in the session view.
        assert_eq!(mesh.session().topology(), Some(Topology::Mesh));
        mesh.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn waker_surfaces_driver_output_without_waiting_for_pump() {
        // With a deliberately huge pump interval, driver output produced while
        // recv() is parked must still surface promptly because the driver wakes
        // the controller via its MeshWaker (no up-to-one-pump-interval latency).
        let driver = SharedDriver::default();
        let (transport, _sent) = MockTransport::new(vec![Some(Ok(authed()))]);
        let mut mesh =
            MeshController::start(transport, SignalFishConfig::new("app"), driver.clone())
                .with_pump_interval(std::time::Duration::from_secs(30));

        // Drain the initial signaling events so the next recv() genuinely parks.
        while (tokio::time::timeout(std::time::Duration::from_millis(60), mesh.recv()).await)
            .is_ok()
        {}

        // Produce driver data AND wake from another task after recv() has parked.
        let d2 = driver.clone();
        let waker_task = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            d2.emit_and_wake(DriverEvent::Data {
                peer: uuid(1),
                data: vec![7],
            });
        });

        let start = std::time::Instant::now();
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), mesh.recv())
            .await
            .expect("recv must surface waker-signaled data well before the 30s pump");
        let elapsed = start.elapsed();
        waker_task.await.unwrap();

        assert!(
            matches!(got, Some(MeshEvent::Data { .. })),
            "should surface the driver data, got {got:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "waker must surface output promptly (well under one 30s pump), took {elapsed:?}"
        );
        mesh.shutdown().await;
    }
}
