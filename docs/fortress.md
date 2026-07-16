# Godot + Fortress Rollback

Use `SignalFishPollingClient<GodotWebSocketTransport>` as the relay beneath a
Fortress Rollback `NonBlockingSocket`. This is the same architecture exercised
by the repository's two-process Godot browser test.

Add the rollback library beside the SDK in the Godot GDExtension crate:

```toml
fortress-rollback = "=0.10.0"
serde = { version = "1.0", features = ["derive"] }
signal-fish-client = { version = "0.8.0", default-features = false, features = ["transport-godot"] }
```

## Configure Signal Fish

Enable protocol v3 and MessagePack before constructing the polling client:

```rust,ignore
use signal_fish_client::protocol::GameDataEncoding;
use signal_fish_client::{
    GodotWebSocketTransport, SignalFishConfig, SignalFishPollingClient,
};

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
issue #61 while keeping each callback bounded. Inspect `polling_stats()` and
`transport_diagnostics()` for queue peaks, work-budget exhaustion, admission
hits, and accepted multi-frame bursts.

## System test

The `Godot Web` workflow starts a real Signal Fish server, launches two
independent Chromium processes running the official Godot 4.5 no-thread export,
creates and joins one room, and advances a deterministic two-player Fortress
game for 600 confirmed frames. A deterministic eight-frame outbound impairment
forces prediction, rollback, state load, and resimulation. The gate then proves
matching serialized state checksums, in-sync Fortress health and checksum
comparisons, exact room/roster identity, relay and server conservation, an
observable v3 `PlayerLeft` terminal watermark, and zero residual queues. It
also rejects malformed envelopes, capacity hits, callback stalls, server drops,
and slow-consumer disconnects, and uploads per-process diagnostics on every run.
