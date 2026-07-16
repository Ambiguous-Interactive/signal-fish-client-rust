---
name: doc-accuracy-guarantees
description: Write evidence-backed behavioral guarantees for concurrent client code. Use when documenting event delivery, errors, shutdown, backpressure, timing, or any claim using absolute language.
---

# Documentation Accuracy for Behavioral Guarantees

Reference for writing accurate documentation about behavioral guarantees
(e.g., event delivery, error handling, shutdown behavior) in async/concurrent
systems.

## The Problem

Doc comments and user-facing docs can overstate guarantees. Words like "always",
"never", "guaranteed", and "unconditional" create contracts the implementation
may not fully honor, especially in async systems with timeouts, task
cancellation, channel drops, etc.

## Rules

1. **Audit absolute claims** — Before writing "always", "never", "guaranteed",
   or "unconditional", verify every code path. Common failure modes in async
   Rust:
   - Receiver dropped — `send().await` returns `Err`
   - Task aborted (timeout, cancellation) — code after the abort point never
     runs
   - Channel full — a `try_send` path refuses or drops the message. (This
     SDK's event path uses `send().await`, so a full channel *blocks* the
     transport loop instead of dropping; its command path is bounded and
     refuses with `SendBufferFull` rather than dropping.)
   - Panic in spawned task — subsequent code in that task is skipped

2. **Qualify delivery semantics** — Describe what the mechanism prevents AND
   what it does not prevent.
   - BAD: "Events are always delivered regardless of capacity."
   - GOOD: "Events are never dropped on overflow — a full channel pauses the
     transport loop and backpressure propagates to the server — but an event
     may be missed if the receiver is dropped, on shutdown (which abandons at
     most one in-flight event and delivers the terminal `Disconnected`
     best-effort), or if the handle is dropped without shutdown."

3. **Document timeout/abort consequences** — If a function has a timeout that
   aborts work, document what events or side effects may be skipped when the
   timeout fires.

4. **Cross-reference related guarantees** — When a guarantee depends on other
   configuration (e.g., `shutdown_timeout`), link to it so readers understand
   the full picture.

## Checklist for New Doc Comments

- [ ] Does the comment contain "always", "never", "guaranteed", or
      "unconditional"?
- [ ] If yes, have ALL code paths been verified? (task abort, receiver drop,
      panic, timeout)
- [ ] Are failure modes documented alongside the guarantee?
- [ ] If the guarantee depends on configuration (e.g., timeout values), is
      this noted?
- [ ] Does the user-facing docs (`docs/*.md`) match the code-level doc
      comments?

## Examples from This Codebase

### `emit_event_or_shutdown` — Qualified guarantee

The transport loop delivers events with backpressure but lets a shutdown
request preempt a blocked delivery, so the guarantee is scoped precisely:

```rust
/// Emit an event with backpressure, but let a shutdown request preempt the
/// wait. `biased` polls the delivery arm first, so when both are ready the
/// event is still delivered; only a genuinely blocked delivery (consumer not
/// draining) lets shutdown win, abandoning that one in-flight event.
```

### `shutdown_timeout` — Documenting abort consequences

```rust
/// If the timeout expires the task is aborted and the `Disconnected` event
/// may not be delivered.
```

## When to Apply This Skill

- Writing or reviewing doc comments for async functions with delivery semantics
- Documenting shutdown/cleanup behavior
- Describing channel-based event delivery
- Any documentation that uses absolute language about runtime behavior
