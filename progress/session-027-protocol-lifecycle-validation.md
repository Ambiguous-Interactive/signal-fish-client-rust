# Session 027 — Protocol Lifecycle Validation

## Scope

Issue #99 makes decoded server input transactional across the shared async and
polling core. It validates connection/negotiation/room phases, authoritative
session-plan shapes, and signal-peer membership before accountability or
observable state can advance.

The implementation was derived from the pinned Signal Fish Server 0.7 source
at commit `3f7f43d4cd4b3cc7f8fb893220dc35c9b1fad333`, with the pinned Server 0.4
source at `50b28a9a13dc2b99d301bfb2482c5fd6f768a2e8` retained only for omitted
signal-generation compatibility.

## Rule → Source → Evidence

| Rule | Pinned source | Executable evidence |
| --- | --- | --- |
| Authentication, `ProtocolInfo`, room admission/exit, and room-scoped messages are accepted only in their legal phase; v3-only messages require negotiated v3. `Error` remains legal before authentication, and `RelayStats`/`GoingAway` remain connection-scoped. | Server 0.7 `src/websocket/connection.rs`; `src/server/message_router.rs:251-400`; `src/server/shutdown.rs:112-155`; AsyncAPI `PeerTransportStatus`/`RelayStats`/`GoingAway` at lines 2001-2079. | `client_core::tests::lifecycle_classifier_rejects_pre_auth_pre_room_post_room_and_v2_v3_mismatches`; `client_core::tests::player_room_baseline_requires_the_local_player_exactly_once`; `polling_parity_tests::lifecycle_plan_and_signal_matrix_has_complete_driver_parity`. |
| Invalid lifecycle/plan/signaling messages emit exactly one lifecycle `ProtocolViolation`, suppress the original event, and do not apply its state. Policy only selects quarantine, disconnect, or continuation. | Client policy contract plus Server 0.7 state-machine ordering. | `client_core::tests::invalid_lifecycle_policy_controls_quarantine_and_disconnect_but_never_applies`; `polling_parity_tests::lifecycle_plan_and_signal_matrix_has_complete_driver_parity`. |
| Legal plans are exactly relay/relay, host/direct, host/webrtc, and mesh/webrtc; fallback is relay and required/forbidden host, endpoint, peer, and ICE fields are enforced. | Server 0.7 `src/server/session_policy.rs:495-671`; AsyncAPI `SessionPlanPayload` at lines 1552-1625. | `client_core::tests::session_plan_topology_transport_cross_product_accepts_only_four_pairs`; `client_core::tests::session_plan_cross_fields_and_peer_identity_are_transactional`; `polling_parity_tests::lifecycle_plan_and_signal_matrix_has_complete_driver_parity`. |
| Direct endpoints use the server's conservative host syntax and a non-zero port. Plans cannot name self, duplicate a peer, or name a host/peer outside the current room roster. | Server 0.7 `src/protocol/validation.rs:6-59`; `src/server/session_policy.rs:520-630`. | `client_core::tests::session_plan_cross_fields_and_peer_identity_are_transactional`. |
| Omitted generation remains accepted only as the structural Server 0.4 compatibility case; omission never excuses an otherwise invalid plan. | Server 0.4 commit `50b28a9a13dc2b99d301bfb2482c5fd6f768a2e8`; Server 0.7 AsyncAPI generation fields at lines 477-483, 893-908, and 1520-1533. | `client_core::tests::generationless_server_04_plan_remains_valid_when_its_shape_is_canonical`; `client_core::tests::session_plan_cross_fields_and_peer_identity_are_transactional`. |
| Current-generation inbound signals and every outbound signal target only a non-self peer in both the authoritative WebRTC peer set and current room roster. Replans and departures revoke authority; stale-generation inbound signals remain harmless reordering and are suppressed. | Server 0.7 `src/server/signaling.rs:126-323`; AsyncAPI `Signal` generation at lines 893-908. | `client_core::tests::signal_peer_membership_is_shared_by_inbound_and_outbound_paths`; `client_core::tests::replacement_plan_and_player_left_remove_signal_authority`; `client_core::tests::stale_signal_after_relay_replan_is_silently_suppressed`; `client::tests::reliable_signal_revalidates_peer_departure_while_waiting_for_capacity`; `polling_parity_tests::unauthorized_outbound_signals_fail_without_wire_output_in_both_drivers`. |
| `NewPeer` is additive compatibility only after an authoritative WebRTC plan and only for a current room player. `PeerTransportStatus` remains room-scoped rather than plan-peer-scoped. | Server 0.7 AsyncAPI `NewPeer` at lines 1534-1551 and `PeerTransportStatus` at lines 2001-2013; `src/server/message_router.rs:251-400`. | `client_core::tests::replacement_plan_and_player_left_remove_signal_authority`; `client_core::tests::peer_transport_status_requires_another_room_player_not_a_plan_peer`; `polling_client::tests::poll_emits_new_peer_and_peer_transport_status_events`. |

## Design Notes

- `ClientCore` owns membership role, finalization, room roster, plan transport,
  and the replace-on-plan peer set so both public drivers make identical
  decisions.
- Validation is read-only and precedes accountability/state mutation for text
  and decoded binary frames. Lifecycle-invalid input is never delivered under
  `Observe`; that policy continues with later valid frames.
- `SignalFishError::SignalPeerNotInSession` makes local outbound refusal precise
  and guarantees no wire frame is queued for self, unknown, departed, or
  re-planned targets.
- Server-assigned `SessionPeer::initiate` is never recomputed.
- Reconnect replay restoration remains tracked by issue #101; Server 0.7 sends
  a fresh full plan instead of placing `ProtocolInfo`/`SessionPlan` in
  `missed_events`.

## Review and Verification

- Initial contract, core, and test audits were performed independently before
  implementation; their findings corrected pre-auth `Error`, connection-scoped
  relay/shutdown messages, admission-failure phases, status scope, binary
  ordering, and endpoint validation.
- Legacy async, polling, WebRTC, and scheduler fixtures were rebuilt as valid
  authenticated/negotiated/room traces instead of weakening the classifier.
- Adversarial review loops are recorded in the PR discussion; every actionable
  finding was fixed and rechecked before handoff.
- Required local gate:

  ```text
  cargo fmt
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo test --workspace --all-features
  ```
