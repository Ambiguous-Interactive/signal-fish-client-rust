---
name: event-lifecycle-timing
description: Preserve lifecycle event timing across async and polling clients. Use when changing or documenting Connected, Disconnected, transport readiness, shutdown, or receiver-drop behavior.
---

# Event Lifecycle Timing

Reference for understanding and correctly documenting the timing semantics of
synthetic events (`Connected`, `Disconnected`) across different client types.

## The Problem

Synthetic events like `Connected` are not triggered by server messages — they
are emitted by the client itself. Both clients use the same readiness boundary,
but observe it through different driver mechanics.

## Client-Specific Timing

### `SignalFishClient` (async, tokio-based)

- `Connected` is emitted when the transport loop first observes
  `Transport::is_ready() == true`.
- Built-in `WebSocketTransport::connect` is already ready. A custom transport
  may still be handshaking when passed to `start()` and defers the event.
- A false-to-true readiness transition must wake a waker registered by
  `poll_send` or `poll_recv` so the async loop can observe it.

### `SignalFishPollingClient` (synchronous, noop-waker)

- `Connected` is emitted once `Transport::is_ready()` returns `true` during
  a `poll()` cycle. Readiness is checked before I/O and after the recv drain,
  so already-ready immediate-close ordering and transports that process their
  open event during recv are both represented correctly.
- For transports that are already connected at construction time (default
  `is_ready() = true`), `Connected` fires on the first `poll()` call.
- For `EmscriptenWebSocketTransport`, `Connected` is deferred until the
  browser's `onopen` callback fires, which sets `opened = true` and makes
  `is_ready()` return `true`.
- `IncomingEvent::Open` from the Emscripten transport is consumed by `recv()`
  and sets the `opened` flag rather than being surfaced to the caller.

## Rules

1. **`Connected` is tied to `Transport::is_ready()`** — both clients fire it
   only after the transport confirms readiness.
   Document any transport whose `is_ready()` has non-trivial behavior.

2. **Document transport-specific behavior** — if a transport's `connect()`
   returns before the handshake is complete, document what happens to messages
   sent in the interim (e.g., browser buffering).

3. **Keep event ordering invariants documented and tested** — `Connected` must
   be the first event in the cycle that observes readiness, before any
   server-derived events from that cycle.

4. **Use `tracing::info!` for transport lifecycle events** — connection open,
   close, and error events should be logged at `info` level (not `debug`) to
   aid debugging in production.

5. **Cross-reference driver mechanics in user-facing docs** — `docs/wasm.md`,
   `docs/events.md`, and type-level doc comments should identify the shared
   readiness boundary and how each driver observes it.

## Checklist for New Synthetic Events

- [ ] Is the event documented as synthetic in the `SignalFishEvent` enum?
- [ ] Does the doc comment specify when the event fires for BOTH client types?
- [ ] Are timing caveats noted in `docs/events.md` and `docs/wasm.md`?
- [ ] Is there a test verifying the event's position in the event ordering?
- [ ] Does `.llm/context.md` mention the event in the Connection/Auth Flow?

## Related Skills

- [doc-accuracy-guarantees.md](../doc-accuracy-guarantees/SKILL.md) — qualifying
  absolute claims about delivery semantics
- [transport-abstraction.md](../transport-abstraction/SKILL.md) — Transport trait design
  and polling-client contract
