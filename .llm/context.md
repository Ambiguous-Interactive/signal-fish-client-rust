# Signal Fish Client SDK — Project Context

## Identity

- **Company:** Ambiguous Interactive
- **Crates:** `signal-fish-client` (core) and `signal-fish-client-godot` (adapter)
- **Version:** 0.10.0 lockstep across both crates
- **Edition:** 2021
- **MSRV:** Rust 1.87.0 for core; Rust 1.94.0 for the Godot adapter
- **License:** MIT
- **Repository:** <https://github.com/Ambiguous-Interactive/signal-fish-client-rust>
- **Guide/API:** <https://Ambiguous-Interactive.github.io/signal-fish-client-rust/> · <https://docs.rs/signal-fish-client>

## Purpose

Framed-transport-agnostic client over one complete, ordered text/binary frame stream bound to the intended server; raw stream/datagram framing and trust policy remain outside core.

## Mandatory Workflow

```shell
cargo fmt && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features
```

Run this before every commit.

## Release Automation

Use the manual **Prepare Release** and **Release** workflows. They prepare,
reproduce, attest, and publish every crates.io-publishable workspace member at
the single `[workspace.package].version`. `scripts/release.py workspace-plan`
discovers members with Cargo metadata and orders internal dependencies; Release
has no version input and resumes only checksum-identical partial publication.
Prepare Release derives its bump and breaking policy from `[Unreleased]`; it accepts no version/bump/breaking/crate-selection input beyond a reversible dry-run switch.
Release jobs pin Rust 1.96.1 and Ubuntu 24.04; crate archives contain library source, required unit-test data, manifest, license, and readme.
Repository-only material stays in the linked source repository; see `skills/release-recovery/SKILL.md` and `docs/releasing.md`.

Every blocking workflow runs on PR and default-branch SHAs and ends in a
uniquely named aggregate `Required` job. The names and desired rules live in
`.github/required-checks.json`; the scheduled Repository Policy workflow audits
GitHub for visible drift with its authenticated built-in `GITHUB_TOKEN`.
GitHub hides ruleset bypass actors from workflow tokens, so maintainers verify an empty bypass list in the ruleset UI. Prepare Release uses only `GITHUB_TOKEN` and dispatches required workflows explicitly on its generated branch because token-created pull-request events are suppressed; path filters must not suppress a configured gate. No checked-in `GITHUB_TOKEN` workflow may auto-merge Dependabot PRs: such merges rely on drifting rules and can suppress push CI on the resulting SHA, so the fail-closed policy rejects Dependabot-specific workflows, automated-merge primitives, and write permissions outside the release/docs allowlist. Ruleset #14801090 live-enforces reviewed merges and all required checks.

## GitHub Tool Order

For every GitHub operation, follow
`skills/github-operations/SKILL.md`: prefer the VS Code GitHub
connector/extension first, use local `git` second, and use GitHub CLI (`gh`)
only as the final fallback. Missing `gh` authentication does not block a
connector- or `git`-capable workflow.

## CI/CD Action Reference Policy

Use `owner/action@vN.N.N` (preferred) or `@vN`, not commit hashes. Exceptions:
`dtolnay/rust-toolchain@stable|nightly|beta` and `mymindstorm/setup-emsdk@vN`.

## Changelog Policy

Only add `CHANGELOG.md` entries for user-visible changes.

- Include: public API, behavior, protocol, feature flags, error-model, MSRV/dependency changes that affect consumers, and contributor-facing environment fixes that unblock using the repository.
- Exclude: internal-only updates such as CI/script/pre-commit automation, refactors, tests, and non-behavioral maintenance.

## Safety Analysis Policy

