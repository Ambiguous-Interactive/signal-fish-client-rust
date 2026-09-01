# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.12.0] - 2026-09-01

<!-- semver-checks: major -->

### Added

- **Breaking:** the exhaustive `SignalFishError` enum gains
  `PayloadTooDeep { max_depth }`; add an arm to exhaustive matches.
- **Breaking:** the `SignalFishClientApi` trait gains `transport_diagnostics`;
  implementors must add the method (consumers of either driver are
  unaffected).
- Added root re-exports for `ProtocolInfoPayload`, `PlayerNameRulesPayload`,
  and `MeshWaker` (the latter requires the `mesh` and `tokio-runtime`
  features).
- Added a deterministic-testing guide (`docs/testing.md`) covering paused-clock
  and caller-cadence recipes for both drivers.

### Changed

- The pinned server compatibility binding moved from Server 0.7.0 to the
  released Server 0.8.0 (commit `d79dcdc7549777c8c2bd9fcb2d132641532d8c86`;
  tarball digest updated in the E2E matrix and Godot workflows). The vendored
  wire samples and AsyncAPI authority are byte-identical to the previously
  pinned post-0.7 preview, so no client wire changes were required, and the
  byte-frozen v2 relay floor keeps Server 0.7.0 deployments interoperable.
- `ErrorCode::description()` texts were aligned with the Server 0.8.0
  authority for `InvalidAppId` (control characters and the 256-byte cap are
  now refused in every policy mode), `ServerDraining` (also refuses
  reconnection and spectator joins), `ReconnectionFailed` (a refusal on an
  already-closing socket does not consume the token),
  `UnsupportedProtocolVersion` (also covers pre-v3 connections sending
  newer-protocol frames), `RateLimitExceeded`, `ActivityTimeout`, and
  `RoomSessionIncompatible`.
- Documented `LobbyStateChanged`'s advisory `all_ready` semantics (a later
  join is always unready and triggers no corrective broadcast; recompute
  readiness on membership changes and treat `start_game` as the only
  authoritative gate), matching the Server 0.8.0 protocol documentation.
- `WebSocketTransport::connect_with_options` now logs a warning when a
  token-binding offer proceeds over a plain `ws://` URL, where the resulting
  proofs are forgeable by an on-path observer.
- The custom-transport guide documents the mandatory `ProtocolInfo`
  negotiation step that scripted test peers must send between
  `Authenticated` and any further server traffic, and the docs site's
  model-facing `llms.txt` now lists every guide page.

- **Breaking:** `RateLimitInfo`'s `per_minute`, `per_hour`, and `per_day`
  are now `u64` instead of `u32` (update literals/casts that assumed
  `u32`); a server reporting a limit above `u32::MAX` no longer fails
  authentication.
- The Godot adapter's `watermark_hits` and `backend_capacity_hits`
  diagnostics now count each deferred send once instead of once per poll.
- The object-safe client trait's diagnostic accessors (`send_capacity`,
  `max_send_capacity`, `stats`, `snapshot`) are now `#[must_use]`.
- Documented that the async client handle and its event receiver are
  `Send + Sync`, pinned by compile-time assertions on both drivers.
- Raised the declared dependency floors to verified values (`rustls`
  0.23.16, `serde_json` 1.0.68, `tracing` 0.1.35, dev-only `tokio` 1.50)
  so a downstream resolver without this repository's lockfile cannot
  assemble a broken build.
- Documented teardown semantics: plain-dropping the mesh controller never
  disconnects driver peers (aborting the signaling transport is its whole
  teardown), the Godot transport's exact abort/graceful-close split, and
  the async handle-drop race where an immediate graceful close can win
  over the abort fallback.
- Documented custom-transport panic semantics: a panicking `Transport`
  method kills the async loop (event channel closes without `Disconnected`,
  parked reliable sends resolve `NotConnected`) and propagates into the
  caller on the polling driver, where `close()` heals.
- Documented that a room creator already holds authority, so an immediate
  `request_authority(true)` is refused with `AuthorityConflict`.
- Documented `GoingAway`'s absolute epoch-ms deadline and seconds retry hint,
  `RelayStats`' validated invariants (violations report as
  `ProtocolViolation { Counters }`), the `u8` `max_players` ceiling (oversized
  values fail the frame as `DecodeFailed`), `PlayerNameRulesPayload`'s
  unvalidated pass-through, and corrected the wasm docs' `tokio-runtime` /
  `transport-websocket` guidance.

### Fixed

- Prepare Release now also stamps the tracked
  `tests/emscripten-harness/Cargo.lock`; its stale recorded version failed
  the WASM workflow's Emscripten lane on every release branch.
- Debug builds of downstream crates no longer log a spurious
  "must be driven by SignalFishPollingClient" error on the Emscripten
  transport's first receive poll: the debug-only waker-misuse check relied on
  best-effort waker identity that does not hold across crate boundaries and
  was removed (the polling-only contract remains documented).
- Token-binding connections are now robust against serde_json's
  `arbitrary_precision` feature being enabled by a consumer crate:
  previously that unification delivered numbers as private marker maps
  that bypassed the strict number gate and could corrupt outbound
  payloads, so single-member objects carrying serde_json's private marker
  key with a string value are now classified as numbers in all builds,
  while floats, exponents, and out-of-range integers keep their existing
  refusals (only the literal `-0` differs: unified serde_json reports it
  as the integer `0`, whose canonical form matches the server's RFC 8785
  output).
- Deeply nested caller JSON (game data, raw signals, custom connection
  info) is now refused at the call site with `PayloadTooDeep` instead of
  risking a stack-overflow process abort during serialization; the bound
  is 128 nested containers.
- `MeshSession` ignores `NewPeer` directives while the selected transport
  is not WebRTC.
- v2/v3 dialect crossings are now rejected as lifecycle violations:
  out-of-bounds `ProtocolInfo.max_outbound_message_size`, v2 frames
  carrying the v3-only size field, and v2 `RoomJoined` frames carrying
  `reconnection_token` or `ice_servers`.
- The Emscripten transport no longer acknowledges sends on a connection
  the browser already closed (they fail terminally and keep the frame),
  fuses on non-UTF-8 text frames instead of dropping them, and reports the
  queued `onclose` metadata when it arrives behind `onerror`.
- WebSocket URLs whose scheme is not lowercase `ws://` or `wss://` are
  rejected with `InvalidConfig` before any network I/O.
- The Godot adapter applies Godot's web capacity boundary on every wasm
  target, not just Emscripten.
- The async driver's `transport_diagnostics()` now refreshes during the
  terminal drain instead of freezing at the pre-teardown value.
- The Godot adapter classifies a caller-wrapped peer that already closed or
  is natively closing after an open connection (wire or engine-synthesized
  close code) as a post-open close with metadata instead of a "closed
  before opening" failure (never-opened (`-1`), abnormal (`1006`/`1015`),
  and pre-open web closes keep the failure classification; the unknowable
  initiator of a pre-wrap close is reported as peer-initiated), and crate
  docs now record that Godot's native backend does not validate text-frame
  UTF-8, so a corrupt text packet surfaces a terminal receive error that
  ends the stream at the driver.
- Corrected docs: `set_ready()` toggles readiness (call it once per room
  stay), `DecodeFailed.message_type` is `None` for frames over 512 bytes,
  `with_max_players` takes a `u8` (values above 255 cannot be expressed),
  `MeshController` requires
  `tokio-runtime`, the WASM guide warns pthreads is single-thread-only and
  outbound pacing is not surfaced, at Godot's inbound bounds web drops new
  frames while native backpressures, the polling client's `close()`
  documents that already-buffered inbound frames are drained and discarded
  without being decoded or delivered, and the token-binding guide
  classifies challenge-window transport errors.
- The self-contained mesh example's scripted server spoke an obsolete
  protocol flow and hung; it now completes end-to-end and the examples
  surface protocol violations instead of silently discarding them.
- Corrected recommended usage in the docs: the `wasm.md` guide now installs
  and uses the pinned `nightly-2026-03-01` toolchain consistently and
  quotes the actual user-side error for the wrong-target Emscripten
  transport, and the Godot adapter README's Quick Start lists the `godot`
  dependency its snippet imports and pins both crates to the previously
  released version.
- The guide's `SignalFishError` reference table now lists the
  `PayloadTooDeep { max_depth }` variant, matching the errors guide; the
  table predated the variant.
- The Godot adapter README's dependency pins now advance automatically
  during release preparation instead of keeping the previously released
  version after the next release cut.

## [0.11.0] - 2026-08-28

<!-- semver-checks: major -->

### Added

- Added the 0.11 migration guide (`docs/migration-0.11.md`, linked from the
  site navigation and the errors and related guides) covering the breaking
  error-surface changes: the boxed transport error causes and the new
  `SignalFishError::InvalidConfig` reclassifications.
- Added root re-exports for the `PlayerId` and `RoomId` identity aliases that
  appear throughout the client API (`reconnect`, `send_signal`,
  `ClientSnapshot`), so typed helpers no longer need a deep
  `signal_fish_client::protocol::` import.
- Added `ErrorCode::RoomSessionIncompatible` (wire
  `ROOM_SESSION_INCOMPATIBLE`) for the post-0.7 server response used when a
  running room's sticky peer-to-peer topology/transport pair is incompatible
  with the joining connection's negotiated capabilities. The vendored
  AsyncAPI authority advances to server commit
  `5de9105e4c269a29919ae29880f5b67fc8d630c3`; released Server 0.7.0 runtime
  compatibility remains unchanged.
- Added the opt-in `internal-fuzz-facade` feature (which builds on the
  native token-binding support) exposing a `#[doc(hidden)]`, semver-exempt
  module that lets the repository's new `fuzz_token_binding` cargo-fuzz
  target drive the internal challenge parsing, canonicalization, and
  proof-preparation paths without widening the supported public API. The
  target is wired into the Deep Safety fuzz lane with seed inputs;
  production builds are unaffected.
