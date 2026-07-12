# Godot 4.5 web smoke fixture

This no-GDScript GDExtension fixture exercises the supported
`GodotWebSocketTransport` path with official Godot 4.5 web templates. It starts
two polling clients against a test server on `127.0.0.1:3536`, authenticates,
joins one room, verifies application Ping/Pong and a text relay, and prints
stable `SIGNAL_FISH_SMOKE` markers for browser automation.

The protocol negotiates one game-data encoding per connection. The browser
harness therefore uses a separate MessagePack pair when checking binary relay;
this scene is the JSON pair.

The fixture is a standalone Cargo workspace so the SDK's normal all-target
commands do not try to link a GDExtension test binary outside Godot.
