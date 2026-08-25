//! Optional zero-dependency mesh session tracker (protocol v3).
//!
//! [`MeshSession`] folds the v3 [`SignalFishEvent`]s into an always-consistent
//! view of the current peer-to-peer session: the chosen topology/transport, the
//! peers this client should connect to (each with its server-assigned `initiate`
//! flag and selected-path liveness), the elected host, and the ICE servers. It does
//! the fiddly bookkeeping — late joins, host re-election, and reconnect replay —
//! correctly and idempotently, so consumers don't each re-implement it.
//!
//! It contains **no WebRTC**, no I/O, and no threads: drive it by calling
//! [`apply`](MeshSession::apply) on every event you receive, then read the
//! accessors. The client still "obeys the server" — every `initiate` flag is
//! copied verbatim from the server, never computed here.
//!
//! Enabled by the `mesh` feature.

use crate::event::SignalFishEvent;
use crate::protocol::{
    DirectEndpoint, IceServer, PlayerId, SessionGeneration, Topology, TransportKind,
};

/// A peer within a [`MeshSession`], enriched with selected-path liveness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshPeer {
    /// The peer's identifier.
    pub player_id: PlayerId,
    /// The peer's display name (empty until a `SessionPlan` names it).
    pub player_name: String,
    /// Whether the peer is the session's authoritative host.
    pub is_authority: bool,
    /// Whether **this client** sends the WebRTC offer to this peer
    /// (server-assigned; obey verbatim).
    pub initiate: bool,
    /// Last-known liveness reported for the session's selected transport.
    /// Status for any other transport is ignored.
    pub connected: bool,
}

/// An always-consistent view of the current mesh/host/relay session, folded
/// purely from [`SignalFishEvent`]s. See the [module docs](crate::mesh).
#[derive(Debug, Clone, Default)]
pub struct MeshSession {
    generation: Option<SessionGeneration>,
    topology: Option<Topology>,
    transport: Option<TransportKind>,
    fallback: Option<TransportKind>,
    host: Option<PlayerId>,
    direct_endpoint: Option<DirectEndpoint>,
    peers: Vec<MeshPeer>,
    ice_servers: Vec<IceServer>,
}