- Bounded the Emscripten WebSocket transport's callback-bridged inbound queue
  at 8 MiB by default through the new
  `EmscriptenWebSocketConnectOptions::max_inbound_queue_bytes` option and the
  matching `connect_with_options` constructor. Callback-delivered frames that
  exceed the bound — individually, in buffered aggregate, or as a
  zero-length-frame flood (which reserves a small minimum charge) — are
  refused before any payload copy and fuse the transport with one terminal
  receive error, mirroring the native transport's inbound-size policy instead
  of growing memory without limit between game-loop polls. `None` restores
  the previous unbounded buffering.
- Added the negotiated outbound message-size contract to `ProtocolInfoPayload`
  as the v3-only optional field `max_outbound_message_size`, pinned to server
  commit `d5b3135fda53a2a7de69c5ea54faefa95ca9a5b9`. The negotiated value
  mirrors into the new `ClientSnapshot::server_max_outbound_message_size`
  field: the maximum complete encoded application payload, in bytes, the
  connected deployment sends in one WebSocket message (default 8 MiB,
  configurable up to 64 MiB). A server-side delivery above its own limit is
  rejected whole and closes that connection with RFC 6455 close code 1009
  (`outbound_message_too_large`), which surfaces through the `Disconnected`
  event's close reason. Frozen-v2 negotiations and servers
  that omit the field leave both the wire and the snapshot unchanged.
- Added negotiated room-operation correlation for protocol-v3 connections.
  `ClientMessage::Authenticate` gains the optional `requested_capabilities`
  field. Configurations that can negotiate v3 request `room_operation_ids`;
  after the server echoes it in `ProtocolInfo`, all five directed room
  operations use a fresh client-generated UUID. Missing echoes and v2
  negotiations retain the legacy wire unchanged. New public protocol surface
  includes `RoomOperationId`,
  `ROOM_OPERATION_IDS_CAPABILITY`, `RoomOperationRequest`,
  `RoomOperationResult`, `ClientMessage::RoomOperation`,
  `ServerMessage::RoomOperationResult`, and
  `SignalFishEvent::RoomOperationFailed`. Exact wire and MessagePack goldens
  are pinned to the post-0.7 protocol authority recorded in
  `tests/compatibility.toml`. The added fields and exhaustive
  enum variants are breaking API additions for the forthcoming 0.11 release;
  published 0.10 does not expose them.
- Added opt-in native `signalfish.tokenbinding.v2` support through the
  `token-binding` feature. New public types are `TokenBindingMode`,
  `TokenBindingStatus`, `TokenBindingScheme`, `TokenBindingChallenge`, and
  `TokenBindingFailure`. `WebSocketConnectOptions` gains public
  `token_binding` / `token_binding_challenge_timeout` fields and matching
  `with_token_binding` / `with_token_binding_challenge_timeout` builders.
  `WebSocketTransport` gains `connect_with_tls_config`,
  `token_binding_status`, and `token_binding_challenge`. Strict challenge
  validation, shared JSON/binary proof sequencing, canonical Server 0.7
  goldens, and pinned positive/negative required-WSS smokes back the surface.
  Certificate-capable custom rustls connections automatically bind proofs to
  the exact selected mTLS leaf certificate, supporting Server 0.7's
  `require_client_fingerprint=true` profile without a caller-supplied claim.
  With token binding disabled, the handshake does not offer its subprotocol;
  the default dependency graph remains unchanged.
- Added `WebSocketConnectOptions::max_inbound_message_size` and its matching
  builder. New native WebSocket connections limit both individual inbound
  frames and assembled messages to 8 MiB by default; callers can raise the
  inclusive limit or set it to `None` for an unbounded codec. Streams supplied
  through `WebSocketTransport::from_stream` retain their caller-owned limits.
- Added `GodotWebSocketTransport::one_frame_escape_frames()` so admission
  diagnostics distinguish one individually oversized empty-buffer escape from
  the same cumulative byte total spread across multiple frames.
- Added Signal Fish Server 0.7.0 protocol conformance, including
  `SessionGeneration`, `DirectEndpoint::{host, port}`,
  `SessionPlanPayload::generation`, `SessionPlanPayload::direct_endpoint`,
  `ClientMessage::Signal::generation`, `ServerMessage::Signal::generation`,
  `SignalFishEvent::SessionPlan::{generation, direct_endpoint}`,
  `SignalFishEvent::SignalReceived::generation`,
  `ClientSnapshot::session_generation`,
  `MeshSession::generation`, `MeshSession::direct_endpoint`,
  generation fields on `DriverEvent::Signal`, `DriverEvent::Connected`,
  `DriverEvent::Disconnected`, and `DriverEvent::Data`,
  `ErrorCode::UnsupportedProtocolVersion`,
  `SignalFishError::SessionPlanUnavailable`, and
  `SignalFishError::StaleSessionGeneration::{attempted, current}`. Added
  `ErrorCode::NON_EMITTED` to identify the six error variants retained for
  older-server compatibility but no longer emitted by 0.7.
- Added generation-bound `send_signal_for_generation` and
  `send_raw_signal_for_generation` methods to `SignalFishClient`,
  `SignalFishPollingClient`, and `SignalFishClientApi`, so custom drivers can
  atomically refuse stale output when a replacement session plan races their
  event loop.
- Added `ClientSnapshot::{requested_game_data_format,
  effective_game_data_format}` and matching async, polling, and
  `SignalFishClientApi` accessors. Configuration omission now remains visible
  separately from the format selected by the server.
- Added `ClientSnapshot::{session_topology, session_transport}` plus
  `session_topology()`, `session_transport()`, and `is_p2p_active()` to the
  async client, polling client, and `SignalFishClientApi`. Applications can now
  distinguish negotiated local capability from the server-selected active path.
- Added the public `RoomRole::{Player, Spectator}` state,
  `ClientSnapshot::room_role`, matching async/polling/trait accessors, and
  `SignalFishError::{AlreadyInRoom, RoomOperationPending, WrongRoomRole,
  AuthorityRequired}` for synchronous membership-safe command admission.
- Added `ClientSnapshot::transport_ready` and matching async, polling, and
  `SignalFishClientApi` accessors so applications can distinguish a client-owned
  connecting transport from a completed handshake.
- Added `SignalFishClient::transport_diagnostics()`, mirroring the polling
  driver's accessor so tokio users can read backend buffering, watermark, and
  capacity counters when tuning congested deployments instead of mistaking
  them for command-queue saturation. The transport task refreshes its sample
  at every scheduling step — loop-cycle start and each pending-send or receive
  poll — so deferred admission hits are visible while backpressure is in
  flight.
- Re-exported the everyday protocol value types the public API requires in
  signatures and events from the crate root: `ConnectionInfo`,
  `GameDataEncoding`, `RelayTransport`, `LobbyState`, `PlayerInfo`,
  `SpectatorInfo`, `PeerConnectionInfo`, `RateLimitInfo`, and
  `SpectatorStateChangeReason`.
- Derived `PartialEq`/`Eq` for `SignalFishConfig` and `JoinRoomParams` so
  applications can compare constructed configurations and parameters in
  assertions.
- Added the opt-in `require_client_fingerprint` connect option on
  `WebSocketConnectOptions` (builder:
  `with_require_client_fingerprint`). When set, a connect fails with a typed
  error unless the transport observes rustls selecting a real X.509 client
  certificate signer, letting deployments enforce the strictest mTLS
  token-binding profile locally instead of trusting the server to reject
  fingerprint-less proofs. The check happens before network I/O on paths that
  cannot satisfy it (plain connects, custom-TLS connects without token
  binding) and after the handshake when no signer was selected, including
  Optional mode's unsigned fallback.
  **Breaking:** `WebSocketConnectOptions` gains a public field and
  `TokenBindingFailure` gains the `MissingClientFingerprint` variant, so
  exhaustive struct literals and matches require updates; both are breaking
  API additions for the forthcoming 0.11 release.

### Changed

- **Breaking:** Runtime transport failures keep their structure.
  `SignalFishError::TransportSend` and `TransportReceive` now carry the
  backend's boxed original error (as the `#[source]` cause) instead of a
  flattened `String`, so `std::error::Error::source()` reaches the root cause
  for programmatic handling: the built-in native WebSocket boxes the
  backend's own `tungstenite::Error` (whose chain reaches the underlying
  `std::io::Error`), and custom transports box whatever error they produce.
  `Display` text is unchanged; a plain string detail still converts via
  `.into()`.
