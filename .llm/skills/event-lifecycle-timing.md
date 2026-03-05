# Event Lifecycle Timing

Reference for understanding and correctly documenting the timing semantics of
synthetic events (`Connected`, `Disconnected`) across different client types.

## The Problem

Synthetic events like `Connected` are not triggered by server messages — they
are emitted by the client itself. The timing of these events differs between
`SignalFishClient` (async) and `SignalFishPollingClient` (synchronous polling),
which can mislead callers if not documented clearly.

## Client-Specific Timing

### `SignalFishClient` (async, tokio-based)

- `Connected` is emitted at the **start of the transport loop** (`transport_loop()`
  in `src/client.rs`), before entering the `tokio::select!` loop.
- By this point, the transport is already connected — the caller passed an
  already-connected transport to `start()` (e.g., via
  `WebSocketTransport::connect(url).await`).
- `Connected` genuinely reflects a completed handshake.

### `SignalFishPollingClient` (synchronous, noop-waker)

- `Connected` is emitted once `Transport::is_ready()` returns `true` during
  a `poll()` cycle. The check happens after the recv drain loop, so transports
  that process their open event during recv (e.g., `EmscriptenWebSocketTransport`)
  will trigger `Connected` in the same poll cycle.
- For transports that are already connected at construction time (default
  `is_ready() = true`), `Connected` fires on the first `poll()` call.
- For `EmscriptenWebSocketTransport`, `Connected` is deferred until the
  browser's `onopen` callback fires, which sets `opened = true` and makes
  `is_ready()` return `true`.
- `IncomingEvent::Open` from the Emscripten transport is consumed by `recv()`
  and sets the `opened` flag rather than being surfaced to the caller.

## Rules

1. **`Connected` is tied to `Transport::is_ready()`** — for the polling
   client, `Connected` fires only after the transport confirms readiness.
   Document any transport whose `is_ready()` has non-trivial behavior.

2. **Document transport-specific behavior** — if a transport's `connect()`
   returns before the handshake is complete, document what happens to messages
   sent in the interim (e.g., browser buffering).

3. **Keep event ordering invariants documented and tested** — `Connected` must
   always be the first event returned by the first `poll()` call, before any
   server-derived events.

4. **Use `tracing::info!` for transport lifecycle events** — connection open,
   close, and error events should be logged at `info` level (not `debug`) to
   aid debugging in production.

5. **Cross-reference timing differences in user-facing docs** — `docs/wasm.md`,
   `docs/events.md`, and the type-level doc comments should all mention the
   timing difference between async and polling clients.

## Checklist for New Synthetic Events

- [ ] Is the event documented as synthetic in the `SignalFishEvent` enum?
- [ ] Does the doc comment specify when the event fires for BOTH client types?
- [ ] Are timing caveats noted in `docs/events.md` and `docs/wasm.md`?
- [ ] Is there a test verifying the event's position in the event ordering?
- [ ] Does `.llm/context.md` mention the event in the Connection/Auth Flow?

## Related Skills

- [doc-accuracy-guarantees.md](doc-accuracy-guarantees.md) — qualifying
  absolute claims about delivery semantics
- [transport-abstraction.md](transport-abstraction.md) — Transport trait design
  and polling-client contract