Production Rust is safe by default. The core manifest denies `unsafe_code`, the
Godot adapter forbids it, and the target-gated Emscripten WebSocket module is
the sole documented exception for the platform C API. Required WASM policy and
50 checker self-tests enforce close-before-delete callback-state ownership;
terminal deletion failure intentionally leaks the small allocation instead of
risking late-callback use-after-free. Change-scoped, scheduled, and manual
Deep Safety runs Miri protocol tests, ThreadSanitizer, three raw-byte
JSON/MessagePack fuzz targets, and focused mutation testing; required Clippy
and WASM gates enforce compiler safety on every PR. Every member denies
`unwrap_used` through `indexing_slicing`/`unreachable`, plus `arithmetic_side_effects`; the No
Panics grep gate scans all members; release builds enable `overflow-checks` (debug-equivalent arithmetic).

Native ASan is deferred because owned unsafe is Emscripten-only; fuzz sanitizer
instrumentation covers the parsers it drives. Blanket Clippy
pedantic/nursery/cargo groups remain deferred after a noisy inventory; add them
only for stable, actionable defects.

Dependabot uses one root-workspace updater; minimum/latest Godot fixtures stay standalone because incompatible godot-rust versions would break the workspace (hosted run 31969079956 proved clean root discovery).

## Architecture — Core Modules

| File | Purpose |
|------|---------|
| `src/transport.rs` | Object-safe polling `Transport` trait over text/binary `TransportFrame`s |
| `src/protocol.rs` | Wire-compatible protocol types, including v3 delivery/accountability and mesh |
| `src/protocol/binary.rs` | Strict physical MessagePack envelope decoders for v2/v3 binary game data |
| `src/accountability.rs` | Server-0.4.0-derived delivery-accountability state machine |
| `src/signal.rs` | `PeerSignal` — typed, matchbox-compatible WebRTC signal (protocol v3) |
| `src/error_codes.rs` | `ErrorCode` enum — 54 variants from server (48 in the post-0.7 authority, 6 compatibility-only) |
| `src/error.rs` | `SignalFishError` error type |
| `src/event.rs` | `SignalFishEvent` high-level event stream |
| `src/client_core.rs` | Shared command construction, decoding, accountability, state, events, and statistics |
| `src/client_api.rs` | Object-safe `SignalFishClientApi` common synchronous surface |
| `src/client.rs` | Thin async driver + `SignalFishConfig` + `JoinRoomParams` |
| `src/polling_client.rs` | Thin caller-driven polling transport driver (feature: `polling-client`) |
| `src/mesh.rs` | `MeshSession` v3 state tracker (feature: `mesh`) |
| `src/webrtc.rs` | `WebRtcDriver` seam + `MeshController` (feature: `mesh`) |
| `src/transports/websocket.rs` | WebSocket transport (feature: `transport-websocket`) |
| `src/token_binding.rs` | Native WebSocket token-binding-v2 types, validation, canonicalization, and proof state (feature: `token-binding`) |
| `crates/signal-fish-client-godot/src/lib.rs` | Godot 4.5 native/web `WebSocketPeer` adapter and its 39 fake-backend tests |

### Transport Trait

The object-safe surface is pinned verbatim in
`skills/transport-abstraction/SKILL.md`. Required methods are `poll_send`,
`poll_recv`, `poll_close`, and `abort`; `begin_poll_cycle`, `diagnostics`,
`is_ready`, and `close_info` have defaults.

The trait has no `Send` bound; `SignalFishClient::start` separately requires `Transport + Send + 'static`. Its boundary is one complete, ordered text/binary frame stream for one intended server, not raw bytes, datagrams, or server authentication.
A backend owns framing, trust/source binding, and loss/duplicate/reorder policy. The SDK owns no UDP envelope; Server 0.7 ignores `RelayTransport::Udp` join metadata, `ConnectionInfo::Relay` is self-declared, and WebRTC drivers yield assembled messages after owning ICE/DTLS/SCTP/UDP.
`poll_send` may take a frame only at backend ownership transfer; that increments `game_data_sent` even if completion later fails, but never proves peer delivery.
`Pending` before acceptance leaves the frame intact. Async readiness changes wake registered I/O; both drivers map the first `is_ready()` observation to `transport_ready` and `Connected`. Close is
idempotent; on error, logical I/O terminates and fallible cleanup remains safe for the abort fallback. `abort` is a required,
prompt, nonblocking, non-panicking, idempotent fallback that releases or safely detaches backend resources,
discards retained sends, and ends driver polling; completed cleanup is not repeated, while failed cleanup may retry safely. See `skills/transport-abstraction/SKILL.md`.
`begin_poll_cycle` marks one driver scheduling cycle: once per public polling-client `poll()` call, or once per async transport-loop iteration. It is not necessarily a rendered frame or fixed wall-clock interval.

