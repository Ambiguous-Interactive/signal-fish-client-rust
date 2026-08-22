# Session 045 — Godot Inbound Buffer Boundary

## Priority and Audit

The session began from clean `main` at
`4fc5f5cecbe753ae2259804513a358f14e384f7f`, the merge of PR #133. The GitHub
connector found no open pull requests. Issue #126 remained the active
correctness audit; issue #90 remained the separate maintainer-only repository
governance blocker.

Parallel inventory reviews found three independent correctness risks. This
session fixes the highest-impact normal-gameplay boundary: Godot 4.5 defaults
`WebSocketPeer.inbound_buffer_size` to 65,535 bytes even though a legal Signal
Fish Server 0.7 aggregate snapshot can approach roughly 6.25 MiB. The other
findings are preserved as focused follow-ups: issue #134 covers a ready server
farewell discarded across send failure, and issue #135 covers a delayed stale
session plan rolling generation state backward.

## Authority and Design

Godot 4.5 documentation and source define the 65,535-byte default. Its native
and web peers size both an inbound ring and packet buffer from the configured
property before connection. The pinned Server 0.7.0 source defines a 64 KiB
default accepted client-message size but permits configurable multi-player
state that the server can aggregate into substantially larger outbound
messages. As with the native WebSocket decision in session 043, 8 MiB is a
protective compatibility default rather than a protocol maximum.

An initial design exposed the inbound size through `GodotWebSocketOptions`.
Adversarial review rejected it because the caller-owned `from_peer_with_options`
path could not honestly apply a pre-connect setting. The final design keeps the
constant private: `connect` and `connect_with_options` configure 8 MiB before
`connect_to_url`, while `from_peer` and `from_peer_with_options` preserve every
caller choice. This avoids a public option with constructor-dependent meaning.
Godot may eagerly reserve roughly 16 MiB per SDK-created peer across its two
receive buffers; applications that need another tradeoff configure their own
peer before connecting.

## Regression Evidence

Fake-backend tests prove the 8 MiB setting happens before the connection call
and remains ordered even when connection setup fails. The official Godot smoke
sends an 80 KiB JSON relay from a deliberately enlarged sender to a receiver
created through the standard SDK constructor. It reconstructs and serializes
the received `ServerMessage`, then emits exactly one marker containing a wire
length greater than 65,535 bytes and the exact padding length. A pure validator
rejects missing, duplicate, undersized, malformed, and wrong-padding markers.

The existing browser scenarios now configure their pinned Server 0.4 and 0.7
processes to accept the fixture input. Every clean browser run requires the
large-frame oracle. The build job additionally compiles the same fixture as a
native GDExtension, runs it with the official checksum-verified Godot 4.5 editor
and Server 0.7 binary, checks the same oracle, completes the disconnect path,
and retains both process logs.

The load admission oracle was also corrected to honor the documented
single-frame escape: a per-client buffered-byte peak may exceed the 32 KiB
latency ceiling only when that client records exactly one escape and only up to
its cumulative escape bytes. New frame-count diagnostics make that distinction
explicit; the fixture requires JSON counts `[1, 0]`, binary counts `[0, 0]`,
and byte/count conservation. The former absolute check contradicted the
transport contract, while using unqualified cumulative bytes could mask
multiple escapes.

## Adversarial Review

The initial design review rejected a public inbound option whose meaning would
silently differ on caller-owned peers. It selected the private SDK default,
unchanged `from_peer` contract, official native/web proof, and explicit memory
cost implemented here. The frozen-diff review then found that the first native
harness would terminate Server as soon as the binary pair was ready, before the
16-second JSON load and shutdown completed. An independent repair pass added
bounded waits for the load summary and JSON shutdown before SIGTERM, followed
by close-attribution, completion, and process-exit oracles.

That review also rejected cumulative escape bytes as an individual-frame bound
and found the caller-owned preservation test nondiscriminating. The repair adds
the escape-frame counter and exact runtime pattern above, negative controls for
multiple escapes, a fake backend that proves wrapping performs no configuration
or connection call, and a distinctive `2 MiB + 17` runtime sender setting.
The final review then caught misplaced changelog entries, non-exact marker
counting, a copied nonportable readiness probe, and stale skill bookkeeping.
Released history is restored; the validator counts every marker-token line
before strictly parsing its complete payload and rejects malformed duplicates
or trailing data; the new probe uses `printf`; and guidance records 38 tests.
The final re-review reported zero actionable issues.

## Verification and Hosted Disposition

All 12 push workflows on the starting main commit passed. The 38-test Godot
adapter suite and the five pure JavaScript oracle tests pass locally. Workflow
hygiene, shell portability, the 228-test CI policy suite, and the exact
format/Clippy/workspace-test mandatory chain pass. The standalone
fixture cannot compile in the development container because its `api-custom`
godot-rust build requires a Godot 4 executable; the pinned hosted job owns that
official engine proof. PR #136's first hosted pass exposed one deterministic
Actionlint failure: the native oracle's single-quoted inline JavaScript used
template-literal `${...}` expressions, which Actionlint's embedded ShellCheck
reported as SC2016. Rewriting those three diagnostics as string concatenation
preserves their behavior without shell-looking interpolation. Final mandatory,
pull-request, and hosted workflow evidence will be appended after the repaired
implementation diff is frozen.