- **Breaking:** Misconfiguration no longer wears an I/O costume. The new
  exhaustive `SignalFishError::InvalidConfig { field, problem }` variant
  reports caller-supplied values rejected before any network I/O — a zero
  `max_inbound_message_size`/`max_inbound_queue_bytes`, a URL that cannot be
  parsed into a WebSocket request (native; Godot's `ERR_INVALID_PARAMETER`),
  a URL containing interior NUL bytes (Emscripten), and `wss://` without the
  opt-in `tls` feature — instead of `Io` with
  `ErrorKind::InvalidInput`/`Other`. Failures determined by the engine or the
  network (Godot's `ERR_UNAVAILABLE`/`FAILED`, malformed server handshake
  responses, HTTP upgrade rejections) keep the `Io` classification.
  Exhaustive `SignalFishError` matches must handle the new variant. These are
  breaking API additions for the forthcoming 0.11 release; published 0.10
  does not expose them.
- `SendBufferFull` now also names "drain events promptly" as a remedy: a task
  awaiting a reliable send while it is the sole event consumer can deadlock
  against a full event channel (which pauses the transport loop that drains
  the command queue), and pacing or capacity changes alone do not fix that
  case. The variant itself and its matching/emission behavior are unchanged.
- Documented quick-start sketches (crate docs, `MeshController` docs, and the
  mesh guide) now request the protocol-v2 game start only once instead of on
  every repeated all-ready update, matching the guidance in the events guide
  and the compiling `basic_lobby` example.
- The shared diagnostic accessors on both drivers —
  `send_capacity`, `max_send_capacity`, `stats`, `snapshot`, and
  `transport_diagnostics`, plus the polling driver's `polling_stats` and
  `queue_age_stats` — are now `#[must_use]`; ignoring them is a compile
  warning instead of silently discarding an entire coherent diagnostics view.
- `MeshSession` now implements equality comparison like its sibling coherent
  state views (`ClientSnapshot`, stats types), so folded session states can be
  compared and asserted directly.
- `SignalFishPollingClient::poll` is now `#[must_use]`; ignoring the returned
  event list is a compile warning instead of silently discarding that poll
  cycle's deliveries (the leading cause of "stuck in lobby" misuse reports).
- `SignalFishError::Timeout` now names its cause and remedy: the WebSocket
  handshake did not complete within the deadline given to
  `WebSocketTransport::connect_with_timeout`. The variant itself and its
  matching/emission behavior are unchanged.
- The crate-level documentation gained a cargo-feature reference table
  (making the opt-in `tls` feature for `wss://` discoverable on docs.rs), a
  pointer from the Quick Start to the compile-checked
  `examples/basic_lobby.rs`, documented cancelled-`shutdown()`
  semantics (dropping the future mid-teardown leaves the loop's own bounded
  teardown in charge; a later call returns promptly), a Godot adapter README
  with requirements and a working `_process` integration snippet, and
  accuracy fixes across the guide's capacity-clamp, snapshot-field,
  Emscripten-deprecation, and feature-flag coverage.
- The optional `token-binding` dependency `base64` moved from 0.22 to 0.23.
  The STANDARD engine API used for token-binding proofs is unchanged; only
  the version requirement advances.
- Workspace release builds now enable `overflow-checks`, so integer overflow
  aborts loudly instead of wrapping silently. Every supported configuration
  (debug tests, release builds, fuzzing, and Miri) now validates the same
  arithmetic semantics, and a promoted workspace-wide
  `clippy::arithmetic_side_effects` denial keeps raw integer math on
  server-controlled values checked/saturating/wrapping-explicit. Delivery
  accountability gap/counter arithmetic is machine-checked accordingly;
  observable protocol behavior and wire bytes are unchanged.
- **Breaking:** Directed room operations are now refused before the server
  confirms authentication. `join_room`, `leave_room`, `reconnect`,
  `join_as_spectator`, and `leave_spectator` return the new
  `SignalFishError::NotAuthenticated` variant without enqueuing it,
  in both drivers, instead of admitting a command whose admission fence the
  inbound lifecycle gates could never release (a permanent
  `RoomOperationPending` after any early server response). Exhaustive
  `SignalFishError` matches must handle the new variant. Follow the documented
  flow and wait for `SignalFishEvent::Authenticated`; non-room commands keep
  their existing pre-authentication behavior.
- **Breaking:** `WebSocketConnectOptions` gains the public
  `max_inbound_message_size` field, and built-in native WebSocket connections
  no longer inherit tungstenite's larger receive defaults. Oversized input is
  reported once as `SignalFishError::TransportReceive`, then the transport
  becomes terminal. The 8 MiB default is a protective client policy rather
  than a protocol maximum; larger deployments must configure it explicitly.
- Reduced the published `signal-fish-client` crate archive to library source,
  its required token-binding unit-test vector, and package metadata.
  Repository integration tests, other standalone wire fixtures, examples,
  progress records, and changelog history remain available from the linked
  source repository instead of being duplicated on crates.io.
- Adopted Ambiguous Interactive's approved Vector design system across the
  README and MkDocs site, including a client-specific source-vector identity,
  accessible light and dark oceanic palettes, self-hosted typography, clearer
  start actions, responsive navigation with keyboard-safe phone and tablet
  overlay boundaries, and published brand/font attribution.
- Reduced allocation churn when either client sends direct JSON string game
  payloads of at least 4 KiB. The wire format, frame ownership, backpressure,
  delivery accounting, and smaller or structured JSON paths are unchanged.
- Reorganized the guide around first connection, SDK usage, multiplayer
  choices, and advanced reference topics. The README is now a concise on-ramp
  that links to the canonical setup, engine, protocol, and transport pages
  instead of duplicating their detailed instructions.
- **Breaking:** `WebSocketConnectOptions` gains token-binding mode and challenge
  timeout fields, and exhaustive `SignalFishError` matches must handle
  `TokenBinding(TokenBindingFailure)`. Browser, Emscripten, Godot, and
  post-handshake `from_stream` transports remain incapable of required mode
  because their APIs do not expose the RFC 6455 handshake key.
- Clarified that `Transport` requires a complete, ordered text/binary frame
  stream for the intended signaling server and that `RelayTransport::Udp` is
  ignored legacy `JoinRoom` metadata under Server 0.7, not a built-in UDP
  backend or executable relay request. Custom stream/datagram adapters own
  framing, signaling-server trust/source binding, fragmentation,
  loss/duplicate/reorder, and error policy before yielding `TransportFrame`s.
- **Breaking:** `Transport::abort` is now required. Custom transport
  implementors must provide a prompt, nonblocking, non-panicking, idempotent
  backend-abandonment path that releases or safely detaches resources and
  discards retained sends; completed cleanup is one-shot and failed external
  cleanup may be retried safely. Both clients
  invoke it when the configured close deadline expires, after a graceful-close
  error, and when their owner is dropped without completing graceful close;
  drivers perform no later transport polling.
- **Breaking:** the new fields and variants listed above change exhaustive
  struct literals and matches. `SignalFishClientApi` implementors must add the
  two generation-bound send methods. These APIs require the forthcoming 0.11
  release; published 0.10 does not expose them.
- **Breaking:** `WebRtcDriver::connect`/`on_signal` and every `DriverEvent`
  carry the authoritative session generation. `MeshController` now rebuilds
  every retained peer when a generation changes, discards stale inbound,
  buffered, and driver-produced signals, rejects senders outside the current
  plan, and never routes `Direct` or `Relay` plans through a WebRTC driver.
  Protocol-v3 signal sends before the first `SessionPlan` now fail locally.
  Generation-less server 0.4 plans and signals remain supported adaptively.
- **Breaking:** the exhaustive `ClientSnapshot` struct has two new game-data
  negotiation fields. `SignalFishClientApi` provides corresponding default
  requested/effective format accessors through `snapshot()`.
- **Breaking:** the exhaustive `ClientSnapshot` gains selected topology and
  transport fields. `supports_mesh()` now precisely means negotiated v3 plus
  advertised WebRTC and at least one `Host` or `Mesh` topology; custom
  WebRTC + relay-only configurations now return `false`. Use the selected-plan
  snapshot fields or `is_p2p_active()` for routing decisions.
- Reconnect is now a hard WebRTC plan boundary: `MeshSession` and
  `MeshController` clear the previous plan and driver peers at `Reconnected`,
  then wait for Server 0.7's fresh live `SessionPlan` before authorizing new
  signaling.
- **Breaking:** room commands now enforce the Server 0.7 player, spectator,
  no-membership, and authority matrix before protocol and bounded-queue checks.
  An admitted join, leave, or reconnect fences subsequent room work until a
  matching typed terminal response, while `ping` remains available. Generic
  errors and absent responses stay fail-closed until connection teardown. The
  exhaustive `ClientSnapshot`
  and `SignalFishError` types gain the membership fields and variants above.
  `ClientSnapshot::player_id` and `current_player_id()` now consistently mean
  the local player-or-spectator participant ID and clear on confirmed exit.
- **Breaking:** the exhaustive `ClientSnapshot` adds `transport_ready`.
  `connected` now explicitly means a nonterminal client-owned transport attempt;
  `Connected` and `transport_ready` identify handshake completion. Commands
  remain queueable while connecting.
- Changed `ClientStats` counting boundaries: `game_data_sent` increments when
  the transport takes frame ownership, including accepted sends that later
  fail, while `game_data_received` increments after successful protocol decode,
  including stale or quarantined data suppressed before application delivery.

### Fixed

- Unbricked release preparation for every future version bump: the perf-lab
  fixture's default `sdk_version` embedded the crate version in the
  Authenticate wire bytes, so 27 of 28 pinned protocol-ledger digests
  changed on each bump and Prepare Release's mandatory verification failed
  until the pins were refreshed by hand. The fixture now omits the field
  (like its ProtocolInfo fixture), so the pins cover protocol behavior
  rather than release metadata.
- Accepted authoritative `SpectatorLeft` frames that omit the optional
  `room_id` (server removal, spectator disconnect, or room close). The wire
  authority marks the field optional, but the client required it and latched
  quarantine on a schema-valid exit under the default policy; a named room
  must still match the current one.
- Corrected published documentation that reversed actual behavior or
  misdescribed the API: the mesh guide now states that an empty `SessionPlan`
  clears pre-gathered ICE servers (plans replace the list wholesale, they
  never merge), the spectator-leaves event and protocol tables describe
  authoritative exits, the `DecodeFailed.raw_prefix` field documents that
  it carries verbatim frame content, and the events and web/Godot guides no
  longer fabricate a lobby ready-count denominator or silently swallow
  command errors in example sketches.
- Fixed the release tooling's remaining fence-blind spots: release
  preparation, the changelog cut, semver-policy derivation, the previous
  baseline lookup, and release-notes extraction now track CommonMark fenced
  code blocks everywhere the release-intent parser already does, so a
  documentation example quoting changelog syntax inside `[Unreleased]` can
  no longer brick preparation or silently truncate the cut release notes. A
  crates.io response missing its version object now fails with a clear
  release error instead of an uncaught Python traceback.
- Made Release recovery reruns deterministic: the workspace root
  `Cargo.lock` is now tracked — release preparation updates it in lockstep
  with the manifests, and the release workflows package and publish with
  `--locked` — so a rerun after a transient failure rebuilds byte-identical
  `.crate` archives even when dependencies published compatible new versions
  in between; previously such drift could make the checksum-verified recovery
  refuse to complete.
- Hardened the release plumbing against silent no-ops: Prepare Release now
  fails loudly when the required-checks policy file is missing, malformed, or
  empty instead of dispatching zero checks and stalling the release PR, the
  Release workflow re-verifies default-branch HEAD immediately before
  creating the release tag, and the required-checks gate fails closed on
  truncated pagination or malformed check-run payloads.
- Fixed the Prepare Release blocker that would fail every future release run:
  the compatibility manifest now carries both a release-stamped top-level
  `synced` date and real upstream-refresh dates under `[protocol_authority]`,
  but the release tooling still required exactly one `synced` date in the
  whole file. Releases now stamp only the top-level date and never rewrite
  section-level upstream provenance (including the vendored PROVENANCE.toml
  files), and a checkout-level guard catches inventory drift in tests instead
  of on release day.
- The published model-context page (`llms.txt`) now tracks the release
  version automatically instead of advertising a stale crate version after
  each release.
- Release-version notes in the README and the mesh, getting-started, and
  client guides no longer embed the next version number or enumerate
  unreleased features, so release preparation can no longer rewrite them
  into stale claims; they point at the changelog instead.
- Fixed the native WebSocket send path flattening its error cause: a
  synchronous `start_send` rejection (the classic oversized-outbound-message
  refusal) boxed the formatted text instead of the backend's original
  `tungstenite::Error`, so `Error::source()` could not reach the root cause
  on that one path. The structured cause now survives the frame-restoration
  classification; `Display` text, retryability, and exact frame restoration
  are unchanged.
- Fixed misleading token-binding connect errors for HTTP upgrade rejections:
  a non-101 response (404 wrong path, 401/403 rejected auth, 500, ...) on an
  `Optional` or `Required` token-binding connect was reported as
  `TokenBindingFailure::NegotiationRejected` ("the server rejected
  token-binding negotiation"), hiding the actual HTTP status and suggesting a
  server capability problem for what is usually a URL or credential
  misconfiguration. HTTP upgrade rejections now surface as `Io` carrying the
  underlying HTTP error — the same classification the `Disabled` mode already
  used — so one misconfigured URL fails the same way in every mode.
  `TokenBindingFailure::NegotiationRejected` remains in the exhaustive enum
  as a reserved, no-longer-produced variant.
- Fixed `TokenBindingFailure::MalformedChallenge` mislabeling the
  control-flood case: a selected token-binding connection whose server sent
  only control frames (and never any challenge) was reported as "the server
  sent a malformed token-binding challenge" even though no challenge was
  ever delivered. The variant now reads "the server did not send a valid
  token-binding challenge" and its documentation covers both faces (an
  invalid first application frame, or the control-frame budget exhausting
  before any challenge).
- `WebSocketTransport::connect_with_tls_config` now logs a warning when
  called with a plain `ws://` URL: tokio-tungstenite bypasses the connector
  for plain URLs, so the entire custom TLS configuration — including any
  mTLS client identity — was silently unused.
- Corrected published documentation: `SignalFishError::ServerError` is
  reserved and no current server/SDK combination constructs it (server
  errors surface as events); `SessionPlanUnavailable`'s trigger list now
  includes relay (non-WebRTC) session transports; the six compatibility-only
  `ErrorCode` variants are labeled at their definitions and in the docs
  tables; and `require_client_fingerprint`'s failure timing is stated
  precisely (pre-I/O on every path without a tracking resolver, after the
  handshake and challenge on the custom-TLS token-binding path, including
  the plain `ws://` case where no selection can ever be observed).
- Fixed `SignalFishEvent::Disconnected` reason mislabeling for custom
  transports: close metadata that the transport did not report as
  peer-initiated was formatted as `"closed by server: …"`; it now formats as
  `"closed by transport: …"`. Peer-initiated close metadata (including every
  built-in transport's WebSocket Close frames) keeps the established
  `"closed by server: …"` format.
- Fixed a liveness defect in protocol-v3 (and legacy-mode) room operations: a
  typed terminal answer whose payload was rejected by delivery accountability
  (for example, a drifting roster snapshot) left the operation's admission
  fence armed forever, so every later join/leave/reconnect/spectator command
  failed locally with `RoomOperationPending` until the connection tore down
  (under every non-disconnecting violation policy). The answered fence is now
  retired as soon as the matching terminal arrives. Violation handling,
  suppression of the invalid payload, and retention of
  mismatched/forged-operation fences are unchanged; as before, a later
  duplicate reply for the already-settled id reports a lifecycle violation.
  Red/green pinned across correlated, legacy-wire, and reconnect paths in
  both drivers' shared core.
- Fixed a release-automation defect that would make every future Prepare
  Release run fail before writing anything: the version-inventory still
  required `docs/examples.md` to mention the workspace version, but that page
  dropped its version reference in the 0.10 documentation reorganization. The
  stale inventory entry is removed, fenced code blocks in `[Unreleased]` can no
  longer masquerade as changelog intent (an unterminated fence fails closed),
  and a checkout-level parity test keeps the inventory aligned with reality.
- The pre-commit Rust source checkers that scanned `tests/` no longer walk
  generated, ignored nested Cargo target trees (such as the 2.1 GB
  `tests/godot-web-smoke/target` produced by Godot web builds): both checkers
  now resolve tracked sources through the git index, so commits stop spending
  minutes grepping build outputs, git failures fail loudly instead of
  reporting a clean scan, and the scan set stays bash-3.2 portable. Tracked
  fixture sources anywhere under `tests/` — including directories named
  `target` — remain fully checked; a portable regression suite pins both
  sides. The same failure class was swept out of every unbounded repository
  walker found: release SBOM discovery no longer descends into build trees,
  markdown lint is pinned to markdownlint-cli2 (whose excludes cover nested
  targets) in all five invocation sites, admonition scanning prunes skipped
  directories at descent time, and link-check excludes match target trees at
  any depth.
- `ServerErrorInfo`'s derived `Debug` no longer prints the server-authored
  free-text message; it reports `message_len` plus the structured error code,
  matching the close-reason redaction on `TransportCloseInfo`. The message
  remains available through the public field.
- `MeshController` now retries outbound WebRTC handshake signals and
  `TransportStatus` up/down reports that queue congestion or a pending directed
  room operation temporarily refuses. Previously a refused signal could stall
  a handshake after a failed room operation, while a refused status edge was
  permanently suppressed by the already-committed local 0↔1 peer boundary and
  could leave remote fallback/liveness state stale. Signals fence later driver
  output to retain wire order while signaling events remain drainable; status
  coalesces to its latest state without fencing driver data. Authoritative
  teardown discards obsolete pending work.
- Absorbed one benign wire race around voluntary spectator leaves. When an
  authoritative spectator exit (`Disconnected`, `Removed`, or `RoomClosed`)
  tore down the room before the server's mandatory reply to an admitted
  `leave_spectator` arrived, that superseded reply was misclassified as a
  lifecycle violation — latching quarantine under the default policy, or
  tearing down a healthy connection under `Disconnect`. Exactly one matching
  late reply (correlated result or plain `SpectatorLeft`) is now silently
  consumed; duplicates, mismatched ids, and unrelated frames still violate.
- `JoinRoom` commands now omit unset optional members (`room_code`,
  `max_players`, `supports_authority`, `relay_transport`) on the wire in both
  the legacy and negotiated-envelope forms, matching the protocol schema's
  omission convention and the server's own client samples. They were
  previously serialized as explicit JSON `null`s, which strict JSON-Schema
  validators reject even though the reference server tolerated them.
- Both drivers now fail fast when a custom transport reports send success with
  a nonempty caller frame slot, whether it retained the initial frame or put a
  frame back during a continuation poll. The async driver previously treated
  such a contract breach as a completed send and silently dropped the payload;
  the polling driver
  either retried it once per poll without bound or dropped a refilled
  continuation slot. Both now terminate the connection with a terminal send
  error naming the breach, mirroring their existing handling of genuine send
  failures.
- The async client now calls `Transport::begin_poll_cycle` once per loop
  iteration like the polling driver, so backends that sample buffering per
  cycle see the documented callback when driven asynchronously, and the
  never-poll-after-abort guarantee is enforced against real observations
  rather than passing vacuously.
- SDK-created Godot peers now raise Godot's queued-packet cap to 65,536
  alongside the 8 MiB inbound byte buffer. Godot bounds its inbound queue by
  packet count too (engine default 4,096), and its native and web backends can
  silently drop newly arriving frames once either bound fills — so the default
  could silently discard legitimate inbound traffic long before the raised
  byte buffer mattered. Caller-owned peers (`from_peer`) keep their own
  configuration; the crate docs now also record two further engine-imposed
  limits: native builds make pre-CLOSE tail frames inaccessible, and locally
  synthesized close codes cannot be perfectly distinguished from genuine
  peer closes.
- Bounded per-room metadata growth fed by a hostile or pathologically
  churning server. Delivery accountability now refuses, as ordinary
  violation diagnostics, a sender that accumulates more than 16 unresolved
  incarnation announcements or uncovered departures, and a delivery report
  that would push total retained exact gap ranges past 1024 — previously all
  three grew without bound for the whole room stay under churn that never
  completes retirement. Room rosters are now admission-bounded by the server-advertised
  `max_players` (with a wire-absolute `u8` fallback when the latest baseline
  cannot advertise one): an over-capacity `PlayerJoined` is a lifecycle
  violation instead of unbounded roster growth, while duplicates and
  slot-freed rejoins keep working.
- Bounded the session-plan replay fence to the eight most recently superseded
  generations. Previously every generation change retained its superseded
  generation for the whole room stay, so a hostile or pathologically churning
  server could grow client memory without limit during long sessions by
  streaming fresh plan generations. Recent-generation replays — the realistic
  duplicate window on one ordered transport stream — remain fenced; a replay
  older than the fence now degrades to a fresh authoritative plan, which the
  connected server can always send anyway. Authoritative room/session teardown
  still clears the fence.
- Fixed a mesh-signaling availability defect where one routine departure race
  blackholed game data for the rest of the room session. A `Signal` frame from
  a peer the server had just dropped — via `PlayerLeft` or a same-generation
  re-plan — could arrive after the departure was processed while still stamped
  with the current session generation. The frame was valid when the server
  relayed it, but the core classified it as a lifecycle integrity violation:
  under the default `Quarantine` policy that latched quarantine and silently
  suppressed every later game-data delivery until a reconnect, and under
  `Disconnect` it tore the connection down entirely. Departed and
  re-planned-out peers now join the same benign-race fence Server 0.4
  generation-less signaling already used: their late same-generation signals
  are suppressed silently, senders never authorized for the session remain
  violations under every policy, and a peer named by a new plan (or a
  compatibility `NewPeer`) is reauthorized immediately.
- Fixed delivery accountability accepting or wrongly rejecting game data after
  a player fully departed and later rejoined reusing an epoch value. A zero
  terminal (`final_seq = 0`) retired the sender while exact gap ranges from
  that player's older incarnations stayed behind; those dead ranges could then
  "explain" a new incarnation's sequence jump as already-reported loss
  (silently delivering a gapped stream) or collide with its first genuine
  report as an overlapping duplicate (tearing down the connection under the
  `Disconnect` violation policy). Full sender retirement now drops every
  remaining range keyed to that player, so only causal reports from the live
  incarnation are honored.
- Fixed the Godot adapter reporting web-export abnormal terminations — server
  death, network drops, failed handshakes — with `clean: Some(true)` close
  metadata. Godot's web build surfaces engine-synthesized close codes (1006
  for abnormal termination, 1015 for TLS-handshake failure) that RFC 6455
  forbids on the wire, while native reports no code at all, so the same
  failure produced opposite `TransportCloseInfo::clean` values per platform.
  Observed `-1`, `1006`, and `1015` codes now classify as unclean on both
  platforms; real CLOSE-frame codes (1000, app-defined 4xxx) remain clean.
- Fixed a deferred panic when absurdly large channel capacities were requested
  through `SignalFishConfig::with_event_channel_capacity` /
  `with_command_channel_capacity` (or assigned to the public fields directly):
  values above tokio's internal semaphore permit ceiling (`usize::MAX >> 3`)
  previously panicked inside `SignalFishClient::start`, while the polling
  client accepted the same configuration silently. Capacities are now clamped
  to that ceiling everywhere, alongside the existing sub-1 clamp, and the
  field documentation states both bounds.
- Documented the Godot adapter's `GodotBackpressurePolicy` edge values: a
  `Fixed` watermark of 0 degenerates to strict stop-and-wait (one frame in
  flight until Godot drains it), zero `Adaptive::latency_target` drops the
  latency term, and a floor above the ceiling pins the watermark to the
  ceiling (behavior unchanged, previously undocumented).
- Documented that `SignalFishConfig::enable_v3` resets advertised transports
  and topologies to relay-only — overwriting a prior `enable_mesh` call — and
  that `with_protocol_version` defers unknown-version handling to the server,
  so mesh configurations call `enable_mesh` last (or use the power-user list
  builders after it).
- Corrected the published model-context page (`llms.txt`) to advertise the
  current release instead of 0.8.0 and dropped its stale issue reference;
  fixed the delivery guide's shutdown-preemption version anchor (client 0.8.0,
  not 0.7.0); documented the Emscripten transport's
  `connect_with_options` / inbound-queue bound on the wasm docs page; and
  completed `concepts.md`'s `SignalFishError` table with the four missing
  variants (`NotAuthenticated`, `SessionPlanUnavailable`,
  `StaleSessionGeneration`, `TokenBinding`).
