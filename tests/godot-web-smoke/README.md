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
for 16 seconds. It records queue depth, oldest queued-command age,
Godot/browser buffering, acceptance and receipt counts, poll duration, and
end-to-end timestamp latency. The browser
gate requires exact reliable receipt, no admission refusal, bounded/finally
drained client queues, non-positive final depth and oldest-age slopes, peak
oldest age at most 500 ms, p99 latency at most 500 ms, and every `poll()` below
50 ms. CI always uploads JSON/CSV time series
and before/after `/metrics/prom` snapshots; browser/server logs are retained on
failure. Server 0.4.0 does not expose an internal queue/sojourn gauge, so the
fixture uses timestamped end-to-end latency and available conservation/drop
metrics instead of a client-side proxy.

CI also builds a negative-control variant that calls the raw
`EmscriptenWebSocketTransport` during extension initialization. The official
template must reject it with an undefined `emscripten_websocket_new` symbol;
this preserves executable evidence for why the Godot transport is required.

The fixture is a standalone Cargo workspace so the SDK's normal all-target
commands do not try to link a GDExtension test binary outside Godot.
