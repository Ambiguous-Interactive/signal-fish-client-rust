# Session 031 — Client Membership and Operation State

## Scope and Priority

Issue #103 is the highest-impact open gameplay correctness issue. Server 0.7
silently ignores game data from a connection without player membership, while
the client previously admitted player commands before join, after leave, and as
a spectator. The next dependency order is #104 (readiness/stat counters), #106
(custom transport abort contract), then #107 (datagram scope). None belongs in
this PR.

The hosted audit found no open or draft PR and no Dependabot PR to incorporate.
At base `ad3e728`, ten required `main` workflows were green and Godot Web was
still running. Issue #90 remains an administrative governance blocker: live
ruleset #14801090 still lacks pull-request and required-status-check rules, and
Repository Policy run 32006639556 correctly fails on that drift.

## Membership and Command Contract

`ClientSnapshot::room_role` is the authoritative `Player`/`Spectator`
dimension. `room_role`, `player_id` (the local room participant ID), `room_id`,
and `room_code` are all absent outside a room and all present in confirmed room
membership. Confirmed player and spectator exits clear the complete identity.

| Operation category | Required state |
| --- | --- |
| `JoinRoom`, `JoinAsSpectator`, `Reconnect` | no room membership |
| `LeaveRoom`, JSON/binary game data, readiness/start, authority, connection info, typed/raw signaling, transport status | player |
| `LeaveSpectator` | spectator |
| `Ping` | any nonterminal connection |

`request_authority(false)` requires current authority. `start_game` requires
current authority when the room has one, and remains available to any player
when no authority has been assigned, matching pinned Server 0.7 behavior.

Validation order is deterministic: `NotConnected`, a pending admitted room
transition, membership/role, authority, protocol version, format/session plan,
then bounded queue admission. Queue refusal never creates pending state. A
successfully admitted join, leave, or reconnect does create it, fencing later
room commands until a matching typed terminal response; `Ping` remains usable.
Generic errors and absent responses stay fail-closed until connection teardown,
after which a new connection may retry.

## Invariant → Source → Evidence

| Invariant | Source | Executable evidence |
| --- | --- | --- |
| Shared role/authority/no-membership matrix and stable error precedence. | `ClientCore::{validate, set_room, clear_room}`. | `client_core::tests::operation_membership_matrix_is_exhaustive_and_role_specific`; async client guard tests; polling parity command fixtures. |
| Pending transitions mutate only after successful queue admission, accept only matching response types, and roll back only on attributable typed failures. Unattributed errors remain fenced. | `ClientOperationAdmission`; both drivers' queue admission paths; `validate_pending_room_response`. | `client_core::tests::{admitted_room_transitions_fence_fifo_commands_and_failures_roll_back,pending_room_responses_are_correlated_and_unattributed_errors_stay_fenced}`; `admitted_leave_fences_following_player_commands_in_both_drivers`; full-queue recovery tests. |
| Player/spectator identity is coherent and clears on every exit/disconnect. | `RoomBaseline`; `ClientSnapshot::room_role`; `clear_room`. | Async and polling room/spectator lifecycle state tests. |
| Authority used by local guards is roster-consistent and transactional. | `validate_local_player_snapshot`; `validate_authority_snapshot`; `AuthorityChanged` semantic validation. | Baseline-validation policy tests and authority command tests. |
| Confirmed exit tears down WebRTC peers without sending obsolete room-scoped status. | `MeshController::handle_event`. | `webrtc::tests::room_left_tears_down_open_channel_without_post_exit_status`. |

## Server Contract Evidence

Pinned Signal Fish Server 0.7 commit `3f7f43d` returns `NOT_IN_ROOM` for player
readiness/start and connection-info operations, requires current authority to
relinquish it, returns `NOT_A_SPECTATOR` for spectator leave, and rejects joins
from either existing player or spectator membership as `ALREADY_IN_ROOM`.
Its signaling-path game-data handler silently returns when the sender has no
player room. The local matrix therefore prevents both noisy server errors and
the more dangerous silent gameplay-loss path without changing wire shapes.

Server 0.4 compatibility remains adaptive: join and role semantics are shared,
while v3-only protocol checks still occur after valid player membership.

## Review and Verification

- The shared-core and driver-parity implementation was adversarially audited
  against every operation variant, pending FIFO transitions, queue-full
  precedence, authority semantics, identity documentation, and mesh teardown.
  The exhaustive async/polling matrix runs every command outside a room, as a
  player and spectator, and after confirmed leave and rejoin for both roles.
- The mandatory `cargo fmt && cargo clippy --workspace --all-targets
  --all-features -- -D warnings && cargo test --workspace --all-features` gate
  passes: 336 library tests, 33 driver-parity tests, 214 policy tests, 68 async
  integration tests, 126 protocol tests, and 35 Godot adapter tests are green;
  six live-server tests remain intentionally isolated to pinned hosted jobs.
- The no-default workspace check/test, all-feature workspace rustdoc,
  `.llm/` validation, workflow policy, and `git diff --check` pass locally.
  `mkdocs` is not installed in the local image and Python lacks `pip`/`venv`,
  so strict rendered-doc validation remains delegated to the blocking hosted
  Docs Validation workflow.
- [PR #114](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/pull/114)
  is open and non-draft. Its code-bearing head `bd272d9` completed all twelve
  workflow suites successfully: CI 32411518446, Coverage 32411518471, Deep
  Safety 32411518476, Docs Validation 32411518382, Examples Validation
  32411518478, Godot Web 32411518415, No Panics 32411518376, Security
  32411518466, Semver Checks 32411518414, Unused Deps 32411518506, WASM
  32411518567, and Workflow Lint 32411518495.
- The first hosted pass found a spelling violation, the intended breaking API
  classification, and a pre-join live-server expectation that predated the new
  error precedence. Commits `349d85a` and `bd272d9` fixed the tests, and the PR
  title now carries the required `fix!:` marker. The final review surface had no
  actionable conversation comments or inline threads; Copilot posted only its
  quota-limit notice.
