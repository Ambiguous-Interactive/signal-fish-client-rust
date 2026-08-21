# Session 041 — Room Response Correlation

## Priority and Hosted Audit

The session began from clean `main` at
`bb249861ead85b956677e9219f0b7c3d23d30766`, the merge of PR #127. The GitHub
connector found no open or draft pull requests and no dependency pull requests.
All 12 push workflows on that commit completed successfully; Godot Web run
32531187189 was the last required run to finish and every job succeeded.

Open issue #126 remained the highest-impact actionable milestone. Issue #90 is
still a genuine repository-administration blocker: the live ruleset lacks the
required pull-request and status-check rules, so its scheduled policy audit is
correctly red and cannot be repaired without maintainer administration.

## Authority and Defect

The audit used the pinned Signal Fish Server 0.7.0 source at commit
`3f7f43d4cd4b3cc7f8fb893220dc35c9b1fad333`. Its room handlers emit
`RoomJoined`/`RoomJoinFailed`, `RoomLeft`, `Reconnected`/`ReconnectionFailed`,
`SpectatorJoined`/`SpectatorJoinFailed`, and voluntary `SpectatorLeft` as typed
terminal responses to the corresponding client commands. The same server also
uses `SpectatorLeft` with authoritative removal, disconnect, or room-close
reasons; those exits do not require a voluntary-leave command.

`ClientCore` previously rejected a typed operation response only when a
*different* room operation was pending. With no operation pending, unsolicited
responses or duplicates received after a completed transition could pass
correlation and, where the broader lifecycle phase allowed them, mutate
membership. `ReconnectionFailed` checked only the reconnect-details queue
instead of also requiring the shared room-operation admission.

## Implementation

`validate_pending_room_response` now requires a compatible pending operation
for every player join/leave/reconnect, spectator join, and voluntary spectator
leave response. A different pending operation remains fenced, and the absence
of an admission is a lifecycle violation. Reconnect failure now passes both the
shared operation check and its request-detail check before it can clear either
record. Server-authoritative spectator removal, disconnect, and room-close
exits remain valid and clear stale membership without a voluntary request.

The sweep repaired test harnesses that had modeled terminal responses as
autonomous server notifications. Async, polling, WebRTC, robustness, and parity
fixtures now issue player/spectator join, leave, rejoin, and reconnect commands
before their scripted responses. Dedicated ungated traces remain only where a
test intentionally proves violation handling.

The deterministic performance laboratory had the same synthetic setup gap.
Its 25 non-reconnect workloads now send a real `JoinRoom` before consuming
`RoomJoined`; the measured regions remain unchanged. Their exact protocol
ledgers were regenerated to include that setup command, while the three
reconnect ledgers remained byte-identical.

## Regression Evidence

Shared-core tests cover unsolicited success/failure responses, voluntary exits
without leave commands, and duplicates received after completed joins,
failures, and reconnect failures without membership mutation. Async/polling
matrices exercise the six join/reconnect response shapes outside a transition
and both voluntary exit shapes from confirmed player/spectator membership under
Observe, Quarantine, and Disconnect. A separate parity trace proves Server
0.7's autonomous room-close spectator exit remains accepted. Existing
operation-mismatch tests continue to prove that a different response kind
cannot consume the actual pending fence.

Targeted verification passed for the complete shared core, async client,
polling client, WebRTC controller, integration client, negotiation robustness,
and 39-test cross-driver parity suites. The performance smoke verifier accepted
all 28 exact semantic/wire ledgers. The final parity regression proves a delayed
duplicate exit cannot consume the live rejoin fence in either driver.

## Remaining Issue #126 Work

This is one correctness-first slice, not completion of the repository-wide
audit. Same-kind responses from repeated operations cannot be soundly
correlated because Server 0.7 carries no request identifier; detailed follow-up
[#128](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/128)
tracks the protocol design and compatibility work. The next independent #126
slices also remain oversized complete frames, disconnect-adjacent transition
parity, the consolidated authority matrix, fresh safety/analyzer runs, and
evidence-backed performance profiling.

## Final Verification and Review

The exact mandatory command passed on the frozen implementation tree: formatting
was unchanged, workspace/all-target/all-feature Clippy emitted zero warnings,
and the complete all-feature workspace test suite passed. Three independent
adversarial reviews reached zero actionable findings after repair rounds covering
core semantics, harness realism and regression strength, and goal/evidence
completeness.

Draft pull request
[#129](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/pull/129)
was published from implementation commit `26d989d`. All 11 pull-request
workflows passed. Godot Web's server-0.4 browser cell initially lost its
checksum download to a connection reset before executing project code; a
failed-jobs-only retry downloaded the same pinned release and passed the full
scenario. The PR remained mergeable with no review threads, reviews, or
conversation comments after the implementation run completed.
