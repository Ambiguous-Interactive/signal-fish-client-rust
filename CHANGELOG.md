# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
  (`tests/wire-samples/`) with byte-exact round-trip tests
  (`tests/wire_golden_tests.rs`) and a scheduled drift workflow
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