The async driver polls retained sends and receives with one runtime waker, so
outbound backpressure cannot hide inbound work, peer close, or shutdown.
Graceful termination finishes transport-owned sends before close under one
deadline; expiry aborts. After terminal send failure both drivers freeze command
admission and boundedly process immediately ready frames through `ClientCore` until
`Pending`, terminal input, work bound, protocol stop, or deadline (the polling driver clamps this drain to
its caller-configured receive budget). A ready farewell precedes `Disconnected`; only peer-close metadata replaces the send cause; native WebSocket retains buffered read state.

Public `Debug`/tracing form an ambient-log boundary: reconnect/relay/TURN credentials, WebRTC SDP/ICE, arbitrary application data, buffered protocol frames, peer close reasons, and URL userinfo/query values are never formatted; safe diagnostics expose only variants, state flags, identifiers where appropriate, and byte lengths. Wire serialization and explicit public-field access remain unchanged.

The Emscripten transport reports `Pending` while its browser WebSocket is still
connecting and must not call `emscripten_websocket_send_*` or consume the
caller's frame until `onopen`. Preparation or FFI send errors likewise leave
the exact frame available to its caller.

Native token binding belongs exclusively to `WebSocketTransport`. Disabled is
the byte/dependency-compatible default; optional retries without the offer only
after tungstenite's exact successful-upgrade/no-selection result; required
fails closed. A selected connection consumes and validates the first challenge
before either client sees a ready transport, derives HKDF-SHA-256 from the exact
RFC 6455 key plus server nonce, zeroizes retained key material, and protects all
outbound JSON/binary frames with one sequence. Sequence commits only at backend
ownership transfer. Browser, Emscripten, and Godot APIs cannot expose the
handshake key; `from_stream` is likewise post-handshake and cannot opt in.
Required Server 0.7 profiles require WSS. Custom rustls token-binding offers
wrap the resolver and disable resumption only on a clone, including Optional
fallback; a compatible X.509 signer binds proofs to its selected leaf's
lowercase SHA-256-of-DER fingerprint without a caller claim. This supports
strict Server 0.7; browser, Emscripten, Godot, and post-handshake transports remain incapable.
The v2 proof is client-to-server only and binds content/order to one physical
handshake; it is not confidentiality or server authentication. On `ws://`, an
on-path observer sees the key and nonce and can forge it. Every reconnect gets a
fresh nonce, derived key, and sequence space; replay/reordering/tamper fails.

