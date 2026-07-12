# Godot 4.5 web smoke fixture

This no-GDScript GDExtension fixture exercises the supported
`GodotWebSocketTransport` path with official Godot 4.5 web templates. It starts
separate JSON and MessagePack client pairs against server 0.4.0 on
`127.0.0.1:3536`, authenticates, joins rooms, verifies application Ping/Pong
plus text and binary relays, gracefully shuts down the JSON pair, then waits
for a server drain and verifies WebSocket close code 4000 attribution on the
binary pair. Stable `SIGNAL_FISH_SMOKE` markers drive browser automation.

CI also builds a negative-control variant that calls the raw
`EmscriptenWebSocketTransport` during extension initialization. The official
template must reject it with an undefined `emscripten_websocket_new` symbol;
this preserves executable evidence for why the Godot transport is required.

The fixture is a standalone Cargo workspace so the SDK's normal all-target
commands do not try to link a GDExtension test binary outside Godot.