- Documented deterministic recovery from the VS Code Dev Containers rebuild
  race where concurrent cleanup attempts report that container removal is
  already in progress, allowing contributors to retry without deleting the
  repository's named build caches.
- Fixed `MeshSession` continuing to report a departed host through `host()`
  and `direct_endpoint()` until the next plan arrived. A host's `PlayerLeft`
  now clears both immediately, matching the shared core's authority handling,
  so consumers cannot dial a dead endpoint in the re-planning gap; the
  replacement `SessionPlan` owns host re-election.
- Documented two driver-contract boundaries that previously lived only in the
  code: the polling client's post-send-failure ready-frame drain is bounded by
  the configured receive budget against the standard 64 frames / 64 KiB — at
  most `min(receive_frames, 64)` complete frames, stopping at the first frame
  that reaches the byte bound — so frame-budgeted callers never see teardown
  work beyond their per-poll frame bound (the async driver always drains the
  standard budget), and the WebSocket transport's retryable `WriteBufferFull`
  refusals apply to direct `Transport` operation while the built-in drivers
  map any send error, including a restored refusal, to a terminal disconnect.
- Fixed `MeshController` retaining a stale session and live peer-driver state
  when its signaling event stream ended without room for the best-effort
  `Disconnected` event. End-of-stream and explicit shutdown now disconnect
  every known peer exactly once, clear the mesh view, fuse `recv`, and prevent
  post-termination `send_to` calls from reaching the WebRTC driver.