The lockstep `signal-fish-client-godot` adapter defaults to adaptive outbound admission: a 50 ms latency target with a
4 KiB floor, 32 KiB ceiling, and a further native-capacity clamp. A successful Godot send
transfers ownership immediately; browser buffering is observed separately.
SDK-created Godot peers set an 8 MiB inbound buffer and raise the independent queued-packet cap from 4,096 to 65,536 before connecting; the byte storage may reserve roughly 16 MiB in Godot, plus packet metadata. Godot's native and web backends can silently drop newly arriving frames when either inbound limit fills, so enough unusually small frames can still reach the finite packet cap first; `from_peer` preserves caller settings. Outbound keeps Godot's legacy 65,535-byte default: a single frame over that size on native (at or above it on web) parks as `Pending`, growing only capacity diagnostics. The
blocking workflow covers official native/web Godot 4.5, requires a valid frame
over the legacy 65,535-byte default, and runs clean, seeded-netem impaired, and
3,600-frame soak jobs on Server 0.7 plus a clean Server 0.4 gate. It checksum-verifies and builds iproute2
6.6.0 for seeded netem rather
than relying on the runner's older `tc`. A 20-frame Fortress prediction window
leaves recovery headroom. Simulated frames 1 through 60 form an explicit
renderer/JIT warm-up phase bounded by that window; steady-state and final
confirmation lag are capped at eight frames clean or 13 frames impaired/soak.
The fixture uses a
peer-independent fixed 18 Hz simulation cadence that preserves elapsed
deadline debt and catches up by at most one frame per rendered callback, plus
a one-time proposal/ack/commit startup barrier that maps a shared same-host
wall-clock deadline to each browser's monotonic clock, preventing process-launch
order from becoming frame advantage. A bounded causal relay hold and the polling-hitch
oracle then require rollback and forward gameplay progress. These controls must prove
rollback/resimulation, bounded confirmation lag with zero stalls (advisory
frame-advantage wait recommendations are observed but not required to be zero —
they fire inside the lag bound and the fixed cadence never acts on them), exact
state checksum convergence, drained queue age/depth with a non-positive final
eight-sample soak age slope, relay/server conservation, and v3 peer departure.

Delivery accountability accepts coalesced, mixed-reason
`unsupported_format` gap ranges emitted by newer servers. Their inclusive
sequence counts must still exactly match cumulative counter deltas, ranges may
not overlap, and an optional rate-limited supplemental advisory is accepted
only after an actual unsupported-format range, without requiring adjacency.
Authoritative room and spectator exits discard all room-scoped gap, sender,
advisory-authorization, and quarantine state while retaining connection-wide
counters.

### Client Usage Pattern

Connect a transport, construct `SignalFishConfig`, and pass both to `SignalFishClient::start`, which returns the handle and event receiver and
queues `Authenticate`. Wait for `Authenticated` before room commands; drain
events continuously and call `shutdown().await` for graceful teardown. The
complete compiling example is `examples/basic_lobby.rs`.

### SignalFishConfig

Required second argument to `SignalFishClient::start`. Only `app_id` is required.
Opt into protocol v3 relay/accountability with `.enable_v3()`. Use
`.enable_mesh()` only with a WebRTC driver. `MeshController::start` preserves
compatible choices while ensuring v3, WebRTC, and a Host or Mesh topology.
Once `recv` observes signaling end, or on shutdown, the controller clears its
view, disconnects peers, fuses receive, and prevents later driver sends.

```rust,ignore
pub struct SignalFishConfig {
    pub app_id: String,
    pub sdk_version: Option<String>,          // defaults to crate version
    pub platform: Option<String>,             // e.g. "unity", "godot", "rust"
    pub game_data_format: Option<GameDataEncoding>,
    pub event_channel_capacity: usize,        // defaults to 256 (buffer before backpressure)
    pub command_channel_capacity: usize,      // defaults to 1024 (bounded send queue)
    pub shutdown_timeout: std::time::Duration, // async shutdown / polling close deadline; 1s
    pub protocol_violation_policy: ProtocolViolationPolicy, // Quarantine
}

let config = SignalFishConfig::new("mb_app_abc123")
    .with_event_channel_capacity(512)
    .with_command_channel_capacity(2048)
    .with_shutdown_timeout(std::time::Duration::from_secs(5));
```

### JoinRoomParams

Builder for `client.join_room(...)`.

```rust,ignore
let params = JoinRoomParams::new("my-game", "Alice")
    .with_room_code("ABC123")   // omit for quick-match
    .with_max_players(4)
    .with_supports_authority(true);
client.join_room(params)?;
```

### Key Client Methods

All methods except `shutdown` and the `*_reliable` sends are synchronous (they queue a message on the bounded command channel, no round-trip):

Common synchronous commands take `&mut self` through the object-safe
`SignalFishClientApi`. Driver-specific lifecycle stays concrete; both drivers
delegate protocol behavior and state to one `ClientCore`.

