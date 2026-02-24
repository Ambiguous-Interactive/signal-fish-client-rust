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
   - Channel full — `try_send` drops the message
   - Panic in spawned task — subsequent code in that task is skipped

2. **Qualify delivery semantics** — Describe what the mechanism prevents AND
   what it does not prevent.
   - BAD: "The Disconnected event is always delivered regardless of capacity."
   - GOOD: "The Disconnected event uses a blocking send so it will not be
     dropped due to a full channel, but it may be missed if the receiver is
     dropped or if shutdown times out and aborts the transport task."

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

### `emit_disconnected` — Qualified guarantee

```rust
/// Uses `send().await` (blocking) instead of `try_send` so that `Disconnected`
/// is not dropped due to channel backpressure. However, delivery is not
/// unconditional: the event will be lost if the receiver has been dropped, or
/// if `shutdown` aborts the transport task before this function completes.
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
