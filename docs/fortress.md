# Godot + Fortress Rollback

Use `SignalFishPollingClient<GodotWebSocketTransport>` as the relay beneath a
Fortress Rollback `NonBlockingSocket`. This is the same architecture exercised
by the repository's two-process Godot browser test.

Add the rollback library beside the SDK in the Godot GDExtension crate:

```toml
fortress-rollback = "=0.10.0"
godot = { version = "0.5.4", features = ["api-custom", "experimental-wasm", "experimental-wasm-nothreads", "lazy-function-tables"] }
serde = { version = "1.0", features = ["derive"] }
signal-fish-client = { git = "https://github.com/Ambiguous-Interactive/signal-fish-client-rust", default-features = false, features = ["polling-client"] }
signal-fish-client-godot = { git = "https://github.com/Ambiguous-Interactive/signal-fish-client-rust" }
```

The adapter supports godot-rust 0.4.5 through 0.5.x and requires Rust 1.94;
the transport-agnostic core remains compatible with Rust 1.87. Keep the direct
`godot` dependency aligned with the binding used by the adapter. Run
`cargo tree -d` after dependency updates and resolve any duplicate `godot` or
`godot-*` families before passing `Gd` values across the adapter boundary,
because bindings from different versions are distinct Rust types.

The issue #61 polling and admission guarantees in this guide are currently on
`main` under [`Unreleased`](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/blob/main/CHANGELOG.md);
use the git dependency until the next crate release publishes them.

## Configure Signal Fish

Enable protocol v3 and MessagePack before constructing the polling client:

```rust,ignore
use signal_fish_client::protocol::GameDataEncoding;
use signal_fish_client::{SignalFishConfig, SignalFishPollingClient};
use signal_fish_client_godot::GodotWebSocketTransport;

let transport = GodotWebSocketTransport::connect("wss://example.com/v2/ws")?;
let mut config = SignalFishConfig::new("mb_app_example").enable_v3();
config.game_data_format = Some(GameDataEncoding::MessagePack);
let mut client = SignalFishPollingClient::new(transport, config);
```

Create or join the room only after `Authenticated`. Build the Fortress session
after `RoomJoined` and the roster contains every expected player. Sort Signal
Fish player UUIDs and use their positions as stable Fortress player handles so
every process derives the same mapping. The included fixture intentionally
requires exactly two players. For a larger game, sort the complete UUID roster
on every client and assign every handle from that shared order.

## Relay framing

Fortress `NonBlockingSocket::send_to` is synchronous and best-effort. Give the
socket bounded inbound and outbound queues shared with the game-loop adapter.
For each outbound Fortress `Message`:

1. Encode it with `fortress_rollback::network::codec::encode`.
2. Prefix the bytes with the destination player's 16-byte UUID.
3. Queue the frame without blocking.
4. Pump it with `send_binary_game_data`. Pop the front frame only on success;
   retain it for ordered retry on `SignalFishError::SendBufferFull`, and treat
   every other error as fatal without silently dropping the frame.

Signal Fish broadcasts game data to the room. On receipt, require a MessagePack
v3 envelope with nonzero sequence and epoch and verify the sender is in the
roster. Ignore a valid frame whose UUID prefix names another room member;
require locally addressed frames to name the local UUID, then decode the
remaining bytes with `fortress_rollback::network::codec::decode_message`.
Reject trailing bytes and keep both relay queues bounded.

The complete tested adapter is in
[`tests/godot-web-smoke/src/fortress.rs`](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/blob/main/tests/godot-web-smoke/src/fortress.rs).

## Frame order

Drive the integration once from Godot's `_process` callback:

1. Call `client.poll()` exactly once and route binary events into the relay.
2. Call `session.poll_remote_clients()`.
3. If Fortress is running, add deterministic local input and advance one frame.
4. Pump the relay's bounded outbound queue into the SDK command queue.

Messages produced in step 3 are offered to the WebSocket on the next rendered
callback. This ordering preserves the real frame-driven pressure that exposed
issue #61 while keeping each callback bounded. Inspect `polling_stats()` for
queue peaks and work-budget exhaustion, and `queue_age_stats()` so a
stable-depth but increasingly stale queue is visible; reset the age peak when
measured simulation begins. Inspect `transport_diagnostics()` separately for
admission hits, backend buffering, and accepted multi-frame bursts.

## System test

The required `Godot Web` checks reuse one official Godot 4.5 no-thread export
across clean, impaired, and soak jobs. Every job launches two independent
Chromium processes and a real Signal Fish Server 0.4.0. The clean case advances
600 confirmed frames; the impaired case adds seeded bidirectional 40 ms delay,
10 ms jitter, 0.2% correlated loss, a 10 Mbit/s rate, and a six-callback polling
hitch at frame 240; the soak advances 3,600 confirmed frames under the same
profile. The fixture configures a 20-frame prediction window so acceptable
constrained-network lag and the declared hitch can recover without an internal
scheduler stall. Simulated frames 1 through 60 are an explicit browser
renderer/JIT warm-up phase bounded by the 20-frame prediction window. From
frame 61 onward, the scenario oracles cap confirmation lag at eight clean or
13 impaired/soak frames; final current lag must obey the same scenario bound.
Simulation advances on a fixed
local 18 Hz cadence, independent of peer or network progress, so unequal browser CPU
slices do not become artificial frame advantage and real prediction-window
stalls remain observable. Delayed callbacks retain their elapsed deadline debt
and recover by at most one simulation frame per rendered callback, preventing
permanent scheduling skew without allowing a multi-frame burst. A bounded
one-time proposal/ack/commit barrier maps a shared deadline to each browser's
monotonic clock, preventing process-start order from becoming measured gameplay
skew. This wall-clock assumption is fixture-scoped because CI launches both
Chromium processes on the same host. A bounded relay hold uses causal
post-advance frame watermarks to prove the remote peer
predicted the changed delayed input before release, forcing rollback, state
load, and resimulation while both games keep advancing. The hitch oracle
separately requires forward simulation progress during its six skipped polling
callbacks. CI builds
the pinned, checksum-verified iproute2 6.6.0 `tc` because the runner's packaged
version cannot apply a deterministic netem seed.

The gates require exact checksum convergence, in-sync health, bounded
phase-aware confirmation lag, zero waits/stalls, at least two relay messages per simulated
frame, final queue depth and age of zero, a sampled queue-age peak no greater
than 500 ms, a non-positive final eight-sample queue-age slope for the soak,
exact client/server conservation, and an observable v3 `PlayerLeft` terminal
watermark. Browser/server logs, time series, summaries, Prometheus snapshots,
and netem seed/configuration are uploaded even on failure.
