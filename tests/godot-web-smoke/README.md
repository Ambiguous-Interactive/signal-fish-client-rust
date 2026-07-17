# Godot 4.5 web smoke fixture

This no-GDScript GDExtension fixture exercises the supported
`GodotWebSocketTransport` path with official Godot 4.5 web templates. It starts
separate JSON and MessagePack client pairs against server 0.4.0 on
`127.0.0.1:3536`, authenticates, joins rooms, verifies application Ping/Pong
plus text and binary relays, gracefully shuts down the JSON pair, then waits
for a server drain and verifies WebSocket close code 4000 attribution on the
binary pair. Stable `SIGNAL_FISH_SMOKE` markers drive browser automation.

Before shutdown, the fixture proves four binary packets are accepted in one
rendered callback, then runs two JSON clients at 136 offered frames/second each
for 16 seconds. It records queue depth, Godot/browser buffering, acceptance and
receipt counts, poll duration, and end-to-end timestamp latency. The browser
gate requires exact reliable receipt, bounded/finally drained client queues, a
non-positive final queue slope over eight consecutive 250 ms drained samples,
p99 latency at most 500 ms, and every `poll()` below 50 ms. CI always uploads JSON/CSV time series
and before/after `/metrics/prom` snapshots; browser/server logs are retained on
failure. The transport wrapper also requires zero contemporaneous adaptive
watermark violations and reports empty-buffer single-frame escape bytes
separately from the immutable 32 KiB ceiling. Server 0.4.0 does not expose an
internal queue/sojourn gauge, so the
fixture uses timestamped end-to-end latency and available conservation/drop
metrics instead of a client-side proxy.

CI also builds a negative-control variant that calls the raw
`EmscriptenWebSocketTransport` during extension initialization. The official
template must reject it with an undefined `emscripten_websocket_new` symbol;
this preserves executable evidence for why the Godot transport is required.

The fixture is a standalone Cargo workspace so the SDK's normal all-target
commands do not try to link a GDExtension test binary outside Godot.

## Fortress rollback scenario

The same fixture also contains a deterministic two-player
`fortress-rollback` 0.10.0 game. CI launches two independent Chromium
processes, each hosting its own Godot runtime and Signal Fish client, against
one real Signal Fish server 0.4.0 process. Player A creates a room and player B
joins the exact room code reported by A.

Each rendered callback polls Signal Fish exactly once, pumps a bounded relay
queue into Fortress, supplies deterministic local input, advances rollback,
and records both confirmed-input and serialized game-state checksums. Player B
changes its delayed input at frame 120 and holds its outbound relay until
player A's causal post-advance watermark proves that A predicted through the
changed input. The bounded hold deterministically forces A to roll back, load
state, and resimulate after release. Both peers advance on an independent fixed
18 Hz cadence, preserving elapsed deadline debt and recovering by at most one
simulation frame per rendered callback. Before those independent clocks begin,
A must observe B's frame-zero synchronization packet and B must observe A's
causally subsequent frame-one packet; the summary retains the release evidence.
The gate requires both clients to
confirm 600 frames within the scenario timeout, settle in sync with matching
state, and drain every relay and SDK queue,
conserve every client/server delivery, and cross-check the exact room and
player IDs. Confirmation lag is capped at eight clean, 13 impaired, and 12 soak
frames. Player B
then closes first; player A must observe its nonzero v3 `PlayerLeft` epoch and
final sequence before closing. Malformed packets, relay loss, desynchronization,
backend-capacity refusals, server drops, and slow consumers all fail the run.

After exporting the fixture and starting the server, run it with:

```shell
node scripts/run-godot-fortress-e2e.mjs \
  tests/godot-web-smoke/project/build
```

The runner writes per-process browser logs, before/after server metrics, and a
machine-readable `godot-fortress-summary.json`. The normal CI artifact upload
retains these files even when an assertion fails.