impl MeshSession {
    /// Create an empty session tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one event into the session. Returns `true` if applying the event
    /// changed the view (handy for deciding whether to redraw or re-evaluate
    /// connections); a [`SessionPlan`](SignalFishEvent::SessionPlan) always
    /// returns `true` because it re-asserts the authoritative plan.
    ///
    /// Irrelevant events — and no-ops such as a status for an unknown peer or a
    /// redundant `NewPeer`/`PeerTransportStatus` — return `false`. Applying the
    /// same sequence twice yields the same state as applying it once (idempotent
    /// under reconnect replay).
    pub fn apply(&mut self, event: &SignalFishEvent) -> bool {
        match event {
            SignalFishEvent::SessionPlan {
                generation,
                topology,
                transport,
                host,
                direct_endpoint,
                peers,
                ice_servers,
                fallback,
            } => {
                let selected_path_changed =
                    self.generation != *generation || self.transport != Some(*transport);
                self.generation = *generation;
                self.topology = Some(*topology);
                self.transport = Some(*transport);
                self.fallback = Some(*fallback);
                self.host = *host;
                self.direct_endpoint = direct_endpoint.clone();
                // A plan fully REPLACES the peer set (handles host re-election
                // and topology change). Surviving liveness is preserved only
                // when generation, selected transport, and offerer role are
                // unchanged; peers absent from the new plan are dropped.
                self.peers = peers
                    .iter()
                    .map(|p| MeshPeer {
                        player_id: p.player_id,
                        player_name: p.player_name.clone(),
                        is_authority: p.is_authority,
                        initiate: p.initiate,
                        connected: !selected_path_changed
                            && self.peer(p.player_id).is_some_and(|existing| {
                                existing.connected && existing.initiate == p.initiate
                            }),
                    })
                    .collect();
                // Every SessionPlan is authoritative. In particular, an
                // explicit relay/relay plan clears stale WebRTC ICE state.
                self.ice_servers = ice_servers.clone();
                true
            }
            SignalFishEvent::NewPeer {
                peer_id,
                you_initiate,
            } => {
                // Late joiner: upsert by id (idempotent; latest flag wins).
                if let Some(existing) = self.peers.iter_mut().find(|p| p.player_id == *peer_id) {
                    let changed = existing.initiate != *you_initiate;
                    existing.initiate = *you_initiate;
                    if changed {
                        // The controller restarts the handshake when the server
                        // changes the offerer role, so prior liveness is stale.
                        existing.connected = false;
                    }
                    changed
                } else {
                    self.peers.push(MeshPeer {
                        player_id: *peer_id,
                        player_name: String::new(),
                        is_authority: false,
                        initiate: *you_initiate,
                        connected: false,
                    });
                    true
                }
            }
            SignalFishEvent::PeerTransportStatus {
                peer_id,
                transport,
                connected,
            } => {
                if self.transport != Some(*transport) {
                    return false;
                }
                // Only mutate liveness; never invent a peer the server's plan
                // didn't include.
                if let Some(p) = self.peers.iter_mut().find(|p| p.player_id == *peer_id) {
                    let changed = p.connected != *connected;
                    p.connected = *connected;
                    changed
                } else {
                    false
                }
            }
            // A departing player is dropped immediately so peers() never
            // advertises someone who has left (the server also re-plans on
            // membership change, but this closes the window in between).
            // Removal needs no server authority, so it is safe to fold here.
            SignalFishEvent::PlayerLeft { player_id, .. } => {
                let before = self.peers.len();
                self.peers.retain(|p| p.player_id != *player_id);
                let host_departed = self.host == Some(*player_id);
                if host_departed {
                    // The server elects and replans a replacement host on
                    // departure, but the next SessionPlan owns that decision.
                    // Until it lands, a departed host must not stay reachable
                    // through host()/direct_endpoint().
                    self.host = None;
                    self.direct_endpoint = None;
                }
                self.peers.len() != before || host_departed
            }
            // ICE pre-gather: seed the ICE servers during the lobby wait. Do not
            // create peers here — a relay-floor room may never produce a plan.
            SignalFishEvent::RoomJoined { ice_servers, .. } => self.apply_pre_gather(ice_servers),
            // Reconnect is a hard plan boundary. Server 0.7 publishes a fresh
            // live SessionPlan after this baseline; SessionPlan and other mesh
            // controls are not valid replay entries. Clear the prior plan and
            // peer set immediately so stale topology cannot remain actionable
            // during the gap, then seed the refreshed pre-gather ICE servers.
            SignalFishEvent::Reconnected { ice_servers, .. } => {
                let had_authoritative_state = self.generation.is_some()
                    || self.topology.is_some()
                    || self.transport.is_some()
                    || self.fallback.is_some()
                    || self.host.is_some()
                    || self.direct_endpoint.is_some()
                    || !self.peers.is_empty();
                let ice_changed = self.ice_servers != *ice_servers;
                *self = Self::default();
                let _ = self.apply_pre_gather(ice_servers);
                had_authoritative_state || ice_changed
            }
            // The session is over.
            SignalFishEvent::RoomLeft
            | SignalFishEvent::SpectatorJoined { .. }
            | SignalFishEvent::SpectatorLeft { .. }
            | SignalFishEvent::Disconnected { .. } => {
                let had_state = self.topology.is_some()
                    || !self.peers.is_empty()
                    || !self.ice_servers.is_empty();
                *self = Self::default();
                had_state
            }
            // Every other event is irrelevant to mesh bookkeeping. (A wildcard is
            // intentional here: the tracker folds only the mesh-relevant events.)
            _ => false,
        }
    }

