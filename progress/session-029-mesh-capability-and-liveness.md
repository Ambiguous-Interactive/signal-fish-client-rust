# Session 029 — Mesh Capability and Liveness

## Scope

Issue #102 separates three facts that were previously conflated: locally
advertised and negotiated WebRTC/P2P capability, the topology and transport in
the server's authoritative plan, and peer liveness on that selected transport.

## Invariant → Source → Evidence

| Invariant | Source | Executable evidence |
| --- | --- | --- |
| A controller that owns a WebRTC driver advertises v3, WebRTC, and at least one Host/Mesh topology even when the input config already selected v3. Compatible explicit lists and future versions remain intact. | `SignalFishConfig::enable_controller_mesh`; `MeshController::start`. | `client::tests::controller_mesh_configuration_preserves_compatible_choices`; `webrtc::tests::controller_authentication_adds_missing_mesh_capability_without_overwrite`. |
| `supports_mesh()` reports negotiated local WebRTC + Host/Mesh capability, never the server-selected path. | `SignalFishConfig::advertises_mesh_capability`; `ClientCore::supports_mesh`. | `client::tests::mesh_capability_requires_webrtc_and_a_p2p_topology`; `polling_parity_tests::parity_mesh_capability_requires_webrtc_and_p2p_topology`. |
| The latest authoritative topology and transport form one coherent snapshot, update atomically with a valid plan, and clear at room/connection boundaries. | `ClientSnapshot::{session_topology, session_transport}`; `ClientCore::replace_session_plan`, `clear_room`, and `clear_session`. | `polling_parity_tests::parity_selected_plan_accessors_follow_ordered_canonical_transitions`; `parity_selected_plan_resets_at_room_and_connection_boundaries`; `parity_reconnected_baseline_is_planless_until_fresh_session_plan`. |
| Peer `connected` describes only the current selected transport. A transport, generation, or offerer-role change clears stale liveness. | `MeshSession::apply`. | `mesh::tests::liveness_tracks_only_the_selected_transport_across_plan_transitions`; `generation_change_clears_surviving_peer_liveness`; `replan_replaces_peers_and_ice_not_merges`; `new_peer_for_known_peer_updates_latest_wins`. |
| Async, polling, object-safe API, and controller views agree on selected plan state. Direct and Relay plans never invoke WebRTC work. | Shared `ClientCore`; `MeshController::session`. | `polling_parity_tests::parity_selected_plan_accessors_follow_ordered_canonical_transitions`; `parity_selected_plan_resets_at_room_and_connection_boundaries`; `webrtc::tests::direct_and_relay_plans_never_connect_through_webrtc_driver`. |

## Public API and Migration

- `ClientSnapshot` adds exhaustive `session_topology` and `session_transport`
  fields. Both clients and `SignalFishClientApi` add matching accessors and
  `is_p2p_active()`.
- `supports_mesh()` remains source-compatible but is capability-only. It now
  returns `false` for custom WebRTC + relay-only advertisements. Routing code
  migrates to one `snapshot()` read or `is_p2p_active()`.
- `MeshController::start` preserves compatible custom choices instead of
  replacing them with the convenience defaults.

## Review and Verification

- Parallel local, hosted-state, and adversarial audits found the configuration,
  API, state-duplication, stale-liveness, documentation, and test risks.
- The first adversarial pass drove a single authoritative snapshot transport,
  offerer-role liveness reset, exact controller wire tests, accessor/controller
  parity, and consumer migration documentation.
- Two final adversarial passes reported zero remaining implementation, API,
  test, changelog, roadmap, or consumer-documentation findings.
- The mandatory `cargo fmt && cargo clippy --workspace --all-targets
  --all-features -- -D warnings && cargo test --workspace --all-features`
  workflow passes: 323 library tests, 31 async/polling parity tests, 126 protocol
  tests, 35 Godot adapter tests, and every other workspace target are green;
  six live-server tests remain intentionally ignored outside their pinned jobs.
- Default and all-feature rustdoc pass with warnings denied. No-default and
  feature-isolated builds/tests, workflow policy validation, LLM drift checks,
  and `git diff --check` also pass. MkDocs is not installed locally, so the
  hosted Docs Validation aggregate is the strict documentation-build evidence.