- Documented the Godot adapter's outbound frame bound: Godot refuses to buffer
  an outbound message once the peer's outbound buffer would overflow, so a
  single frame larger than `outbound_buffer_size` (65,535 bytes by default)
  parks as `Pending` on native builds — at or above it on web exports — with
  only capacity diagnostics growing. SDK-created peers keep that legacy
  outbound default and raise only the inbound buffer; the crate guide now
  explains how to admit larger frames with `from_peer`.
- Hardened async-driver terminal teardown: observing the shutdown signal is
  now tracked explicitly by a sticky wrapper instead of being inferred from
  event-delivery outcomes, so no future call-site change can re-poll the
  completed `shutdown` oneshot — which would panic the transport task
  mid-teardown, closing both channels and failing every parked reliable
  sender with `NotConnected` instead of completing graceful teardown.
  Documented event-delivery and budget contracts are unchanged. (The polling
  client emits synchronously from `poll()` and was never affected.)
- Fixed a `ProtocolViolationPolicy::Disconnect` teardown hanging past
  [`shutdown_timeout`](SignalFishConfig::shutdown_timeout) when the violating
  frame's event batch meets a consumer that stopped draining and `shutdown`
  is never called. The policy-disconnect batch now shares one shutdown budget
  with the farewell delivery and transport close — the same bound the
  send-failure teardown already applies — so the loop terminates and every
  sender parked on the command queue resolves. The polling client was never
  affected.
- Corrected documentation drift found in review: `send_game_data_reliable`'s
  documented error surface now matches its membership checks, the concepts
  guide lists all four synthetic events including `ProtocolViolation`, the
  errors guide states where `Timeout` originates, the reconnection events
  describe all three `ReplayStatus` variants, and the client guide gains a
  consolidated end-to-end reconnect recovery policy with per-error-code
  fallbacks.
- Fixed a terminal disconnect wedging the async transport loop forever when
  the event consumer stops draining and `shutdown` is never called. Terminal
  delivery is now bounded by [`shutdown_timeout`](SignalFishConfig::shutdown_timeout):
  on expiry the loop attempts one nonblocking delivery of the terminal
  `Disconnected` event, closes or aborts the transport, and exits — releasing
  every sender parked on the command queue instead of leaking the task.
  Shutdown-preempted multi-event frames now attempt their remaining events
  through the same nonblocking fallback before abandoning them. (The polling
  client emits events synchronously from `poll()` and was never affected.)
- Fixed delayed or replayed `SessionPlan` messages rolling WebRTC signaling
  authority back to a superseded generation. Both clients now reject every
  previously superseded non-null generation before changing the selected
  generation, topology, transport, peer set, or mesh revision. Current-plan
  duplicates and generation-less Server 0.4 plans remain compatible, and room
  or connection teardown clears the replay history.
- Fixed async and polling clients discarding an immediately ready server
  `Error` farewell when an outbound transport send failed at the same time.
  Both drivers now freeze new commands, process bounded already-ready frames
  before `Disconnected`, preserve `last_server_error` attribution, and retain
  the original send error unless peer-initiated close metadata is available.
  Native WebSockets retain buffered inbound frames after send failure, async
  reliable sends cannot bypass the terminal freeze through an outstanding
  channel permit, and polling error reasons no longer repeat their prefix.
- Fixed standard Godot connections rejecting or omitting valid server messages
  larger than Godot's 65,535-byte inbound default. SDK-created peers now set an
  8 MiB inbound buffer before connecting, while `from_peer` keeps advanced
  caller configuration intact. Official native and web smokes require an exact
  over-64-KiB Signal Fish frame. The setting may reserve roughly 16 MiB per
  peer inside Godot and is a protective default, not a protocol maximum.
- Prevented a delayed terminal response from an older same-kind join, leave,
  reconnect, spectator join, or spectator leave from clearing or mutating the
  current operation fence after correlation is negotiated. Wrong, stale,
  duplicate, malformed, and unwrapped directed results are rejected without
  consuming current state in both async and polling clients.
- Corrected the built-in examples and API documentation to use Signal Fish
  Server's versioned `/v2/ws` endpoint instead of the nonexistent unversioned
  `/ws` or `/signal` paths.
- Fixed player-only commands being queued before join, after leave, or by a
  spectator, where Server 0.7 could silently discard gameplay data. Authority
  baselines and changes are validated before they affect local guards,
  attributable typed room-transition failures roll back their admission fence,
  and both drivers preserve identical error precedence without consuming queue
  capacity.
