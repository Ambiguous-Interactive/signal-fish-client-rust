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
format/Clippy/workspace-test mandatory chain pass. The container did not ship a
Godot executable, so the standalone fixture initially remained hosted-only; a
later checksum-verified official Godot 4.5 arm64 download enabled the local
native compile and engine startup checks described below. PR #136's first
hosted pass exposed one deterministic
Actionlint failure: the native oracle's single-quoted inline JavaScript used
template-literal `${...}` expressions, which Actionlint's embedded ShellCheck
reported as SC2016. Rewriting those three diagnostics as string concatenation
preserves their behavior without shell-looking interpolation. The next hosted
pass reached the official native engine but timed out before any fixture marker;
its retained Godot log showed `SignalFishSmoke` was an unloaded placeholder.
The harness had copied the native GDExtension without running the project import
that the existing web-export path already requires. A checksum-verified official
Godot 4.5 arm64 editor reproduced a Godot signal-11 crash after first-scan import
registered the native extension; the same isolated project with that one-line
registry already present loaded `SignalFishSmoke`, emitted `fixture-ready`, and
exited normally. The native harness therefore uses an isolated project copy and
seeds `.godot/extension_list.cfg` before runtime, avoiding both the first-scan
engine crash and contamination of the later web export. A following hosted run
reached the real native scenario and proved the exact large relay
(`wire_bytes=82068`, `padding_bytes=81920`), JSON shutdown, and binary close
readiness. It failed only because the harness also applied the browser load
oracle's required `multi_frame_poll`; the faster native loop accepted one load
frame per poll. The native gate retains the load/shutdown sequencing waits but
now validates only its intended large-inbound oracle, leaving performance
batching to the existing browser jobs.

PR #136 implementation head `088ade56afbee808b6e4de505374424b5095bea4`
is mergeable and all 11 pull-request workflows passed. Godot Web run
32589292120 passed the build/export aggregate, official native smoke, Server
0.7 clean/impaired/3,600-frame soak browsers, Server 0.4 clean compatibility,
and required aggregate gate. The other ten successful workflows were CI,
Coverage, Docs Validation, Examples Validation, No Panics, Security, Semver
Checks, Unused Deps, WASM, and Workflow Lint. The final local tree again passed
the exact mandatory chain, all 16 available pre-commit checks, and all six
pre-push checks. PR inspection found no inline review threads or code findings;
the only automated review messages reported exhausted Copilot quota, the
maintainer-only governance condition already tracked by issue #90. Follow-up
correctness findings remain independently actionable in issues #134 and #135.