    /// Fold an ICE pre-gather set (from `RoomJoined`/`Reconnected`). An empty set
    /// preserves the existing one and an identical set is a no-op; either way it
    /// reports `false` so `apply` only signals a real change.
    fn apply_pre_gather(&mut self, ice_servers: &[IceServer]) -> bool {
        if ice_servers.is_empty() || self.ice_servers.as_slice() == ice_servers {
            false
        } else {
            self.ice_servers = ice_servers.to_vec();
            true
        }
    }

    /// The chosen session topology, or `None` before any plan.
    #[must_use]
    pub fn topology(&self) -> Option<Topology> {
        self.topology
    }

    /// The latest authoritative session-plan generation.
    ///
    /// `None` before a plan and for legacy Server 0.4 protocol-v3 plans.
    #[must_use]
    pub fn generation(&self) -> Option<SessionGeneration> {
        self.generation
    }

    /// The chosen data-path transport, or `None` before any plan.
    #[must_use]
    pub fn transport(&self) -> Option<TransportKind> {
        self.transport
    }

    /// The universal fallback transport (always relay), or `None` before any plan.
    #[must_use]
    pub fn fallback(&self) -> Option<TransportKind> {
        self.fallback
    }

    /// The elected host (host topology only).
    #[must_use]
    pub fn host(&self) -> Option<PlayerId> {
        self.host
    }

    /// The validated connect target for a `host + direct` plan.
    #[must_use]
    pub fn direct_endpoint(&self) -> Option<&DirectEndpoint> {
        self.direct_endpoint.as_ref()
    }

    /// The peers this client should connect to.
    #[must_use]
    pub fn peers(&self) -> &[MeshPeer] {
        &self.peers
    }

    /// The ICE (STUN/TURN) servers for WebRTC (pre-gathered or from the plan).
    #[must_use]
    pub fn ice_servers(&self) -> &[IceServer] {
        &self.ice_servers
    }

    /// Look up a peer by id.
    #[must_use]
    pub fn peer(&self, player_id: PlayerId) -> Option<&MeshPeer> {
        self.peers.iter().find(|p| p.player_id == player_id)
    }

    /// Returns `true` once a non-relay (host or mesh) plan is in effect — i.e.
    /// the consumer should be establishing peer-to-peer connections.
    #[must_use]
    pub fn is_p2p(&self) -> bool {
        matches!(self.topology, Some(Topology::Host | Topology::Mesh))
    }
}