- Fixed typed room join, leave, reconnect, and spectator responses being
  accepted when no compatible room operation was pending. Unsolicited responses
  and duplicates received after a completed transition now follow the configured
  lifecycle-violation policy; responses mismatched with a pending operation
  remain fenced. Server-authoritative spectator removal, disconnect, and
  room-close exits continue to clear stale membership without a voluntary leave.
- Fixed `MeshController` attempting a room-scoped transport-status update after
  a confirmed room/spectator exit; peer drivers still tear down immediately.

- Fixed the built-in `WebSocketTransport` allowing buffered Ping/Pong floods to
  monopolize one receive poll and leaving EOF or socket errors logically open.
  Control-frame work is now bounded with wake-driven continuation, automatic
  Pong and Close responses still flush before further reads, and terminal
  socket outcomes fuse subsequent receive/send/close operations.

- Fixed `MeshController::start` leaving an `enable_v3()` configuration
  relay-only despite owning a WebRTC driver. Controller startup now preserves
  compatible explicit and future-version choices while adding any missing
  WebRTC/P2P capability. `MeshSession` also ignores peer status for transports
  other than the selected path and clears stale liveness when that path or the
  server-assigned offerer role changes.

- Fixed game-data encoding being treated as one mutable requested/effective
  value. The shared core now resolves the first canonical Server 0.7
  `ProtocolInfo.game_data_formats` atomically, preserves the configured request,
  ignores the earlier unsupported-format advisory for state selection, validates
  binary envelopes and outbound admission against the effective format, and
  refuses fallback binary sends before transport admission. JSON-origin text
  relays remain valid for MessagePack recipients, matching the server
  materializer. Async, polling, and pinned Server 0.7 tests cover supported
  MessagePack and unsupported Rkyv fallback.
- Fixed malformed `Reconnected` baselines being applied under `Observe` and v3
  payloads accepting missing replay/token metadata. Reconnect validation is now
  version-strict and transactional: v3 requires replay status, a nonempty token
  rotated from the submitted credential, exact player stamps, and complete
  matching watermarks; v2 rejects those v3-only fields. Non-replayable nested
  protocol/session messages remain rejected, and stale driver output is fenced
  until the fresh post-reconnect plan.

- Fixed decoded server messages being accepted outside their negotiated
  lifecycle/version phase, malformed authoritative session plans replacing
  valid state, and signals addressing self, unknown, departed, or re-planned
  peers. Both async and polling clients now suppress these lifecycle, plan, and
  signaling frames before state/accountability mutation under every policy,
  validate the Server 0.7 plan cross-field contract, and refuse off-plan
  outbound signaling before anything reaches the wire. Generation-less Server
  0.4 plans remain supported when their shape is otherwise canonical, including
  harmless suppression of unfenceable late signals after a relay re-plan or
  peer departure.
  Connection-scoped `Pong` responses also remain valid while authentication and
  protocol negotiation are still in flight.

- Fixed public `Debug` implementations and built-in transport tracing exposing
  reconnect and relay credentials, TURN userinfo, WebRTC signaling material,
  peer-controlled close reasons, buffered protocol frames, arbitrary game
  payloads, and credential-bearing WebSocket URLs. Ambient diagnostics now
  retain only safe variant, state, and payload-length metadata.
- Fixed the async transport loop allowing an indefinitely pending send to hide
  inbound frames, peer close, and shutdown. Send and receive now make
  bidirectional progress, backend-owned sends finish before close, terminal
  state and events do not wait for graceful close, and close/abort progress is
  bounded even when the event channel is full. A peer close discovered while
  a send is flushing also retains its close metadata instead of being
  misclassified as a generic send failure.
- Fixed async `*_reliable` sends retaining validation decisions made before a
  full command queue drained. Waiting sends now preserve immediate validation
  errors but revalidate connection, negotiation, binary-format, and current
  session-plan state atomically when queue capacity is actually reserved.
  Generation-bearing and legacy generation-less re-plans cannot relabel or
  admit stale signaling, while an idempotent plan reassertion stays valid.
- Fixed the deprecated Emscripten WebSocket transport reclaiming callback
  state after the browser failed to unregister it, which could let a late
  callback access freed memory. Native cleanup now closes after logical
  receive errors, reports deletion failures, retries during teardown, and
  deliberately leaks the small callback allocation if unregistering never
  succeeds.
- Fixed explicit JSON `null` generations being interpreted as legacy omitted
  generations. Omission remains accepted only for Server 0.4 compatibility;
  present generations must decode as UUIDs.
- Fixed protocol-v3 accountability rejecting coalesced or mixed-reason
  `unsupported_format` gap reports emitted by newer Signal Fish servers;
  optional rate-limited advisories now require a prior causal report without
  requiring adjacency. Range validity, non-overlap, and exact counter-delta
  validation remain enforced, and room/spectator exits discard old-room gap
  and advisory authorization.

## [0.10.0] - 2026-07-24

### Added

- Added an opt-in `tls` feature that enables `wss://` for the built-in
  `WebSocketTransport` (rustls with the ring crypto provider and bundled webpki
  roots). Without it, a `wss://` connect now fails cleanly with
  `SignalFishError::Io` rather than the previously documented — but never
  functional — "TLS handled transparently" behavior.
- Added `WebSocketConnectOptions` and `WebSocketTransport::connect_with_options`
  for controlling connection socket tuning — currently whether Nagle's algorithm
  is disabled (`TCP_NODELAY`).

### Fixed

- Fixed the built-in `WebSocketTransport` leaving TCP's Nagle algorithm enabled,
  which added roughly 30-35 ms of latency to the small request/reply messages
  typical of game traffic. `connect` and `connect_with_timeout` now set
  `TCP_NODELAY` by default; opt out with
  `WebSocketConnectOptions::with_disable_nagle(false)`.
- Fixed release preparation failing after successfully verifying and pushing a
  release branch when enterprise policy forbids `GITHUB_TOKEN` from opening
  pull requests; required checks now still dispatch and the successful run
  emits an app-free maintainer PR link and command.
- Fixed release asset generation using Cargo's unsupported `--workspace` flag
  with pinned `cargo-cyclonedx` 0.5.7; the workflow now uses the tool's `--all`
  workspace flag and policy-tests that exact invocation.

## [0.9.0] - 2026-07-18

<!-- semver-checks: major -->

### Added

- Added the lockstep `signal-fish-client-godot` companion crate, which owns
  `GodotWebSocketTransport`, `GodotWebSocketOptions`, and
  `GodotBackpressurePolicy` and supports godot-rust 0.4.5 through 0.5.x.
- Added `PollingClientOptions`, `PollingWorkBudget`, `PollingClosePolicy`,
  `PollingStats`, `SignalFishPollingClient::new_with_options`,
  `polling_stats()`, `transport_diagnostics()`, and the read-only `transport()`
  accessor for bounded per-poll work, explicit flush-on-close, deadline
  handling, and queue/transport observability; defaults are 64 frames/64 KiB
  per direction and `Abandon` on close.
- Added `PollingQueueAgeStats`, `SignalFishPollingClient::queue_age_stats()`,
  and `reset_queue_age_peak()` for sampled current/peak age of the oldest
  client-owned outbound item, complementing queue-depth diagnostics without
  counting frames after backend acceptance.
- Added `GodotWebSocketOptions`, `GodotBackpressurePolicy`,
  `connect_with_options`, and `from_peer_with_options` with adaptive
  latency-targeted admission by default (50 ms, 4 KiB–32 KiB), plus explicit
  fixed-watermark and native-capacity modes. Godot-specific admission
  diagnostics expose invariant violations and empty-buffer single-frame
  escape bytes.
- Added defaulted `Transport` polling-cycle, abort, and diagnostics hooks so
  existing custom transport implementations remain source-compatible, plus
  the public `TransportDiagnostics` snapshot type.
- Added a Godot 4.5 + Fortress Rollback integration guide and a published
  `llms.txt` usage index for discovering current SDK documentation.

### Changed

- Changed release preparation to infer the lockstep version and breaking policy
  from `[Unreleased]`, discover every publishable workspace crate, and dispatch
  required checks with the built-in `GITHUB_TOKEN`; no release App, PAT,
  version input, or crate selector is required.
- **Breaking:** Godot consumers now depend on the lockstep companion adapter and
  import the transport from that crate; the transport-agnostic core keeps its
  Rust 1.87 MSRV while the adapter requires Rust 1.94. The tested production
  Godot 4.5 browser integration now pins godot-rust 0.5.4.
- Changed repository agent guidance to prefer the VS Code GitHub
  connector/extension for hosted operations, then local `git`, with GitHub CLI
  (`gh`) reserved for the final fallback.

### Removed

- **Breaking:** Removed the core crate's `transport-godot` feature, optional
  `godot` dependency, Godot transport module, and crate-root Godot re-exports.
  See the 0.9 migration guide for dependency and import changes.

### Fixed

- Fixed the Emscripten WebSocket transport consuming a queued frame while the
  browser socket was still connecting; pre-open and failed-send frames now
  remain owned by the caller for ordered retry.
- Fixed Godot web throughput being limited to one accepted frame per rendered
  callback by treating successful `WebSocketPeer` submission as ownership
  transfer instead of waiting for browser socket-wide buffering to reach zero;
  capacity refusals now retain frames for ordered retry without loss.
- Fixed polling close deadlines to release built-in WebSocket sockets
  immediately, finish backend-owned sends before a disconnect closes the
  transport, and drain already-buffered inbound frames under bounded close
  progress.

## [0.8.0] - 2026-07-13

<!-- semver-checks: major -->

### Added

- Signal Fish Server 0.4.0 protocol surface: delivery classes and exact gap
  reports, relay statistics, graceful-drain advisories, reconnect replay
  status/watermarks and rotating reconnection tokens.
- `SignalFishConfig::enable_v3()` for v3 relay/accountability without WebRTC,
  plus invalid-state-proof `GameDataDelivery` classified JSON sends.
- Strict protocol-v2/v3 MessagePack binary-envelope decoding and binary game-data
  sends through both async and polling clients.
