//! Optional zero-dependency mesh session tracker (protocol v3).
//!
//! [`MeshSession`] folds the v3 [`SignalFishEvent`]s into an always-consistent
//! view of the current peer-to-peer session: the chosen topology/transport, the
//! peers this client should connect to (each with its server-assigned `initiate`
//! flag and last-known liveness), the elected host, and the ICE servers. It does
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
use crate::protocol::{IceServer, PlayerId, Topology, TransportKind};

/// A peer within a [`MeshSession`], enriched with last-known data-path liveness.
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
    /// Last-known data-path liveness for this peer (from `PeerTransportStatus`).
    pub connected: bool,
}

/// An always-consistent view of the current mesh/host/relay session, folded
/// purely from [`SignalFishEvent`]s. See the [module docs](crate::mesh).
#[derive(Debug, Clone, Default)]
pub struct MeshSession {
    topology: Option<Topology>,
    transport: Option<TransportKind>,
    fallback: Option<TransportKind>,
    host: Option<PlayerId>,
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
                topology,
                transport,
                host,
                peers,
                ice_servers,
                fallback,
            } => {
                self.topology = Some(*topology);
                self.transport = Some(*transport);
                self.fallback = Some(*fallback);
                self.host = *host;
                // A plan fully REPLACES the peer set (handles host re-election
                // and topology change), preserving each surviving peer's
                // liveness; peers absent from the new plan are dropped.
                self.peers = peers
                    .iter()
                    .map(|p| MeshPeer {
                        player_id: p.player_id,
                        player_name: p.player_name.clone(),
                        is_authority: p.is_authority,
                        initiate: p.initiate,
                        connected: self.peer(p.player_id).is_some_and(|e| e.connected),
                    })
                    .collect();
                // SessionPlan ICE supersedes any pre-gathered ICE; an empty list
                // keeps the pre-gathered set.
                if !ice_servers.is_empty() {
                    self.ice_servers = ice_servers.clone();
                }
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
                peer_id, connected, ..
            } => {
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
            SignalFishEvent::PlayerLeft { player_id } => {
                let before = self.peers.len();
                self.peers.retain(|p| p.player_id != *player_id);
                self.peers.len() != before
            }
            // ICE pre-gather: seed the ICE servers during the lobby wait. Do not
            // create peers here — a relay-floor room may never produce a plan.
            SignalFishEvent::RoomJoined { ice_servers, .. } => self.apply_pre_gather(ice_servers),
            // Reconnect: seed pre-gather ICE, then defensively replay any mesh
            // events the server batched into `missed_events`. Today's server
            // rebuilds the session by re-sending a *live* `SessionPlan` after the
            // `Reconnected` (so `missed_events` is empty), but folding here keeps
            // the view correct against servers that carry mesh state in
            // `missed_events`. The fold is idempotent and a later live plan
            // replaces the peer set wholesale.
            SignalFishEvent::Reconnected {
                ice_servers,
                missed_events,
                ..
            } => {
                let mut changed = self.apply_pre_gather(ice_servers);
                for missed in missed_events {
                    match missed {
                        // Terminal / meta events never belong in a reconnect
                        // replay — by definition we are back in the room.
                        SignalFishEvent::RoomLeft
                        | SignalFishEvent::Disconnected { .. }
                        | SignalFishEvent::Reconnected { .. }
                        | SignalFishEvent::RoomJoined { .. } => {}
                        other => changed |= self.apply(other),
                    }
                }
                changed
            }
            // The session is over.
            SignalFishEvent::RoomLeft | SignalFishEvent::Disconnected { .. } => {
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
            topology,
            transport: TransportKind::WebRtc,
            host,
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
        // Surviving peer 2 keeps its liveness across the re-plan...
        assert!(s.peer(uuid(2)).unwrap().connected);
        // ...but its `initiate` is taken from the NEW plan.
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
        // A later NewPeer for the same id overrides the initiate flag.
        s.apply(&SignalFishEvent::NewPeer {
            peer_id: uuid(2),
            you_initiate: false,
        });
        assert_eq!(s.peers().len(), 1);
        assert!(!s.peer(uuid(2)).unwrap().initiate);
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
    fn pre_gather_ice_then_plan_precedence() {
        let mut s = MeshSession::new();
        // RoomJoined seeds pre-gathered ICE.
        s.apply(&room_joined(vec![ice("stun:pre")]));
        assert_eq!(s.ice_servers(), &[ice("stun:pre")]);
        assert!(s.topology().is_none(), "pre-gather creates no plan/peers");
        // A plan with EMPTY ice keeps the pre-gathered set.
        s.apply(&plan(Topology::Mesh, None, vec![peer(1, true)], vec![]));
        assert_eq!(s.ice_servers(), &[ice("stun:pre")]);
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
    fn reset_on_disconnect_and_room_left() {
        for terminal in [
            SignalFishEvent::Disconnected { reason: None },
            SignalFishEvent::RoomLeft,
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
        let changed = s.apply(&SignalFishEvent::PlayerLeft { player_id: uuid(2) });
        assert!(changed);
        assert!(s.peer(uuid(2)).is_none());
        assert!(s.peer(uuid(1)).is_some());
        // PlayerLeft for an unknown / already-removed peer is a no-op.
        assert!(!s.apply(&SignalFishEvent::PlayerLeft { player_id: uuid(2) }));
        assert!(!s.apply(&SignalFishEvent::PlayerLeft {
            player_id: uuid(99)
        }));
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
        }
    }

    #[test]
    fn reconnect_replays_missed_mesh_events_to_rebuild_view() {
        // A server that batches mesh state into `Reconnected.missed_events`
        // (instead of re-sending a live SessionPlan) must still rebuild the view.
        let mut s = MeshSession::new();
        let changed = s.apply(&reconnected(
            vec![ice("stun:pre")],
            vec![
                plan(
                    Topology::Mesh,
                    None,
                    vec![peer(1, true)],
                    vec![ice("stun:plan")],
                ),
                SignalFishEvent::NewPeer {
                    peer_id: uuid(2),
                    you_initiate: false,
                },
            ],
        ));
        assert!(changed, "replaying missed mesh events changes the view");
        assert_eq!(s.topology(), Some(Topology::Mesh));
        assert!(s.peer(uuid(1)).is_some(), "plan peer restored");
        assert!(s.peer(uuid(2)).is_some(), "missed NewPeer restored");
        // The plan's ICE supersedes the pre-gather set.
        assert_eq!(s.ice_servers(), &[ice("stun:plan")]);
    }

    #[test]
    fn reconnect_ignores_terminal_events_in_missed_events() {
        // Build an active mesh, then a reconnect whose missed_events contains a
        // stray terminal event must NOT reset the freshly rebuilt session.
        let mut s = MeshSession::new();
        let changed = s.apply(&reconnected(
            vec![],
            vec![
                plan(Topology::Mesh, None, vec![peer(1, true)], vec![]),
                SignalFishEvent::RoomLeft,
                SignalFishEvent::Disconnected { reason: None },
            ],
        ));
        assert!(changed);
        assert_eq!(
            s.topology(),
            Some(Topology::Mesh),
            "terminal events were ignored"
        );
        assert!(s.peer(uuid(1)).is_some());
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
