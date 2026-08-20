## Summary

Gives every custom transport an explicit close-deadline abandonment hook.
`Transport::abort` is now required; both client drivers invoke it for deadline
expiry, close errors, or owner drop before graceful close completes, while the
async ownership guard also covers task cancellation and panic unwinding.

Closes #106.

## Changes

- Require every `Transport` implementation to provide a synchronous,
  idempotent abort path that releases or safely detaches backend resources and
  discards retained accepted sends.
- Add an async transport ownership guard plus polling-client drop fallback so
  cancellation and drop cannot bypass backend abandonment.
- Preserve backend-owned send completion before graceful close under the one
  configured deadline; abort on expiry or close failure and never poll the
  transport afterward.
- Prove accepted-send hangs, close hangs/errors, resource release, repeated
  shutdown/drop, event termination, and zero post-abort transport activity in
  both drivers; strengthen native WebSocket and Godot abort tests.
- Add third-party migration guidance and update custom transport examples,
  lifecycle docs, changelog, roadmap, canonical context, and focused skills.

## Validation

- [x] Focused async/polling deadline, close-error, and drop tests
- [x] Built-in native WebSocket and Godot abort contract tests
- [x] Repository documentation and policy validators
- [x] `cargo fmt && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features`
- [ ] Hosted required checks and review feedback