- `ClientSnapshot`, `ProtocolViolationPolicy`, and typed
  `SignalFishEvent::ProtocolViolation` diagnostics with quarantine as the
  default accountability response.
- `ErrorCode::ServerDraining` and `ErrorCode::InvalidDeliveryClass`.
- `SignalFishError::BinaryFormatNotNegotiated` for binary sends attempted on
  JSON-format connections.
- Object-safe `SignalFishClientApi` for writing synchronous room, signaling,
  capacity, statistics, and snapshot logic that works with either client
  driver.
- `transport-godot` and `GodotWebSocketTransport`, a pure-Rust Godot 4.5
  `WebSocketPeer` path for native builds and official no-thread web exports,
  replacing raw Emscripten WebSocket FFI as the standard Godot integration.
- `ErrorCode::SlowConsumer` (wire `SLOW_CONSUMER`) and
  `ErrorCode::ActivityTimeout` (wire `ACTIVITY_TIMEOUT`), plus
  `SignalFishEvent::DecodeFailed` so unknown or malformed server frames surface
  as typed events instead of being silently discarded.
- `ClientStats::messages_undecodable` and
  `SignalFishEvent::Disconnected.last_server_error` for diagnosing protocol
  drift and attributing disconnects to best-effort server farewell messages.
- Error-code-space conformance tests against the vendored server AsyncAPI spec,
  with weekly drift detection for newly introduced server codes.
- `examples/load_lab.rs`, an env-gated real-server E2E suite, and the Delivery
  Contract & Backpressure guide for measuring relay throughput, liveness, and
  slow-consumer behavior.

### Changed

- **Breaking:** `Transport` is now a frame-capable, object-safe polling trait
  over `TransportFrame::Text` and `TransportFrame::Binary`; it no longer
  requires `Send`. The async client applies `Send + 'static` only at its task
  boundary, while the polling client accepts main-thread-only transports.
- **Breaking:** protocol and event types expose v3 sequence, epoch, delivery,
  reconnect, shutdown, and accountability metadata. Exhaustive matches must
  handle the new variants and fields.
- `WebSocketTransport` now passes binary frames through, retains structured
  close metadata, and explicitly flushes automatic Pong responses.
- Event and snapshot debug formatting no longer prints
  reconnect credentials or arbitrary application payloads.
- **Breaking:** common synchronous commands on `SignalFishClient` now take
  `&mut self`, matching `SignalFishPollingClient` and the shared
  `SignalFishClientApi`. The matching `MeshController` room delegations now
  take `&mut self` and `client_mut()` exposes other mutable commands. Async
  waiting sends remain callable through `&self`.
- The minimum supported Rust version is now 1.87.0, matching `godot` 0.4.5.
- **Breaking:** `ErrorCode` gained `SlowConsumer` and `ActivityTimeout`;
  `SignalFishEvent` gained `DecodeFailed`; `Disconnected` gained
  `last_server_error`; and `ClientStats` gained `messages_undecodable`.
- Graceful `shutdown()` now preempts a transport loop wedged on a full event
  channel on every path and closes the transport cleanly instead of waiting
  for the shutdown timeout to abort the task. The terminal `Disconnected`
  event remains best-effort when the event channel is already full.

### Fixed

- Enforce negotiated JSON-vs-binary frame representation on inbound and
  outbound game data, reject explicit-null negotiation fields, preserve
  accountability baselines transactionally, and clear room tokens/quarantine
  on room or spectator exit.
- Polling disconnect policy now closes the physical transport, pending polling
  closes remain driven across `poll()` calls, peer WebSocket Close responses
  are flushed, Emscripten callback payload handling matches its C ABI, and
  debug builds diagnose wake-driven misuse of its polling-only receive path.
- `supports_mesh()` now requires both local WebRTC advertisement through
  `enable_mesh()` and negotiated protocol v3; relay-only `enable_v3()` clients
  no longer report that mesh is available.
- README no longer overclaims byte-exact wire parity, and
  `GameDataEncoding::Rkyv` documentation now reflects that the server does not
  negotiate rkyv.

### Deprecated

- `EmscriptenWebSocketTransport` is deprecated for standard Godot exports;
  use `GodotWebSocketTransport` instead. It remains supported for custom
  Emscripten hosts in 0.8.

## [0.6.0] - 2026-07-01

### Added

