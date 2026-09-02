# Migrating from 0.10 to 0.11

Version 0.11 makes runtime failures easier to diagnose. Transport send and
receive errors keep the backend's original error instead of a flattened
`String`, and misconfiguration is reported as a typed `InvalidConfig` error
instead of wearing an I/O costume. This guide covers these two error-surface
changes; the release's other breaking changes are listed at the end.

## Structured transport error causes

`SignalFishError::TransportSend` and `SignalFishError::TransportReceive` now
carry a boxed original error (`Box<dyn Error + Send + Sync>`) as their
`#[source]` cause instead of a `String`:

```diff
-Err(SignalFishError::TransportSend(message)) => {
+Err(SignalFishError::TransportSend(cause)) => {
     eprintln!("Transport send failed: {cause}");
 }
```

Log lines need no change: `Display` is byte-identical to 0.10 because the
boxed cause's own message replaces the old flattened text. Simple bindings
like the one above keep compiling.

What breaks is code that relied on the payload being a `String` — for example
`message.as_str()`, storing it in a `Vec<String>`, or matching on string
content. For programmatic handling, walk the cause chain with
`Error::source()`. The built-in native WebSocket boxes the backend's own
`tungstenite::Error`, whose chain reaches the underlying `std::io::Error`
when there is one:

```rust,ignore
use std::error::Error as _;

match client.send_game_data(payload) {
    Ok(()) => {}
    Err(err) => {
        eprintln!("send failed: {err}");
        // New in 0.11: reach the root cause for programmatic handling.
        let mut source = err.source();
        while let Some(cause) = source {
            if let Some(io) = cause.downcast_ref::<std::io::Error>() {
                eprintln!("root cause: {} ({:?})", io, io.kind());
            }
            source = cause.source();
        }
    }
}
```

Custom transports box whatever error they produce — an `io::Error`, a typed
backend error, or a plain string detail. Constructing from a string keeps
working through `.into()`:

```rust,ignore
Err(SignalFishError::TransportSend(
    "this text-only loopback does not accept binary frames".into(),
))
```

When a backend error's own `Debug` would embed application payload bytes,
box its `Display` text instead so ambient logs stay redacted.

## Typed configuration errors

The new exhaustive variant
`SignalFishError::InvalidConfig { field, problem }` reports caller-supplied
values rejected before any network I/O. Exhaustive `SignalFishError` matches
must add the arm:

```rust,ignore
match error {
    SignalFishError::InvalidConfig { field, problem } => {
        // The value itself is unusable; retrying without correcting it
        // keeps failing.
        eprintln!("invalid configuration: {field}: {problem}");
    }
    // ... remaining arms unchanged
}
```

Each rejected value in this table returns `InvalidConfig` where 0.11
development builds surfaced `Io` (with `ErrorKind::InvalidInput` or
`ErrorKind::Other`); the zero-limit options themselves are also new in this
release. Code matching `Io` around connect-time validation should match
`InvalidConfig` instead:

| Rejected value | Where |
|----------------|-------|
| Zero `max_inbound_message_size` | Native `WebSocketTransport` options |
| Zero `max_inbound_queue_bytes` | Emscripten `EmscriptenWebSocketConnectOptions` |
| URL that cannot be parsed into a WebSocket request | Native transport (checked before I/O) and Godot adapter (`ERR_INVALID_PARAMETER`) |
| URL containing interior NUL bytes | Emscripten transport |
| `wss://` URL without the opt-in `tls` feature | Native transport |

Failures determined by the engine or the network keep the `Io`
classification: Godot's `ERR_UNAVAILABLE`/`FAILED` engine faults, malformed
server handshake responses, and HTTP upgrade rejections. The split is
deliberate — `InvalidConfig` means "the value is wrong on its face", while
`Io` still means "something outside your configuration failed".

## Other 0.11 breaking changes

The 0.11 window deliberately bundles further breaking changes that have their
own migration notes:

- Custom transports must implement `abort` explicitly; see
  [Migrating custom transports for 0.11](transport.md#migrating-custom-transports-for-011).
- `supports_mesh()` semantics and the new exhaustive `ClientSnapshot` fields;
  see [the 0.11 capability-versus-active-plan
  migration](protocol-versioning.md#011-migration-capability-versus-active-plan).
- The remaining API additions — pre-authentication refusals, token-binding
  connect options, generation-bound driver signaling, and the new error
  variants behind exhaustive matches — are enumerated under `CHANGELOG.md`'s
  `[0.11.0]` **Breaking** bullets and in the
  [errors guide](errors.md) variant table.
