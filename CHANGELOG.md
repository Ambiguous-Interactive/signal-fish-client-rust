# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Added `PollingClientOptions`, `PollingWorkBudget`, `PollingClosePolicy`,
  `PollingStats`, `SignalFishPollingClient::new_with_options`,
  `polling_stats()`, and `transport_diagnostics()` for bounded per-poll work,
  explicit flush-on-close, deadline handling, and queue/transport observability,
  including current and peak oldest queued-command age;
  defaults are 64 frames/64 KiB per direction and `Abandon` on close.
- Added `GodotWebSocketOptions`, `GodotBackpressurePolicy`,
  `connect_with_options`, and `from_peer_with_options` with fixed 32 KiB,
  adaptive latency-targeted, and native-capacity admission modes.
- Added defaulted `Transport` polling-cycle, abort, and diagnostics hooks so
  existing custom transport implementations remain source-compatible, plus
  the public `TransportDiagnostics` snapshot type.

### Fixed

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

[Unreleased]: https://github.com/Ambiguous-Interactive/signal-fish-client-rust/compare/v0.8.0...HEAD
[0.8.0]: https://github.com/Ambiguous-Interactive/signal-fish-client-rust/compare/v0.6.0...v0.8.0
