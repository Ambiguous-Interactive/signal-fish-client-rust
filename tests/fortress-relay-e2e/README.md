# Fortress relay multiprocess E2E

This standalone test crate reproduces the traffic shape from
Fortress Rollback issue 242 with production components:

- one separately spawned Signal Fish Server 0.4.0 process;
- two separately spawned `fortress-relay-peer` game processes;
- a real `fortress-rollback` 0.10.0 `P2PSession` in each game;
- `SignalFishPollingClient<WebSocketTransport>` and protocol-v3 MessagePack relay.

Each game advances 600 confirmed frames with exactly one client/transport poll
per 60 Hz callback. The high-entropy per-frame inputs intentionally exercise
prediction misses and rollback repair. The test fails unless both peers sustain
at least 120 Fortress protocol messages per second and transfer more than two
game-data frames per callback, drain all client-owned queues, keep the oldest
command below 500 ms, remain below the prediction window, compare matching
game-state checksums, and observe no stalls, wait recommendations, adapter
overflow, malformed relay frames, unknown senders, event loss, or protocol-v3
metadata violations.

The adapter prepends the destination player UUID to every Fortress message, as
the issue-242 integration did. Its socket callback only admits bytes to a local
FIFO. The owner drains that FIFO into the client and restores a refused head,
preserving Fortress's non-blocking, best-effort socket contract without the
socket-wide `bufferedAmount == 0` stop-and-wait behavior.

Run it against a local server binary:

```sh
SIGNAL_FISH_SERVER_BIN=/path/to/signal-fish-server \
  cargo test --manifest-path tests/fortress-relay-e2e/Cargo.toml --all-targets -- --nocapture
```

The Godot no-thread browser E2E in the same workflow separately verifies the
browser-specific callback and `WebSocketPeer` admission path at 136 frames per
second for 16 seconds. Together, the two jobs cover the real Fortress game
protocol and the browser transport condition that caused the original defect.
