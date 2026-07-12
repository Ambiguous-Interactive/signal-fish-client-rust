# Delivery Contract & Backpressure

What actually happens to a message between `send_game_data(...)` on one
client and the event receiver on another — including what happens when a
consumer falls behind. This page documents the **end-to-end** contract
(client + server), not just this SDK's half of it.

Numbers on this page were originally measured against the reliable v2 relay.
Protocol-v3 behavior and accountability were verified against Signal Fish
Server 0.4.0 at commit `50b28a9`. The repository's
[`load_lab`](examples.md#load-lab-measurement-harness) example. Localhost figures are lower bounds —
real networks add RTT — but the *shapes* (where queues form, who waits,
who gets evicted) are configuration-driven and transfer directly.

## The pipeline

```text
your code ──► bounded command queue (1024) ──► WebSocket ──► server
                                                             │ per-recipient
                                                             │ FIFO queue (1024)
                                                             ▼
your code ◄── bounded event channel (256) ◄── WebSocket ◄── batcher (10 / 16 ms)
```

Every hop is bounded, and no hop drops silently:

- **Send side (this SDK):** `send_game_data` fails fast with
  [`SendBufferFull`](errors.md#handling-sendbufferfull) when the command
  queue is full; `send_game_data_reliable` waits for space. *Queued is not
  delivered* — commands still in the queue when the connection ends are
  discarded with it (surfaced by `Disconnected`).
- **Relay (server):** v2 and `GameDataDelivery::Reliable` preserve reliable,
  ordered delivery per connection. Protocol v3 additionally offers keyed
  `Latest` and loss-tolerant `Volatile`; every intentional omission is named
  by a causally prior exact `DeliveryReport` range. A reliable recipient that
  remains blocked through `slow_consumer_timeout_ms` is disconnected loudly.
- **Receive side (this SDK):** events are delivered with backpressure (never
  dropped merely because the consumer is behind); a consumer that stops
  draining stops the socket being read (the server then sees *you* as the slow
  consumer). Undecodable frames surface as
  [`DecodeFailed`](events.md#decodefailed) events.

## What a slow-consumer eviction looks like from here

When server 0.4.0 evicts this client, it writes a best-effort
`Error { error_code: SlowConsumer }` farewell and closes with semantic code
`4002 slow_consumer`. Measured outcomes (queue=8, timeout=500ms, 8 KiB
payloads; wedged consumer resumed after eviction):

| Signal | Observed |
|--------|----------|
| `Error { SlowConsumer }` event | **Arrived** — the farewell sat in the kernel receive buffer and surfaced once draining resumed. It may be lost when buffers are truly full; treat it as best-effort. |
| `Disconnected.last_server_error` | Carried the farewell (`SlowConsumer` + message) whenever the farewell arrived. |
| `Disconnected.reason` | Carries structured close code `4002` and reason `slow_consumer` when the close frame arrives. |

Handle it like this:

```rust,ignore
SignalFishEvent::Disconnected { reason, last_server_error } => {
    if let Some(info) = &last_server_error {
        if info.error_code == Some(ErrorCode::SlowConsumer) {
            // We weren't draining events fast enough and got evicted.
            // Slow down consumption work, raise event_channel_capacity,
            // or move heavy per-event work off the draining task —
            // then reconnect and rejoin.
        }
    }
}
```

## Protocol-v3 delivery classes and accountability

Use `SignalFishConfig::enable_v3()` to opt into stamped relay delivery without
advertising WebRTC. Existing `send_game_data` calls remain reliable and
v2-byte-compatible. `send_game_data_with_delivery` accepts:

| Class | Queue behavior | Intended use |
|---|---|---|
| `Reliable` | Preserve every message or disconnect the blocked recipient. | Control/state transitions that must not vanish. |
| `Latest { key }` | Keep the newest undelivered value for each sender-defined key. | Replaceable state such as position or aim. |
| `Volatile` | Drop under pressure rather than building stale backlog. | Ephemeral effects and frequent samples. |

Every v3 room payload is stamped with sender `epoch` and `seq`. The SDK ports
the server 0.4.0 reference accountability algorithm and validates snapshots,
lifecycle epochs, exact gap coverage, reports, terminal watermarks, and
reconnect baselines. A sequence hole is authorized only by the union of prior
exact `DeliveryReport` ranges for that sender and epoch; aggregate
`RelayStats` and counter deltas are diagnostics, not authorization.

On a violation the SDK emits `SignalFishEvent::ProtocolViolation`. The default
`ProtocolViolationPolicy::Quarantine` suppresses subsequent room game data
until `RoomJoined`, `SpectatorJoined`, or `Reconnected` establishes a fresh
authoritative baseline. `Disconnect` closes the signaling connection;
`Observe` reports the violation and still delivers the offending data, leaving
the application to decide whether and how to recover.
Inspect `client.snapshot().quarantined` when application state recovery matters.

## Reconnect token lifecycle

Server 0.4.0 issues a secret token in v3 `RoomJoined` and rotates it after a
successful `Reconnected`. The client stores the latest value in the coherent
`ClientSnapshot`; persist `snapshot().reconnection_token` together with the
room/player IDs before an unexpected disconnect. Reconnect on a fresh,
authenticated v3 connection, then replace the consumed token with the rotated
one from the new snapshot. `sender_watermarks` provide the authoritative
per-sender baseline, while `replay` describes completeness of replayed control
events. Never log reconnect tokens.

## Capacity: what the relay sustains (measured)

Localhost, 1 sender → 3 recipients, all draining promptly, default server
config. `load_lab throughput` sweep; latency is send-timestamp →
receive-drain on one clock:

| Payload | Offered rate | Delivered | p50 | p99 |
|---------|--------------|-----------|-----|-----|
| 256 B | 50–1600 msg/s | **100% at every rate** | 5–10 ms | 16–49 ms |
| 1 KiB | 50–1600 msg/s | 100% | 15–45 ms | 63–107 ms |
| 16 KiB | 50–800 msg/s | 100% | 10–26 ms | 22–91 ms |
| 16 KiB | 1600 msg/s (~78 MB/s fan-out) | ~99.9% accepted, delivered completely | 5.7 ms | 167 ms |

The latency floor is dominated by the server's write batching
(`batch_size` 10 / `batch_interval_ms` 16). **Healthy rooms are not the
hazard — slow consumers are:**

| Scenario (measured) | Result |
|---------------------|--------|
| One peer drains at 10 msg/s in a 120 msg/s room (default queue 1024, 25 s) | The slow peer's data ages to **22.9 s stale**; it is **never evicted** (eviction requires drain < 1 message per 5 s); nobody is notified. |
| Same, `send_queue_capacity=64`, 4 KiB payloads, 30 s | The **healthy** peers' latency climbs to **p95 5.4 s / p99 6.6 s** — a room broadcast waits on its slowest recipient, so one slow member paces everyone. |
| Backlogged recipient sends Pings during a flood | **1 of 15 Pongs arrived in 15 s** (the pre-backlog one, 3.8 ms; baseline p50 3.6 ms). Control messages share the single per-connection FIFO with game data. |

Practical guidance:

- **Drain events on a dedicated task** and keep per-event work minimal —
  the [wedged-consumer hazard](#the-wedged-consumer-hazard) below is the
  single most damaging client-side failure mode.
- Pace bulk sends with `send_game_data_reliable` and watch
  [`send_capacity()`](client.md) shrink as the congestion signal.
- Compare [`stats()`](client.md#send-queue-and-traffic-stats) counters across peers to detect
  relay-path anomalies; watch `messages_undecodable` for protocol drift.
- For loss-tolerant, latency-critical traffic (rollback-netcode inputs,
  position streams), the relay's reliable-and-ordered semantics work
  against you: a slow recipient converts your freshest packets into a
  backlog of stale ones. Prefer the [v3 mesh](mesh-guide.md) and WebRTC
  unreliable data channels for that traffic — the relay is the control
  plane and universal fallback. Note that `send_game_data` **always** uses
  the relay, even when a mesh session is established; mesh traffic goes
  through your `WebRtcDriver`'s data channels (`MeshController::send_to`).

## The wedged-consumer hazard

If your event consumer stops draining **forever**, the transport loop
blocks delivering the next event, stops reading the socket, and the server
eventually evicts you (`SLOW_CONSUMER`) — but the wedged client cannot
observe the eviction (it is not reading), so from the inside the session
just goes quiet. As of 0.7.0, [`shutdown()`](client.md#shutdown) preempts
the wedge: it abandons at most the one in-flight event delivery, closes
the transport cleanly, and completes without waiting for the timeout
abort. If draining ever resumes instead, the buffered events (often
including the eviction farewell) arrive, followed by `Disconnected`.

## Sizing the channels

| Knob | Default | Raise it when… |
|------|---------|----------------|
| `event_channel_capacity` | 256 | your consumer has bursty frame times (GC pauses, loading screens) and you want more absorption before socket-read backpressure engages. Capacity does not fix a consumer that is *sustainably* slower than the room's send rate — nothing client-side can. |
| `command_channel_capacity` | 1024 | you emit large synchronized bursts and prefer queuing to `SendBufferFull` refusals. The queue only drains at socket speed; deeper queues mean staler data, not more throughput. |
