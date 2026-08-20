# Session 032 — Readiness and Traffic Counter Contracts

## Scope and Priority

Issue #104 was the highest gameplay-impacting open correctness issue after PR
#114 merged and closed #103. The hosted audit found no open or draft PR and no
dependency PR to incorporate. This session intentionally excludes #106 and
#107 so readiness and traffic observability form one reviewable contract.

## Connection Phase Contract

`ClientSnapshot` exposes four nested dimensions without a redundant phase enum:

| Phase | `connected` | `transport_ready` | `authenticated` | `room_role` |
| --- | ---: | ---: | ---: | --- |
| Connecting/client-owned | true | false | false | none |
| Transport ready | true | true | false | none |
| Authenticated | true | true | true | none |
| In room | true | true | true | player or spectator |
| Terminal | false | false | false | none |

`connected` preserves command admission during asynchronous handshakes.
`transport_ready` is a sticky driver observation of `Transport::is_ready()`
and drives the one synthetic `Connected` event. Both reset at logical
termination. A connecting transport retains caller ownership and wakes
registered async I/O when readiness changes.

## Traffic Counter Contract

- `game_data_sent` increments exactly at `Transport::poll_send` ownership
  transfer (`Some` to `None`), including accepted-Pending and
  accepted-then-error outcomes. It is not server or peer delivery.
- `game_data_received` increments after successful logical game-data decode,
  before message validation, sequence accountability, stale, quarantine, or
  event suppression. Physical binary lifecycle/representation checks that
  reject a frame before logical decoding remain outside this counter.
- `messages_undecodable` remains disjoint from decoded game data. Non-game
  protocol frames, WebRTC application traffic, and physical binary frames
  rejected before logical decode are outside the game-data counters.
- All counters saturate and survive room and connection teardown. A new client
  alone starts from zero.

Cross-peer equality is deliberately not promised: broadcast fanout can make
aggregate receipt larger, while server delivery policy, rejection, terminal
unread work, or accepted-then-error can make it smaller.

## Invariant to Evidence Matrix

| Invariant | Source | Executable evidence |
| --- | --- | --- |
| Both drivers defer `Connected` until readiness and preserve terminal ordering. | `ClientCore::mark_transport_ready`; async `transport_loop`; polling `poll_at`. | Cross-driver phase/terminal parity; async delayed/terminal readiness tests; polling delayed, ready-during-recv, ready-immediate-close, and terminal-before-ready tests. |
| Outbound ownership is counted once, independent of completion. | Async `poll_pending_send`; polling `drive_outbound`. | Async accepted-Pending success/error and take/no-take error tests; polling accepted-Pending and take/no-take error tests. |
| Decoded receipt includes suppressed data. | `ClientCore::{process_text,process_binary}`. | Exact async/polling stale/quarantine/lifetime parity, shared-core suppression, and physical binary receipt tests. |
| State and statistics remain driver-aligned. | `ClientCore` snapshots/counters and default `SignalFishClientApi` readiness accessor. | Full async/polling unit and parity suites plus the mandatory workspace gate. |

## Review and Verification

Three independent audits reviewed readiness semantics, traffic counting points,
public API/semver impact, documentation obligations, and hidden terminal cases.
Their findings drove guarded readiness transitions, wake requirements,
accepted-then-error coverage, stale/quarantine receipt coverage, conservation
documentation, and the decision not to add a redundant exhaustive phase enum.

Local verification completed with the mandatory workspace command:

```text
cargo fmt
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Clippy completed with zero warnings; all 345 root library tests, 35 Godot
adapter tests, 470 integration tests, and seven runnable rustdoc tests passed
(six live-server tests and five example-only rustdoc blocks remained explicitly
ignored). Repository LLM, workflow-policy, and documentation validators also
passed. Hosted check and review evidence will be recorded on the PR before
merge.
