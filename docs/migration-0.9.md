# Migrating from 0.8 to 0.9

Version 0.9 moves the concrete Godot integration out of the transport-agnostic
core and into a lockstep companion crate. Transport behavior and public type
names are unchanged, but this is a breaking dependency and import migration.

## Dependency migration

Replace the old core feature with the polling core, the Godot adapter, and the
Godot binding version selected by your GDExtension:

```diff
-signal-fish-client = { version = "0.8", default-features = false, features = ["transport-godot"] }
+godot = { version = "0.5.4", features = ["api-custom", "experimental-wasm", "experimental-wasm-nothreads", "lazy-function-tables"] }
+signal-fish-client = { version = "0.9", default-features = false, features = ["polling-client"] }
+signal-fish-client-godot = "0.9"
```

The adapter supports godot-rust 0.4.5 through every compatible 0.5.x release.
The browser/Fortress fixture pins 0.5.4, while a standalone minimum fixture
pins 0.4.5 and passes a directly constructed `Gd<WebSocketPeer>` through
`GodotWebSocketTransport::from_peer`.

Cargo treats the 0.4 and 0.5 lines of pre-1.0 dependencies as semver-incompatible
and can retain both in an existing lockfile. Check the resolved graph after
changing versions:

```sh
cargo tree -d
```

There must be one `godot` version and one version of each `godot-*` family
crate. If a 0.4.5 project retains a 0.5 binding selected only for the adapter,
align the lockfile explicitly and commit it:

```sh
cargo update -p godot@0.5.4 --precise 0.4.5
```

Do not pass `Gd` values between duplicate binding versions; those are distinct
Rust types even when they represent the same engine class.

## Import migration

Only the transport import moves:

```diff
-use signal_fish_client::{
-    GodotWebSocketTransport, SignalFishConfig, SignalFishPollingClient,
-};
+use signal_fish_client::{SignalFishConfig, SignalFishPollingClient};
+use signal_fish_client_godot::GodotWebSocketTransport;
```

`GodotWebSocketOptions` and `GodotBackpressurePolicy` move to
`signal_fish_client_godot` in the same way. Constructors, `from_peer` APIs,
backpressure behavior, diagnostics, and the `Transport` implementation retain
their existing names and semantics.

## Independent version axes

Keep these versions conceptually separate:

- Signal Fish core and adapter versions are published in lockstep.
- Godot Engine remains 4.5 for the supported export and custom API.
- godot-rust is supported from 0.4.5 through 0.5.x.
- Core requires Rust 1.87; the Godot adapter requires Rust 1.94.
- The web build keeps its independently pinned Emscripten SDK and nightly Rust.

The core crate no longer resolves or exposes godot-rust types. Non-Godot users
therefore keep the core MSRV and dependency graph independently of future
Godot binding upgrades.