```rust,ignore
client.join_room(params: JoinRoomParams) -> Result<()>
client.leave_room() -> Result<()>
client.send_game_data(data: serde_json::Value) -> Result<()>
client.send_game_data_reliable(data).await   // waits for queue space (pacing)
client.send_game_data_with_delivery(data, GameDataDelivery::Latest { key: 7 })
client.send_binary_game_data(payload: Vec<u8>) -> Result<()> // v3 physical binary frame
client.set_ready() -> Result<()>
client.start_game() -> Result<()>           // protocol v2: explicit game start
client.request_authority(become_authority: bool) -> Result<()>
client.provide_connection_info(info: ConnectionInfo) -> Result<()>
client.reconnect(player_id, room_id, auth_token) -> Result<()>
client.join_as_spectator(game_name, room_code, spectator_name) -> Result<()>
client.leave_spectator() -> Result<()>
client.ping() -> Result<()>
client.send_signal_reliable(to, signal).await // v3 only; waiting send_signal
client.send_capacity() / client.max_send_capacity() -> usize // queue diagnostics
client.stats() -> ClientStats  // accepted-send / decoded-receipt counters
client.snapshot() -> ClientSnapshot // coherent readiness/role/session view
client.shutdown().await      // async, graceful
```

WebRTC signaling also requires an authoritative `SessionPlan`. Server 0.7
plans/signals carry a UUID generation; the core stamps outgoing signals, suppresses stale/unknown inbound
generations plus departed/re-planned-out peers' late same-generation signals (benign races), rejects
self/off-plan/unknown senders, and snapshots the generation plus selected topology/transport atomically.
Accepted finalized-room, current-roster plans use one of four Server 0.7 topology/transport pairs and replace peer authority atomically; the most recent eight superseded non-null generations stay fenced against replay before authoritative plan fields, peers, or mesh revision change (bounded memory under generation churn; older replays degrade to fresh authoritative plans).
Current-generation duplicates and generation-less Server 0.4 plans remain valid, while authoritative room/session teardown clears replay history.
`supports_mesh()` reports negotiated WebRTC + Host/Mesh capability, not the
selected plan. `MeshController` rebuilds retained pairs across generations or
offerer-role changes. `MeshSession` accepts liveness only for the selected transport.

Membership pairs `room_role` with room/participant IDs. Commands validate connection, authentication (directed room
operations refuse with `NotAuthenticated` before the server confirms it), transition, role, authority, protocol/session, then queue capacity.
Admitted joins/leaves/reconnects fence later work until a matching typed terminal response; generic errors/absence stay fenced until teardown.
A v3-capable config requests `room_operation_ids`; after an exact echo, the core records one fresh UUID at successful queue admission for all five room operations, and only the matching ID/result kind releases it.
Pre-echo admissions and missing-capability servers stay legacy for that operation's lifetime. Terminal responses without a compatible pending operation violate lifecycle, except that an authoritative spectator removal, disconnect, or room-close may overtake an already-admitted voluntary leave: exactly one matching late reply for the prior room is absorbed without disturbing a newly admitted join, while wrong-room, duplicate, and unrelated results still violate. Authoritative spectator exits need no voluntary leave. Events are never dropped: a full event channel backpressures; undecodable frames surface as `DecodeFailed`;
events are missed only on receiver/handle drop or preempted delivery — `shutdown`, or an async terminal disconnect facing a
non-draining consumer, abandons at most the one in-flight delivery (remaining batch events get one nonblocking attempt) after
`shutdown_timeout` and terminates the loop so parked reliable senders resolve; the polling driver emits synchronously from `poll()`.
Snapshots distinguish nonterminal `connected`, observed `transport_ready`, server-confirmed `authenticated`, and authoritative room membership.
Decoded game-data receipt counts before suppression; counters are diagnostic boundaries, not cross-peer delivery equality.
`SignalFishPollingClient` shares the classified/binary sends, queue bound,
capacity accessors, `stats()`, and coherent `snapshot()`. Its default per-poll
work budget is 64 frames/64 KiB in each direction, and its default close policy
abandons client-owned queued work. Flush-on-close is an explicit opt-in;
adaptive outbound admission lives on the Godot transport options, not the
polling client. Use `polling_stats()` for scheduling/queue diagnostics and
`queue_age_stats()` for sampled current/peak age of client-owned work; reset the
age peak after authentication/setup when measuring gameplay. Backend acceptance
ends queue age but is not peer delivery. Use `transport_diagnostics()` for
backend buffering/admission diagnostics.
Use the polling client's read-only `transport()` accessor for Godot's zero-expected
`admission_watermark_violations()` and separately accounted
`one_frame_escape_frames()` / `one_frame_escape_bytes()` diagnostics.

