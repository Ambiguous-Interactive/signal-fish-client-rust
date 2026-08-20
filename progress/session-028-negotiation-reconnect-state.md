# Session 028 — Negotiation and Reconnect State

## Scope

Issues #100 and #101 harden the shared async/polling state transaction around
the first authoritative `ProtocolInfo` and a successful reconnect. The work is
bound to Signal Fish Server 0.7.0 commit
`3f7f43d4cd4b3cc7f8fb893220dc35c9b1fad333`.

## Rule → Source → Evidence

| Rule | Pinned Server 0.7 source | Executable evidence |
| --- | --- | --- |
| Preserve the exact requested game-data format, including omission, while leaving the effective format unresolved until the first valid `ProtocolInfo`. | `src/websocket/connection.rs:2016-2062`; `src/config/protocol.rs:73-82`. | `polling_parity_tests::requested_and_effective_game_data_formats_have_complete_parity`; `polling_parity_tests::malformed_duplicate_and_around_negotiation_frames_have_complete_parity`. |
| Accept only Server 0.7's canonical `[Json]` or `[Json, MessagePack]` advertisement. Resolve an advertised request to itself and omission/unsupported requests to JSON. The earlier `UnsupportedGameDataFormat` frame is advisory and arrives before `Authenticated`/`ProtocolInfo`. | `src/websocket/connection.rs:2016-2062`; `src/config/protocol.rs:73-82`. | `polling_parity_tests::json_fallback_is_enforced_before_outbound_transport_admission_in_both_drivers`; `polling_parity_tests::fallback_json_is_delivered_and_binary_is_rejected_with_driver_parity`; `real_server_e2e::e2e_server_070_rkyv_request_resolves_to_json`. |
| Use the effective format for physical binary representation and outbound binary admission; JSON-origin relays remain text for every recipient format. Clear only effective state on disconnect. | `src/websocket/sending.rs:259-345` plus AsyncAPI game-data frames. | `polling_parity_tests::message_pack_receiver_accepts_json_origin_text_relay_with_driver_parity`; shared-core negotiation tests; supported-MessagePack binary parity coverage. |
| Reconnect replay admits only the canonical seven membership/lobby/authority controls; `ProtocolInfo`, `SessionPlan`, signaling, and game data are not replay entries. | `src/server/reconnection_service.rs:954-1111`; AsyncAPI v3 `Reconnected`. | `client_core::tests::reconnect_replay_rejects_non_replayable_session_messages_atomically`; negotiation robustness and polling parity nested-`ProtocolInfo` tests. |
| A reconnect terminal response must match an admitted player/room/token request. V3 also requires replay status, exact current-player stamps, complete matching sender watermarks, and a nonempty rotated token; v2 exposes none of those fields. Invalid authoritative baselines never apply under any policy. | `src/server/reconnection_service.rs:954-1111`; server spec tests `V3Reconnected` requirements. | `client_core::tests::reconnect_responses_require_the_matching_admitted_request`; `client_core::tests::reconnected_payload_version_matrix_is_transactional`; `client_core::tests::invalid_reconnect_accountability_never_resets_the_existing_frontier`; reconnect-aware polling parity. |
| `Reconnected` is an immediate plan fence. A finalized room receives a separate fresh live `SessionPlan` with a new generation and refreshed ICE/TURN state. | `src/server/reconnection_service.rs:1183-1381`. | `mesh::tests::reconnect_fences_the_old_plan_until_a_fresh_live_plan_arrives`; `webrtc::tests::reconnect_fences_stale_driver_output_until_the_replacement_plan`; `client_tests::reconnect_receives_fresh_authoritative_session_plan`; strengthened real-server reconnect E2E. |

## Public API and Compatibility

- `ClientSnapshot` now exposes `requested_game_data_format` and
  `effective_game_data_format`; async, polling, and object-safe shared APIs have
  matching accessors.
- These additions are breaking for exhaustive `ClientSnapshot` literals, so
  they are recorded for 0.11. `SignalFishClientApi` supplies default accessors.
- Server 0.4 v2 reconnect shapes remain accepted: no v3 stamps, replay status,
  watermarks, or reconnect-token field is required or permitted.

## Review and Verification

- Three independent pre-implementation audits compared open issues, local
  code, pinned Server 0.7 behavior, public API, test parity, and WebRTC state.
- Targeted shared-core, async/polling, mesh-controller, and ignored real-server
  tests passed locally. The real-server runs used an ARM64 binary built directly
  from the pinned commit; both unsupported-Rkyv fallback and finalized reconnect
  with replay/watermark/token/fresh-plan assertions passed.
- Three adversarial reviews found and drove fixes for JSON-origin relay
  representation, reconnect-response causality, stale WebRTC output, malformed
  negotiation tuples, fixture validity, and secret-safe assertions; the final
  local review reported no remaining findings. Hosted review then identified a
  coalesced plan-barrier race; the controller now advances one revision per
  consumed room/plan/spectator barrier instead of copying the core's latest
  revision, with an exact queued-replan regression.
- `cargo fmt`, all-target/all-feature clippy with warnings denied, the complete
  all-feature workspace suite, feature-isolation checks, rustdoc, and local docs
  validation pass. Hosted CI evidence is recorded on the pull request.