#[cfg(test)]
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
    use crate::protocol::SessionPeer;

    fn uuid(n: u128) -> PlayerId {
        uuid::Uuid::from_u128(n)
    }

    fn peer(id: u128, initiate: bool) -> SessionPeer {
        SessionPeer {
            player_id: uuid(id),
            player_name: format!("P{id}"),
            is_authority: false,
            initiate,
        }
    }

    fn ice(url: &str) -> IceServer {
        IceServer {
            urls: vec![url.into()],
            username: None,
            credential: None,
        }
    }

    fn plan(
        topology: Topology,
        host: Option<PlayerId>,
        peers: Vec<SessionPeer>,
        ice_servers: Vec<IceServer>,
    ) -> SignalFishEvent {
        SignalFishEvent::SessionPlan {
            generation: None,
            topology,
            transport: TransportKind::WebRtc,
            host,
            direct_endpoint: None,
            peers,
            ice_servers,
            fallback: TransportKind::Relay,
        }
    }

    #[test]
    fn empty_default() {
        let s = MeshSession::new();
        assert!(s.peers().is_empty());
        assert!(s.topology().is_none());
        assert!(!s.is_p2p());
    }

    #[test]
    fn applies_plan() {
        let mut s = MeshSession::new();
        let changed = s.apply(&plan(
            Topology::Mesh,
            None,
            vec![peer(1, true), peer(2, false)],
            vec![ice("stun:a")],
        ));
        assert!(changed);
        assert_eq!(s.topology(), Some(Topology::Mesh));
        assert_eq!(s.transport(), Some(TransportKind::WebRtc));
        assert_eq!(s.fallback(), Some(TransportKind::Relay));
        assert!(s.is_p2p());
        assert_eq!(s.peers().len(), 2);
        assert!(s.peer(uuid(1)).unwrap().initiate);
        assert!(!s.peer(uuid(2)).unwrap().initiate);
        assert_eq!(s.ice_servers(), &[ice("stun:a")]);
    }

    #[test]
    fn debug_redacts_nested_ice_credentials() {
        let secret = "mesh-turn-secret";
        let mut s = MeshSession::new();
        s.apply(&plan(
            Topology::Mesh,
            None,
            vec![peer(1, true)],
            vec![IceServer {
                urls: vec![format!("turn://user:{secret}@relay.example")],
                username: Some("mesh-user".into()),
                credential: Some(secret.into()),
            }],
        ));

        let output = format!("{s:?}");
        assert!(!output.contains(secret), "debug output leaked: {output}");
        assert!(
            !output.contains("mesh-user"),
            "debug output leaked: {output}"
        );
    }

    #[test]
    fn exposes_generation_and_direct_endpoint() {
        let mut s = MeshSession::new();
        let generation = uuid(12);
        let endpoint = DirectEndpoint {
            host: "203.0.113.8".into(),
            port: 7000,
        };
        s.apply(&SignalFishEvent::SessionPlan {
            generation: Some(generation),
            topology: Topology::Host,
            transport: TransportKind::Direct,
            host: Some(uuid(1)),
            direct_endpoint: Some(endpoint.clone()),
            peers: vec![],
            ice_servers: vec![],
            fallback: TransportKind::Relay,
        });
        assert_eq!(s.generation(), Some(generation));
        assert_eq!(s.direct_endpoint(), Some(&endpoint));
    }

    #[test]
    fn generation_change_clears_surviving_peer_liveness() {
        let mut s = MeshSession::new();
        for generation in [uuid(12), uuid(13)] {
            s.apply(&SignalFishEvent::SessionPlan {
                generation: Some(generation),
                topology: Topology::Mesh,
                transport: TransportKind::WebRtc,
                host: None,
                direct_endpoint: None,
                peers: vec![peer(1, false)],
                ice_servers: vec![],
                fallback: TransportKind::Relay,
            });
            if generation == uuid(12) {
                s.apply(&SignalFishEvent::PeerTransportStatus {
                    peer_id: uuid(1),
                    transport: TransportKind::WebRtc,
                    connected: true,
                });
                assert!(s.peer(uuid(1)).unwrap().connected);
            }
        }
        assert!(!s.peer(uuid(1)).unwrap().connected);
    }

    #[test]
    fn replan_replaces_peers_and_ice_not_merges() {
        // Host re-election: plan A then plan B with a new host and a different
        // peer set. Peers and ICE are replaced wholesale, not merged.
        let mut s = MeshSession::new();
        s.apply(&plan(
            Topology::Host,
            Some(uuid(1)),
            vec![peer(1, false), peer(2, true)],
            vec![ice("stun:a")],
        ));
        // Mark peer 2 connected, then re-plan keeping peer 2 but dropping peer 1.
        s.apply(&SignalFishEvent::PeerTransportStatus {
            peer_id: uuid(2),
            transport: TransportKind::WebRtc,
            connected: true,
        });
        s.apply(&plan(
            Topology::Host,
            Some(uuid(3)),
            vec![peer(2, false), peer(3, true)],
            vec![ice("stun:b")],
        ));
        assert_eq!(s.host(), Some(uuid(3)));
        assert!(s.peer(uuid(1)).is_none(), "peer 1 dropped on re-plan");
        assert!(s.peer(uuid(3)).is_some());
        // The controller restarts peer 2 because its offerer role changed, so
        // selected-path liveness must reset until that new handshake connects.
        assert!(!s.peer(uuid(2)).unwrap().connected);
        // Its `initiate` is taken from the NEW plan.
        assert!(!s.peer(uuid(2)).unwrap().initiate);
        // ICE replaced, not merged.
        assert_eq!(s.ice_servers(), &[ice("stun:b")]);
    }

    #[test]
    fn duplicate_new_peer_is_idempotent() {
        let mut s = MeshSession::new();
        s.apply(&SignalFishEvent::NewPeer {
            peer_id: uuid(5),
            you_initiate: true,
        });
        s.apply(&SignalFishEvent::NewPeer {
            peer_id: uuid(5),
            you_initiate: true,
        });
        assert_eq!(s.peers().len(), 1);
        assert!(s.peer(uuid(5)).unwrap().initiate);
    }

    #[test]
    fn new_peer_for_known_peer_updates_latest_wins() {
        let mut s = MeshSession::new();
        s.apply(&plan(Topology::Mesh, None, vec![peer(2, true)], vec![]));
        s.apply(&SignalFishEvent::PeerTransportStatus {
            peer_id: uuid(2),
            transport: TransportKind::WebRtc,
            connected: true,
        });
        assert!(s.peer(uuid(2)).unwrap().connected);
        // A later NewPeer for the same id overrides the initiate flag.
        s.apply(&SignalFishEvent::NewPeer {
            peer_id: uuid(2),
            you_initiate: false,
        });
        assert_eq!(s.peers().len(), 1);
        assert!(!s.peer(uuid(2)).unwrap().initiate);
        assert!(
            !s.peer(uuid(2)).unwrap().connected,
            "offerer-role changes restart the selected-path handshake"
        );
    }

    #[test]
    fn transport_status_unknown_peer_ignored() {
        let mut s = MeshSession::new();
        s.apply(&plan(Topology::Mesh, None, vec![peer(1, true)], vec![]));
        let changed = s.apply(&SignalFishEvent::PeerTransportStatus {
            peer_id: uuid(99),
            transport: TransportKind::WebRtc,
            connected: true,
        });
        assert!(!changed);
        assert_eq!(s.peers().len(), 1);
        assert!(s.peer(uuid(99)).is_none());
    }

    #[test]
    fn transport_status_updates_liveness_not_initiate() {
        let mut s = MeshSession::new();
        s.apply(&plan(Topology::Mesh, None, vec![peer(1, true)], vec![]));
        s.apply(&SignalFishEvent::PeerTransportStatus {
            peer_id: uuid(1),
            transport: TransportKind::WebRtc,
            connected: true,
        });
        let p = s.peer(uuid(1)).unwrap();
        assert!(p.connected);
        assert!(p.initiate, "initiate is server-authoritative, untouched");
    }

    #[test]
    fn liveness_tracks_only_the_selected_transport_across_plan_transitions() {
        let mut session = MeshSession::new();
        let peer_id = uuid(1);

        session.apply(&plan(
            Topology::Host,
            Some(peer_id),
            vec![peer(1, false)],
            vec![],
        ));
        assert!(!session.apply(&SignalFishEvent::PeerTransportStatus {
            peer_id,
            transport: TransportKind::Direct,
            connected: true,
        }));
        assert!(!session.peer(peer_id).unwrap().connected);
        assert!(session.apply(&SignalFishEvent::PeerTransportStatus {
            peer_id,
            transport: TransportKind::WebRtc,
            connected: true,
        }));
        assert!(session.peer(peer_id).unwrap().connected);

        session.apply(&SignalFishEvent::SessionPlan {
            generation: None,
            topology: Topology::Host,
            transport: TransportKind::Direct,
            host: Some(peer_id),
            direct_endpoint: Some(DirectEndpoint {
                host: "192.0.2.1".into(),
                port: 7_777,
            }),
            peers: vec![peer(1, false)],
            ice_servers: vec![],
            fallback: TransportKind::Relay,
        });
        assert!(!session.peer(peer_id).unwrap().connected);
        assert!(!session.apply(&SignalFishEvent::PeerTransportStatus {
            peer_id,
            transport: TransportKind::WebRtc,
            connected: true,
        }));
        assert!(session.apply(&SignalFishEvent::PeerTransportStatus {
            peer_id,
            transport: TransportKind::Direct,
            connected: true,
        }));
        assert!(session.peer(peer_id).unwrap().connected);

        session.apply(&SignalFishEvent::SessionPlan {
            generation: None,
            topology: Topology::Relay,
            transport: TransportKind::Relay,
            host: None,
            direct_endpoint: None,
            peers: vec![],
            ice_servers: vec![],
            fallback: TransportKind::Relay,
        });
        assert!(session.peers().is_empty());
        assert!(!session.is_p2p());
    }

    #[test]
    fn pre_gather_ice_then_plan_precedence() {
        let mut s = MeshSession::new();
        // RoomJoined seeds pre-gathered ICE.
        s.apply(&room_joined(vec![ice("stun:pre")]));
        assert_eq!(s.ice_servers(), &[ice("stun:pre")]);
        assert!(s.topology().is_none(), "pre-gather creates no plan/peers");
        // A plan with empty ICE authoritatively clears pre-gather state.
        s.apply(&plan(Topology::Mesh, None, vec![peer(1, true)], vec![]));
        assert!(s.ice_servers().is_empty());
        // A plan with ICE overrides it.
        s.apply(&plan(
            Topology::Mesh,
            None,
            vec![peer(1, true)],
            vec![ice("stun:plan")],
        ));
        assert_eq!(s.ice_servers(), &[ice("stun:plan")]);
    }

    #[test]
    fn pre_gather_ice_reapply_identical_reports_no_change() {
        // Re-applying RoomJoined/Reconnected with an ICE set identical to the
        // one already held must report `changed == false` — `apply` returns true
        // only when the view actually changes (avoids spurious redraws /
        // connection re-evaluation on a duplicate or echoed pre-gather).
        let mut s = MeshSession::new();
        assert!(s.apply(&room_joined(vec![ice("stun:a")])));
        assert!(
            !s.apply(&room_joined(vec![ice("stun:a")])),
            "identical pre-gather ICE must not report a change"
        );
        // A genuinely different set still reports a change.
        assert!(s.apply(&room_joined(vec![ice("stun:b")])));
        assert_eq!(s.ice_servers(), &[ice("stun:b")]);
    }

    #[test]
    fn room_and_spectator_transitions_reset_authoritative_mesh_state() {
        for terminal in [
            SignalFishEvent::Disconnected {
                reason: None,
                last_server_error: None,
            },
            SignalFishEvent::RoomLeft,
            SignalFishEvent::SpectatorJoined {
                room_id: uuid(10),
                room_code: "WATCH".into(),
                spectator_id: uuid(11),
                game_name: "g".into(),
                current_players: vec![],
                current_spectators: vec![],
                lobby_state: crate::protocol::LobbyState::Waiting,
                reason: None,
            },
            SignalFishEvent::SpectatorLeft {
                room_id: Some(uuid(10)),
                room_code: Some("WATCH".into()),
                reason: None,
                current_spectators: vec![],
            },
        ] {
            let mut s = MeshSession::new();
            s.apply(&plan(
                Topology::Mesh,
                None,
                vec![peer(1, true)],
                vec![ice("stun:a")],
            ));
            assert!(s.is_p2p());
            let changed = s.apply(&terminal);
            assert!(changed);
            assert!(s.peers().is_empty());
            assert!(s.topology().is_none());
            assert!(s.ice_servers().is_empty());
            // Reset is idempotent (no further change).
            assert!(!s.apply(&terminal));
        }
    }

    #[test]
    fn ignores_unrelated_events() {
        let mut s = MeshSession::new();
        assert!(!s.apply(&SignalFishEvent::Pong));
        assert!(!s.apply(&SignalFishEvent::Connected));
        assert!(s.peers().is_empty());
    }

    #[test]
    fn player_left_drops_peer_immediately() {
        let mut s = MeshSession::new();
        s.apply(&plan(
            Topology::Mesh,
            None,
            vec![peer(1, true), peer(2, false)],
            vec![],
        ));
        // A departing player is removed right away (no waiting for a re-plan).
        let changed = s.apply(&SignalFishEvent::PlayerLeft {
            player_id: uuid(2),
            epoch: None,
            final_seq: None,
        });
        assert!(changed);
        assert!(s.peer(uuid(2)).is_none());
        assert!(s.peer(uuid(1)).is_some());
        // PlayerLeft for an unknown / already-removed peer is a no-op.
        assert!(!s.apply(&SignalFishEvent::PlayerLeft {
            player_id: uuid(2),
            epoch: None,
            final_seq: None,
        }));
        assert!(!s.apply(&SignalFishEvent::PlayerLeft {
            player_id: uuid(99),
            epoch: None,
            final_seq: None,
        }));
    }

    #[test]
    fn player_left_clears_departed_host_and_endpoint() {
        let mut s = MeshSession::new();
        s.apply(&SignalFishEvent::SessionPlan {
            generation: None,
            topology: Topology::Host,
            transport: TransportKind::Direct,
            host: Some(uuid(1)),
            direct_endpoint: Some(DirectEndpoint {
                host: "203.0.113.8".into(),
                port: 7000,
            }),
            peers: vec![],
            ice_servers: vec![],
            fallback: TransportKind::Relay,
        });
        // A departed host must not stay reachable through host() or
        // direct_endpoint(); the replacement SessionPlan owns re-election.
        // The plan carries no peer rows, so `true` here reports exactly the
        // host/endpoint view change.
        assert!(s.apply(&SignalFishEvent::PlayerLeft {
            player_id: uuid(1),
            epoch: None,
            final_seq: None,
        }));
        assert!(s.host().is_none());
        assert!(s.direct_endpoint().is_none());
        assert!(s.peers().is_empty());

        // A non-host departure leaves the elected host untouched.
        s.apply(&plan(
            Topology::Host,
            Some(uuid(3)),
            vec![peer(3, false), peer(4, false)],
            vec![],
        ));
        assert_eq!(s.host(), Some(uuid(3)));
        assert!(s.apply(&SignalFishEvent::PlayerLeft {
            player_id: uuid(4),
            epoch: None,
            final_seq: None,
        }));
        assert_eq!(s.host(), Some(uuid(3)));
    }

    #[test]
    fn topology_transition_mesh_to_host() {
        let mut s = MeshSession::new();
        s.apply(&plan(Topology::Mesh, None, vec![peer(1, true)], vec![]));
        assert_eq!(s.topology(), Some(Topology::Mesh));
        assert!(s.host().is_none());
        // Re-plan as a host topology with an elected host.
        s.apply(&plan(
            Topology::Host,
            Some(uuid(9)),
            vec![peer(9, false)],
            vec![],
        ));
        assert_eq!(s.topology(), Some(Topology::Host));
        assert_eq!(s.host(), Some(uuid(9)));
        assert!(s.is_p2p());
        assert!(s.peer(uuid(1)).is_none());
    }

    #[test]
    fn redundant_updates_return_false() {
        let mut s = MeshSession::new();
        s.apply(&plan(Topology::Mesh, None, vec![peer(1, true)], vec![]));
        // Re-asserting the same liveness / initiate is a no-op (returns false).
        assert!(s.apply(&SignalFishEvent::PeerTransportStatus {
            peer_id: uuid(1),
            transport: TransportKind::WebRtc,
            connected: true,
        }));
        assert!(!s.apply(&SignalFishEvent::PeerTransportStatus {
            peer_id: uuid(1),
            transport: TransportKind::WebRtc,
            connected: true,
        }));
        s.apply(&SignalFishEvent::NewPeer {
            peer_id: uuid(2),
            you_initiate: true,
        });
        assert!(!s.apply(&SignalFishEvent::NewPeer {
            peer_id: uuid(2),
            you_initiate: true,
        }));
    }

    #[test]
    fn replay_is_idempotent() {
        // Applying a full event sequence twice equals applying it once
        // (reconnect missed_events may overlap with live events).
        let sequence = vec![
            room_joined(vec![ice("stun:pre")]),
            plan(
                Topology::Mesh,
                None,
                vec![peer(1, true), peer(2, false)],
                vec![ice("stun:plan")],
            ),
            SignalFishEvent::NewPeer {
                peer_id: uuid(3),
                you_initiate: true,
            },
            SignalFishEvent::PeerTransportStatus {
                peer_id: uuid(1),
                transport: TransportKind::WebRtc,
                connected: true,
            },
        ];

        let mut once = MeshSession::new();
        for e in &sequence {
            once.apply(e);
        }
        let mut twice = MeshSession::new();
        for e in sequence.iter().chain(sequence.iter()) {
            twice.apply(e);
        }

        assert_eq!(once.topology(), twice.topology());
        assert_eq!(once.host(), twice.host());
        assert_eq!(once.ice_servers(), twice.ice_servers());
        assert_eq!(once.peers(), twice.peers());
    }

    fn room_joined(ice_servers: Vec<IceServer>) -> SignalFishEvent {
        SignalFishEvent::RoomJoined {
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
            ice_servers,
            reconnection_token: None,
        }
    }

    fn reconnected(
        ice_servers: Vec<IceServer>,
        missed_events: Vec<SignalFishEvent>,
    ) -> SignalFishEvent {
        SignalFishEvent::Reconnected {
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
            ice_servers,
            missed_events,
            replay: None,
            sender_watermarks: vec![],
            reconnection_token: None,
        }
    }

    #[test]
    fn reconnect_fences_the_old_plan_until_a_fresh_live_plan_arrives() {
        let mut s = MeshSession::new();
        assert!(s.apply(&plan(
            Topology::Mesh,
            None,
            vec![peer(1, true)],
            vec![ice("stun:old")],
        )));
        assert_eq!(s.topology(), Some(Topology::Mesh));

        let changed = s.apply(&reconnected(
            vec![ice("stun:refreshed")],
            // Mesh controls are not canonical replay entries. Even if a caller
            // constructs such an event directly, they cannot revive the old
            // plan across the reconnect boundary.
            vec![plan(
                Topology::Mesh,
                None,
                vec![peer(2, false)],
                vec![ice("stun:replayed")],
            )],
        ));
        assert!(changed);
        assert!(s.topology().is_none());
        assert!(s.generation().is_none());
        assert!(s.peers().is_empty());
        assert_eq!(s.ice_servers(), &[ice("stun:refreshed")]);
    }

    #[test]
    fn reconnect_without_missed_events_is_pre_gather_only() {
        // The common case (server re-sends a live plan; missed_events empty):
        // the reconnect only seeds pre-gather ICE and creates no peers.
        let mut s = MeshSession::new();
        assert!(s.apply(&reconnected(vec![ice("stun:pre")], vec![])));
        assert!(s.topology().is_none(), "no plan means no topology yet");
        assert!(s.peers().is_empty());
        assert_eq!(s.ice_servers(), &[ice("stun:pre")]);
        // Identical reconnect ICE is a no-op.
        assert!(!s.apply(&reconnected(vec![ice("stun:pre")], vec![])));
    }
}