- `SignalFishClient::send_game_data_reliable(data)` — backpressure-aware
  counterpart to `send_game_data` that waits for space in the outgoing command
  queue instead of failing fast, pacing the caller to actual transport
  throughput; the recommended way to stream high-rate payloads such as
  rollback input packets (#47).
- `SignalFishClient::send_signal_reliable(to, signal)` — waiting counterpart
  to `send_signal` for WebRTC signaling (protocol v3 only), so congestion
  never loses an offer/answer/ICE candidate (#47).
- `SignalFishConfig::command_channel_capacity` field (default `1024`, values
  below 1 clamped to 1) and the
  `SignalFishConfig::with_command_channel_capacity(n)` builder for tuning the
  bounded outgoing command queue (#47).
- `SignalFishError::SendBufferFull { capacity }` — returned by the fail-fast
  send methods when the outgoing command queue is full; the message is not
  queued and nothing is silently dropped (#47).
- `send_capacity()` / `max_send_capacity()` on `SignalFishClient` and
  `SignalFishPollingClient` — remaining/configured command-queue capacity for
  send pacing and congestion diagnostics (#47).
- `ClientStats` (re-exported at the crate root) with cumulative
  `game_data_sent` / `game_data_received` counters, returned by `stats()` on
  both clients. The counters survive disconnection and make relay-path loss
  observable: the client itself never drops game data, so a cross-peer
  sent-vs-received deficit points at the relay path or a peer, not at this
  client (#47).
- Optional low-latency mesh pump: `WebRtcDriver::set_ready_waker` (default no-op)
  hands the driver a `MeshWaker` it can `wake()` when it has output ready, so
  trickled ICE candidates and inbound data surface immediately instead of waiting
  up to one `MeshController` pump interval. Entirely optional to implement and
  available with the `mesh` + `tokio-runtime` features.
- Comprehensive v2/v3 user documentation: new `docs/protocol-versioning.md` and
  `docs/mesh-guide.md` guides, expanded protocol/events/errors/concepts/examples
  pages, a v3 walkthrough of `examples/mesh_session.rs`, and consistent
  "Protocol v3 only" rustdoc notes across the v3 API.

### Changed

- Minimum supported `tokio` is now declared as `1.21` (previously `1`): the
  new queue-capacity diagnostics use `mpsc::Sender::capacity` (tokio 1.5)
  and `mpsc::Sender::max_capacity` (tokio 1.21). Any lockfile from the last
  few years already satisfies this; the requirement is now honest (#47).
- **Breaking (behavior): events are never dropped.** The transport loop now
  delivers every event with `send().await`; a lagging consumer pauses the
  loop and backpressure propagates to the server instead of losing events.
  Previously a full event channel dropped events with a warning (only
  `Disconnected` was blocking). `SignalFishConfig::event_channel_capacity`
  (default 256) now only controls how much buffering the consumer gets before
  backpressure kicks in, not loss (#47).
- **Breaking (API): the async client's command channel is now bounded.** All
  synchronous send methods (`join_room`, `send_game_data`, `send_signal`,
  `ping`, …) fail fast with the new `SendBufferFull` error variant when the
  queue is full. Adding a variant to `SignalFishError` is breaking for
  exhaustive matches (#47).
- `SignalFishPollingClient` applies the same bound to its command queue
  (via `command_channel_capacity`); queuing methods return `SendBufferFull`
  once a stalled transport fills it (#47).
- `MeshController` no longer drops a driver signal the command queue refuses:
  the signal is buffered in the controller and retried (in order, ahead of
  further driver output) until the queue accepts it — or discarded if the
  connection ends or the target peer's handshake is torn down first (a
  stale signal is never relayed) — and `recv()` is
  documented cancel-safe — a buffered signal survives cancellation. Refused
  transport-status reports are now debug-logged instead of silently
  discarded (#47).
- Documented the runtime driving contract: `SignalFishClient::start` spawns
  the transport loop with `tokio::spawn` and works on any *driven* runtime
  (including `current_thread`), but manually "ticking" a runtime starves it —
  frame-driven or wasm environments should use `SignalFishPollingClient`
  (feature `polling-client`) (#47).
- `MeshSession` and `MeshController` now defensively replay any mesh events a
  server batches into a reconnect's `missed_events` (in addition to handling a
  re-sent live `SessionPlan`), so a mesh session is rebuilt correctly after a
  reconnect regardless of which strategy the server uses. The fold is idempotent.

### Fixed

- `examples/basic_lobby.rs` now bases reconnect start decisions on the
  authoritative reconnect snapshot while using missed events only to detect that
  the game already started or finalized.
- Documentation validation scripts now avoid Python 3.10-only annotation forms,
  keeping the pre-commit/docs checks importable on Python 3.9 environments.
- `MeshController` now restarts a peer's handshake when the server *reassigns*
  its offerer role across a re-plan — a host re-election or topology change that
  flips the peer's `initiate`/`you_initiate`. Previously a surviving peer kept
  the driver in its stale offerer role, which could cause WebRTC glare (both
  peers offer) or a stuck handshake (both wait); a survivor whose role is
  unchanged still keeps its live connection.
- `MeshController` now reports `TransportStatus(WebRtc, false)` on the final
  channel-down edge when leaving a room or disconnecting with a live data
  channel; previously the `RoomLeft`/`Disconnected` teardown cleared its
  connected-peer set directly and skipped that report (the per-peer `PlayerLeft`
  path already reported it).
- `MeshSession::apply` no longer reports a spurious change when re-applying an
  ICE pre-gather set identical to the one already held.

## [0.5.0] - 2026-06-20

### Added

- Protocol v2: explicit `ClientMessage::StartGame` to begin a game once players
  are ready (`SignalFishClient::start_game()` / `SignalFishPollingClient::start_game()`),
  plus error codes `GameStartNotReady` (`GAME_START_NOT_READY`) and
  `GameStartForbidden` (`GAME_START_FORBIDDEN`).
- Protocol v3 (additive, backward-compatible "relay floor"): new wire types
  `Topology`, `TransportKind`, `IceServer`, `SessionPeer`, `SessionPlanPayload`,
  and the externally-tagged, matchbox-compatible `PeerSignal`
  (`Offer`/`Answer`/`IceCandidate`).
- New client messages `Signal` and `TransportStatus`, and new server messages
  `Signal`, `NewPeer`, `SessionPlan`, and `PeerTransportStatus`, surfaced as the
  corresponding `SignalFishEvent` variants.
- Six v3 error codes: `CROSS_ROOM_SIGNAL`, `UNSUPPORTED_TRANSPORT`,
  `SIGNAL_TARGET_NOT_FOUND`, `SIGNAL_RATE_LIMITED`, `SIGNAL_TOO_LARGE`, and
  `CONNECTION_IDLE_TIMEOUT`.
- `SignalFishConfig::enable_mesh()` (one-liner mesh opt-in) plus
  `with_protocol_version`/`with_transports`/`with_topologies`. `Authenticate`
  gains optional `protocol_version`/`supported_transports`/`supported_topologies`
  (omitted from the wire by default, so v2 bytes are unchanged); `ProtocolInfo`
  gains negotiated version fields; `RoomJoined`/`Reconnected` gain optional
  `ice_servers` (ICE pre-gather).
- Mesh client API: `send_signal`/`send_offer`/`send_answer`/`send_ice_candidate`/
  `send_raw_signal`, `report_transport_status`, `negotiated_protocol_version()`,
  and `supports_mesh()` on both clients; a fail-fast
  `SignalFishError::ProtocolUnsupported` guard for v3 sends before negotiation.
- `mesh` feature: `MeshSession` (zero-dependency v3 state tracker) and the
  batteries-included `WebRtcDriver` seam + `MeshController` that drives the whole
  signaling handshake against a consumer's WebRTC backend, with a runnable
  `examples/mesh_session.rs`.
- Golden-wire conformance: vendored server protocol samples
  (`tests/wire-samples/`) with semantic round-trip tests
  (`tests/wire_golden_tests.rs`, compared as `serde_json::Value` so key
  order / whitespace are ignored) and a scheduled drift workflow
  (`.github/workflows/protocol-sync.yml`). The default relay path is verified
  byte-identical to v2.

### Fixed

- Dev container: removed Unix-only host initialization and required host-home
  credential bind mounts (`~/.ssh`, `~/.gitconfig`, `~/.gnupg`) so VS Code can
  open the devcontainer reliably across Windows, macOS, Linux, WSL, Codespaces,
  and remote Docker hosts. Previously the container could fail before startup
  when `initializeCommand` ran through Windows `cmd.exe`, when `HOME` was unset,
  or when host credential paths were missing/not shared with Docker.

### Changed

- **Game start is now explicit (migration).** The game no longer auto-starts when
  all players are ready — the authority (or any member, if the room has no
  authority) must call `start_game()`. Non-authority callers in an authority room
  receive `GameStartForbidden`; calling before everyone is ready yields
  `GameStartNotReady`.
- **Relay users are unaffected.** Clients that do not call `enable_mesh()` see no
  wire-format or behavioral change — the relay path is byte-identical to v2. Mesh
  signaling is strictly opt-in.
- Adding variants to the public `ClientMessage`, `ServerMessage`,
  `SignalFishEvent`, `ErrorCode`, and `SignalFishError` enums is breaking under
  semver, so this is a MINOR (`0.4.1` → `0.5.0`) bump for a 0.x crate.
- Dependabot: corrected `open-pull-requests-limit` from 2 to 1 for both the
  `cargo` and `github-actions` ecosystems, aligning the config value with the
  documented "single consolidated batch PR" intent. Updated header comment from
  "area PRs" to "ecosystem-based PRs" for clarity.
- CI policy tests: added three new tests in `ci_config_tests.rs` to enforce
  Dependabot structural invariants — each ecosystem must set
  `open-pull-requests-limit: 1`, all ecosystem limits must be consistent, and
  every ecosystem must declare a wildcard catchall group.

## [0.4.1] - 2026-03-15

### Changed

- Updated CI `lycheeverse/lychee-action` to v2.8.0 (lychee v0.23.0); migrated `.lychee.toml` `header` field from array-of-strings to TOML inline-table format to match the new lychee config schema.
- Removed `tokio-test` as a dev-dependency.

## [0.4.0] - 2026-03-04

### Added

- `transport-websocket-emscripten` feature flag with `EmscriptenWebSocketTransport` — a `Transport` implementation using raw FFI to Emscripten's `<emscripten/websocket.h>` C API for `wasm32-unknown-emscripten` targets. Automatically enables the `polling-client` feature.
- `polling-client` feature flag with `SignalFishPollingClient` — a synchronous, polling-based client for environments without an async runtime (e.g., game loops, single-threaded WASM).
- `tokio-runtime` feature flag for explicit opt-in to the Tokio runtime (`tokio/rt`, `tokio/time`), automatically enabled by `transport-websocket`.

## [0.3.1] - 2026-02-23

### Fixed

- WASM target dependency configuration now enables `uuid` features `v4`, `serde`, and `js` together for `wasm32`, ensuring UUID generation and serialization support remain available when compiling for WebAssembly.

## [0.3.0] - 2026-02-23

### Added

- `SignalFishConfig::event_channel_capacity` field (default `256`) for tuning the bounded event channel size.
- `SignalFishConfig::shutdown_timeout` field (default `1 second`) for controlling graceful-shutdown wait time.
- `SignalFishConfig::with_event_channel_capacity(n)` builder method.
- `SignalFishConfig::with_shutdown_timeout(d)` builder method.

### Changed

- `SignalFishConfig::with_event_channel_capacity` now clamps values below `1` to `1`, so the stored config value matches documented behavior.

### Fixed

- `SignalFishClient::shutdown` now aborts the background transport task if graceful shutdown exceeds `shutdown_timeout`, preventing detached tasks from running indefinitely.
- `SignalFishClient::shutdown` and disconnect handling now always clear `authenticated`, `player_id`, `room_id`, and `room_code`, preventing stale state when shutdown times out or the transport task is aborted before `Disconnected` is emitted.

## [0.2.2]

### Added

- `Transport` trait — async, cancel-safe, transport-agnostic abstraction (`send`, `recv`, `close`)
- `WebSocketTransport` — built-in WebSocket transport via `tokio-tungstenite`, feature-gated under `transport-websocket` (default)
- `ClientMessage` enum with 11 variants (Authenticate, JoinRoom, LeaveRoom, PlayerReady, AuthorityRequest, ProvideConnectionInfo, Reconnect, JoinAsSpectator, LeaveSpectator, GameData, Ping)
- `ServerMessage` enum with 24 variants covering authentication, room lifecycle, player management, authority, spectator, relay, game data, and heartbeat flows
- `SignalFishEvent` enum with 26 variants (24 server message events + 2 synthetic: Connected, Disconnected)
- `SignalFishError` enum with 9 variants (TransportSend, TransportReceive, TransportClosed, Serialization, NotConnected, NotInRoom, ServerError, Timeout, Io)
- `ErrorCode` enum with 40 server error code variants for precise programmatic error handling
- `SignalFishClient` — async client handle with background transport loop
  - `start(transport, config)` spawns the transport loop and returns `(client, event_rx)`
  - Room operations: `join_room`, `leave_room`, `set_ready`, `request_authority`, `provide_connection_info`
  - Spectator operations: `join_as_spectator`, `leave_spectator`
  - Data operations: `send_game_data`
  - Reconnection: `reconnect`
  - Lifecycle: `shutdown`, `ping`
  - State accessors: `is_connected`, `is_authenticated`, `current_room_id`, `current_player_id`, `current_room_code`
- `SignalFishConfig` — client configuration with `new(app_id)` constructor
- `JoinRoomParams` — builder pattern with `new(game_name, player_name)`, `.with_max_players()`, `.with_room_code()`, `.with_supports_authority()`, `.with_relay_transport()`
- Protocol types: `PlayerId`, `RoomId`, `PlayerInfo`, `ConnectionInfo`, `RelayTransport`, `LobbyState`, `PeerConnectionInfo`, `SpectatorInfo`, `RateLimitInfo`, `ProtocolInfoPayload`, `GameDataEncoding`
- 200 tests covering protocol serialization, event mapping, client API, error handling, and transport
- `basic_lobby` example — full WebSocket lifecycle with Ctrl+C support
- `custom_transport` example — channel-based loopback transport implementation
- Comprehensive README with quick start, architecture overview, feature flags, and custom transport guide
- `deny.toml` for dependency auditing
- MIT license

### Changed

- **API change:** `SignalFishError::ServerError.error_code` now uses `Option<ErrorCode>` instead of `Option<String>`.
- **Migration guidance:** update pattern matches and handling to account for missing server codes:
  - Before: `SignalFishError::ServerError { message, error_code }` where `error_code` is `Option<String>`
  - After: `SignalFishError::ServerError { message, error_code }` where `error_code` is `Option<ErrorCode>`
  - Recommended handling: `match error_code { Some(code) => ..., None => ... }`

[Unreleased]: https://github.com/Ambiguous-Interactive/signal-fish-client-rust/compare/v0.12.0...HEAD
[0.12.0]: https://github.com/Ambiguous-Interactive/signal-fish-client-rust/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/Ambiguous-Interactive/signal-fish-client-rust/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/Ambiguous-Interactive/signal-fish-client-rust/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/Ambiguous-Interactive/signal-fish-client-rust/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/Ambiguous-Interactive/signal-fish-client-rust/compare/v0.6.0...v0.8.0
