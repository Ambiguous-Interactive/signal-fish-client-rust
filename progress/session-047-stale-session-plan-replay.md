# Session 047 — Stale Session-Plan Replay

## Priority and Audit

The session began from clean `main` at
`b99303849d709355d8fd2f07eca1604b00c95ac8`, the merge of PR #137. The GitHub
connector found no open pull requests or dependency updates. The actionable
gameplay-correctness issue was #135 under the #126 audit; #90 remains a
separate maintainer-administration blocker for live repository governance.

Issue #135 showed that the client accepted any structurally valid
generation-bearing `SessionPlan`. A delayed plan A arriving after accepted plan
B therefore restored A's generation, topology, transport, and peer signaling
authority. A subsequent signal stamped with A became current again, while B's
real current traffic became stale.

## Design

`ClientCore` now retains every superseded non-null session generation for the
current room/session. Before accountability or authoritative plan-state,
peer-set, or async mesh-revision mutation, session-plan validation rejects a
non-current generation already in that retired set as one lifecycle violation.
A `HashSet` covers non-adjacent replay such as A → B → C → A rather than
remembering only the immediately previous plan.

An accepted replacement retires its prior non-null generation. Reasserting the
current generation remains valid, and generation-less Server 0.4 plans are not
entered into the UUID replay ledger. Authoritative room baselines, confirmed
room exits, reconnect baselines, and physical disconnect clear the ledger so a
UUID from another room/session cannot be rejected by stale local history.

## Regression Evidence

Shared-core tests cover A → B → replayed A under Observe, Quarantine, and
Disconnect. Plan A is a Mesh/WebRTC plan authorizing one peer; B is a
Host/WebRTC plan authorizing another. Replayed A emits one lifecycle violation
without changing B's generation, topology, transport, peer authority, or async
plan revision; Quarantine changes only its documented quarantine flag. A's
following signal remains suppressed and B's current signal remains accepted
for non-terminal policies; Disconnect stops at the replay violation. A separate
selected-path case proves that a replayed WebRTC plan cannot replace a newer
Direct plan.

The async/polling parity regression drives the same A → B → A → signal A →
signal B trace through both public clients for all three policies. Their exact
event streams, terminal behavior, snapshots, and statistics match. Additional
tests pin A → B → C → A history, duplicate current generations, repeated
generation-less plans, room teardown, and physical disconnect.

## Adversarial Review

Parallel code and protocol-authority audits required the ledger to live in the
shared core, validate before all mutation, accumulate more than one retired
generation, preserve Server 0.4 omission semantics, and clear at every room or
connection boundary. Review also rejected an early regression arrangement in
which old plan A selected Direct transport: it could not prove the reported
stale-WebRTC reauthorization because signal A would already be invalid. The
final principal trace makes A WebRTC and verifies both stale-A suppression and
positive current-B delivery; a separate case isolates transport preservation.

## Verification and Hosted Disposition

The frozen implementation passes the mandatory local chain:

- `cargo fmt`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features` (394 core unit tests, 49 polling
  parity tests, all workspace integration and doc tests; 11 live-server tests
  remain ignored by their explicit environment contract)

PR #138 is the session's single pull request. Its implementation head,
`54b0f6a033a9305fec245df6f0274516499c8617`, passed all 11 hosted workflows:
Security, No Panics, Workflow Lint, Unused Deps, WASM, Semver Checks, Examples
Validation, Coverage, Docs Validation, CI, and Godot Web. The Godot Web matrix
also passed its official-template build/export and clean Server 0.7, clean
Server 0.4, soak, and impaired browser scenarios.

The hosted review audit found no inline review threads or actionable review
feedback. Copilot reported only that its review quota was exhausted; that is a
known repository-administration limitation tracked separately by #90, not a
finding against this change.