### Performance Contract

The opt-in `tools/perf-lab` drives 28 deterministic polling-client workloads;
pinned CI gates their protocol ledgers and allocation counters. Direct JSON
strings at least 4 KiB use capacity-aware serialization. See its README.

## Feature Flags

| Flag | Default | Description |
|------|---------|-------------|
| `transport-websocket` | on | Built-in WebSocket via `tokio-tungstenite` |
| `token-binding` | off | Native `signalfish.tokenbinding.v2` negotiation and outbound proofs; enables `transport-websocket` |
| `tls` | off | `wss://` TLS for the built-in WebSocket transport (rustls + ring provider + webpki roots) |
| `transport-websocket-emscripten` | off | Emscripten WebSocket transport; enables `polling-client` |
| `polling-client` | off | `SignalFishPollingClient` — sync, polling-based client for any `Transport` |
| `tokio-runtime` | off (on via `transport-websocket`) | Tokio `rt` + `time` features |
| `mesh` | off | Protocol v3 mesh: `MeshSession` tracker + `WebRtcDriver` seam + `MeshController` |

## Dependencies

| Crate | Purpose |
|-------|---------|
| `tokio` | Async runtime (sync, macros, rt, time features) |
| `serde` + `serde_json` + `serde_bytes` | JSON serialization of protocol messages |
| `rmp` + `rmp-serde` | Strict protocol-v3 MessagePack envelope decoding |
| `uuid` | Player/room IDs serialized as lowercase hyphenated wire strings |
| `thiserror` | Derive macro for `SignalFishError` |
| `tracing` | Structured logging and diagnostics |
| `tokio-tungstenite` | WebSocket transport (optional) |
| `futures-util` | Stream/sink utilities for WebSocket (optional) |
| `base64` + `hkdf` + `hmac` + `sha2` + `zeroize` | Opt-in token-binding-v2 derivation, proofs, and secret lifetime |

The core manifest must never depend on `godot` or expose godot-rust types.
`signal-fish-client-godot` depends exactly on the same core version with
an inherited workspace dependency, `default-features = false`, and
`polling-client`, and declares
`godot = ">=0.4.5, <0.6"` with no-thread WASM and lazy-function-table support.
Its minimum 0.4.5 and latest 0.5.4 standalone fixtures must each lock exactly
one `godot` and one version of every `godot-*` family crate.
Tests additionally use full-featured `tokio` and `tracing-subscriber`.

## Key Design Decisions

### Framed Transport Agnosticism

The `Transport` trait decouples protocol logic from framed network I/O. Tests use
in-memory transports; Server 0.7 production I/O exposes only WebSocket. Custom
backends must provide one complete, ordered text/binary stream for the intended
server and own trust/source binding plus raw stream/datagram policy.

Concrete engine bindings live outside core. In particular, all Godot bindings,
constructors, backend behavior, and godot-rust public types belong to the
lockstep adapter. A Godot Engine version, godot-rust version, Rust MSRV, and
Emscripten SDK version are independent compatibility axes.

### Low-Latency Socket Defaults

