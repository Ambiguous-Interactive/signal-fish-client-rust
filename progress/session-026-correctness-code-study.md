# Session 026 — Correctness Code Study and Hardening

Date: 2026-08-16

## Objective

Execute issue #97 as a data-backed study of protocol/state correctness, public
API safety, transport/network resilience, pinned-server parity, and unexpected
input behavior; fix the highest-impact proven defects and decompose every
unresolved finding into a bounded issue.

## Audit Method

Three independent read-only audits covered:

- protocol negotiation, reconnect, delivery accountability, session plans,
  signaling generations, and Server 0.7 AsyncAPI/source parity;
- public types, snapshots, operation guards, statistics, mesh capability, and
  credential-bearing diagnostic paths;
- async/polling transport ownership, FIFO/backpressure, shutdown, WebSocket
  control frames, close metadata, malformed frames, and missing UDP scope.

Each audit recorded an invariant, authoritative evidence, current executable
coverage, severity, and bounded recommendation. Implemented changes then went
through repeated adversarial review and correction.

## Implemented Hardening

### Reliable operation admission

- Reliable async operations validate before waiting and again while holding a
  reserved command permit, without holding the shared mutex across an await.
- A signal produced for the current plan binds to its original wire generation
  before waiting, preventing stale SDP/ICE from being relabeled after a re-plan.
- A private plan revision fences Server 0.4-compatible generation-less plans;
  generation-bearing plans continue to use their authoritative UUID so an
  idempotent same-generation plan reassertion remains valid.
- A private membership revision prevents JSON or binary game data begun in one
  room from crossing a leave/reconnect/join boundary into another room.
- Saturated-queue regressions prove changed-UUID rejection, `None -> None`
  replacement rejection, same-UUID acceptance, JSON/binary room fencing,
  fail-fast pre-negotiation, and absence of stale frames on the wire.

### Async transport liveness and termination

- The async loop retains one in-flight frame and polls send and receive
  together. A legal indefinitely pending send can no longer hide later inbound
  readiness, peer EOF, receive errors, or shutdown.
- Backend-owned sends finish before graceful close under one deadline; a
  caller-owned frame may be abandoned when termination wins.
- Core state is terminal before the Disconnected event. Event delivery and
  close progress run concurrently, so a full event channel cannot delay close
  or abort forever.
- The task watchdog includes scheduling grace beyond the transport deadline,
  letting the loop invoke `Transport::abort` before task abortion.
- Retained-frame, stored-waker, full-event-channel, hanging-close, and abort
  regressions pin ownership, wake-driven progress, terminal ordering, and
  bounded resource release. Transport error reasons are no longer double
  prefixed.

### Ambient diagnostic redaction

- Replaced credential/payload-bearing derived Debug implementations across
  protocol messages, reconnect payloads, connection info, ICE servers,
  signaling, binary envelopes, raw transport frames, close metadata,
  WebSocket state, mesh sessions, and driver/mesh events.
- Native WebSocket tracing no longer records full URLs or peer-controlled close
  reasons; Godot transport Debug inherits the redacted close metadata.
- Sentinel tests cover direct and transitive wrappers and reject both string
  secrets and decimal `Vec<u8>` Debug output. Serde/wire shapes, public fields,
  variants, and Debug trait availability are unchanged.

## Unresolved Finding Decomposition

| Issue | Invariant / scope |
| --- | --- |
| #99 | Lifecycle/version/message-scope validation, semantic SessionPlan shapes, and signal peer membership |
| #100 | Requested versus negotiated effective game-data encoding |
| #101 | Atomic reconnect negotiation, accountability, snapshot, and plan restoration |
| #102 | Mesh capability, controller startup, selected plan, and per-transport liveness semantics |
| #103 | Coherent membership identity and locally enforced operation/role guards |
| #104 | Connection readiness phases and exact ClientStats counting points |
| #105 | Bounded WebSocket control-frame work, terminal EOF, and missing state-machine evidence |
| #106 | Enforceable custom Transport abort/resource-release contract |
| #107 | Datagram/UDP framing, trust, ordering, and resilience ownership |

The study also confirmed strong existing coverage for strict v2/v3 binary
decoding, exact delivery-gap accountability, bounded polling work/FIFO queues,
malformed JSON/binary diagnostics, WebSocket close metadata and idempotency,
TCP_NODELAY, TLS behavior, and async/polling shared-core parity.

## Verification

- Focused reliable-generation/membership, redaction, mesh, WebSocket, Godot,
  and async transport liveness regressions pass.
- The panic-free source policy passes.
- Polling-only Clippy passes with `-D warnings`, proving async-only binding
  state does not leak dead code into runtime-less feature builds.
- Workspace/all-target/all-feature Clippy passes with `-D warnings`.
- The mandatory workflow passes: formatting, workspace/all-target/all-feature
  Clippy with `-D warnings`, and workspace/all-feature tests. The latter runs
  294 core unit tests, 35 Godot adapter tests, all integration/policy suites,
  and doc tests; five live-server E2E cases remain intentionally ignored in
  local runs and are delegated to hosted blocking workflows.

## Hosted State

- Root-workspace Dependabot run 31969079956 succeeded on main and satisfied
  issue #95; no Cargo dependency PR was opened.
- Main project workflows were green in the session baseline audit. Repository
  policy enforcement remains the maintainer-administration blocker tracked by
  issue #90 and is not fixable from repository code alone.