Socket-owning transports disable Nagle (`TCP_NODELAY`) by default; `WebSocketTransport`
callers override with `WebSocketConnectOptions::with_disable_nagle(false)`. Native
connections cap frames and assembled messages at 8 MiB; `None` disables both and
`from_stream` preserves caller policy. This is a client resource policy, not a protocol maximum: deployments advertise their own outbound bound through v3 `ProtocolInfo.max_outbound_message_size` (mirrored into `ClientSnapshot::server_max_outbound_message_size`), pre-connect `/client-config` endpoints, and the `x-signal-fish-max-outbound-message-size` upgrade header; an over-limit server delivery closes with RFC 6455 code 1009.
Receive polls bound skipped controls, flush Pong/Close, and fuse terminal errors.

### Wire Compatibility

`ClientMessage` and `ServerMessage` use adjacently-tagged serde encoding
(`#[serde(tag = "type", content = "data")]`) to match the Signal Fish server
v2 JSON protocol. Server 0.7.0 commit `3f7f43d4cd4b3cc7f8fb893220dc35c9b1fad333` remains the released runtime compatibility binding.
The wire samples and AsyncAPI protocol authority include the post-0.7 room-correlation extension, advertised outbound limit, and room-session incompatibility error at commit `5de9105e4c269a29919ae29880f5b67fc8d630c3`.
Never change serde attributes without verifying both bindings. See `skills/serde-patterns/SKILL.md` and `skills/protocol-wire-conformance/SKILL.md` for details.

### Exhaustive Public Types

Public enums and protocol payload structs are exhaustive. `SignalFishEvent`,
`ErrorCode`, `SignalFishError`, and protocol payload types all require explicit
handling of their known variants. Adding variants to these enums is a semver
breaking change.

### Delivery Accountability

Negotiated v3 delivery carries per-sender epoch/sequence stamps. The SDK ports
the server 0.4.0 native reference state machine and validates snapshots,
lifecycle transitions, prior exact gap coverage, cumulative counters, terminal
and reconnect watermarks, and unsupported-format causality. Stale payloads are
suppressed. The shared core also rejects messages outside authentication,
negotiation, room, role, and version phases before state mutation. Those
lifecycle/plan/signal offenders are always suppressed; delivery-accountability
`Observe` retains diagnostic delivery. Policy defaults to quarantine until a
new authoritative room/reconnect snapshot. `Pong` remains connection-scoped
while authentication and protocol negotiation are still in flight.

Requested game-data format preserves omission; effective format resolves from
the first canonical Server 0.7 `ProtocolInfo`, with unsupported requests using
JSON. V3 reconnects require replay, complete watermarks, and a rotated token;
v2 exposes none. `Reconnected` fences peers until a fresh live `SessionPlan`.

Per-room accountability metadata is bounded against hostile or pathologically
churning servers (issue #166); overflow is an ordinary server-misbehavior
diagnostic under the violation policy, and every bound validates before
mutating state: more than 16 unresolved incarnation announcements or
uncovered departed incarnations per sender; more than 1024 total retained
exact gap ranges; roster inserts beyond the advertised `max_players`
(wire-absolute `u8::MAX + 1` fallback when the latest baseline cannot
advertise one) — servers swapping players on a full room must order the
departure before the replacement join. Unknown-player `PlayerReconnected`
sender cursors remain the
one documented trusted-server envelope: containment gates would reject
legitimate reconnects of players absent from the local roster snapshot.

### No Heavy Dependencies

No `chrono` (timestamps remain `String` from the server), no `bytes` (binary
payloads are `Vec<u8>` with `serde_bytes`), no `reqwest` (HTTP is out of scope).
TLS is opt-in: the default build pulls no crypto stack; `wss://` support
(rustls + ring) is gated behind the `tls` feature.

### Connection / Auth Flow

1. A token-binding-selected native WebSocket consumes its challenge and
   establishes proof state inside `connect_with_options`; disabled transports
   keep the old path.
2. `SignalFishClient::start(transport, config)` queues `ClientMessage::Authenticate`
   immediately before spawning the transport loop.
3. Server responds with `ServerMessage::Authenticated` → `SignalFishEvent::Authenticated`.
4. Client may then call `join_room`, etc.
5. Both clients emit synthetic `SignalFishEvent::Connected` when their driver first observes `Transport::is_ready() == true`.
   `SignalFishEvent::Disconnected` is emitted when the transport closes (best-effort; missed only if the receiver is dropped,
   delivery was preempted as bounded above, or the handle is dropped without `shutdown()`).

## Protocol Overview

Both `ClientMessage` and `ServerMessage` use adjacent tagging:

```json
{ "type": "JoinRoom", "data": { "game_name": "my-game", ... } }
{ "type": "RoomJoined", "data": { "room_id": "...", ... } }
```

Variant names are PascalCase in JSON (serde default for adjacently-tagged enums
with no `rename_all`). Protocol v3 adds the additive, opt-in mesh (the default
stays a byte-identical-to-v2 "relay floor"); WebRTC signals are externally tagged
(`{ "Offer": "..." }`). See `skills/serde-patterns/SKILL.md` for the full wire format,
and `skills/protocol-versioning-and-negotiation/SKILL.md` + `skills/webrtc-mesh-signaling/SKILL.md`
for the v2/v3 deltas.

## `.llm/` Structure

- `.llm/context.md` -- this file (canonical source of truth)
- `.llm/skills/index.md` -- auto-generated human-readable skill catalog (do not edit)
- `.llm/skills/<name>/SKILL.md` -- focused skill with YAML trigger metadata
- `.llm/skills/<name>/{scripts,references,assets}/` -- on-demand resources

Agents discover skills from frontmatter and load a matching `SKILL.md` only when
its description applies. Resolve links from its directory; directory and
frontmatter names must match and use lowercase hyphen-case.

## Progress Records

Planning records are local-only working notes: session logs/evidence under `progress/` and the `PLAN.md` roadmap are gitignored, never committed or force-added; durable conclusions belong in `CHANGELOG.md`, docs, or tests. No file may be both tracked and ignored (`no_file_is_both_tracked_and_ignored`).

## Documentation Rendering (MkDocs)

MkDocs Material + pymdownx powers Pages. `hooks/rustdoc_codeblocks.py` strips
rustdoc fence annotations for Pygments; `hooks/llms_txt.py` generates model text.
Mermaid requires `custom_fences` in `mkdocs.yml`. Approved Vector assets use pinned provenance and local OFL fonts preloaded via `overrides/main.html`; both palettes retain AA contrast, visible focus, reduced-motion behavior, and responsive scrolling (`docs_brand_policy` pins these contracts; evidence under local-only `progress/assets/`). CI runs strict MkDocs
(`.github/workflows/docs-deploy.yml`) and 17 checks in
`.github/workflows/docs-validation.yml`; see `skills/markdown-and-doc-validation/SKILL.md`.

## Pre-commit Enforcement

A pre-commit hook enforces:

1. No `.llm/*.md` file exceeds 500 lines (`scripts/pre-commit-llm.py`)
2. Every skill uses `.llm/skills/<name>/SKILL.md`, valid YAML frontmatter, and
   a description that states what the skill does and when to activate it
3. `skills/index.md` is auto-regenerated from skill frontmatter and headings
4. `cargo fmt --all -- --check` passes
5. `cargo clippy --workspace --all-targets --all-features -- -D warnings` passes
6. Workflow guard checks pass (`scripts/check-workflows.sh`): explicit step names, MSRV/toolchain policy, fenced-YAML step-key alignment
7. FFI safety check and its script tests pass (`scripts/check-ffi-safety.sh`)
8. Test quality check passes (`scripts/check-test-quality.sh`) — catches `&mut <literal>` temporaries
9. Devcontainer compatibility checks pass (`scripts/check-devcontainer-compat.sh`, plus a Dockerfile `docker buildx build --check` when buildx is available)
10. MkDocs admonition/details titles are well-formed (`scripts/check-admonitions.py`) — no embedded double quotes

`cargo test` runs on push, not every commit (too slow for a blocking hook) —
run it manually before opening a PR.

Install hooks with: `bash scripts/install-hooks.sh`
