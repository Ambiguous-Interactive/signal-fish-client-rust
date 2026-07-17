## Summary

Completes the remaining issue #61 reliability work by hardening browser
transport ownership, replacing a startup-sensitive soak assertion with a
phase-aware rollback oracle, strengthening independent CI validation, and
synchronizing the public documentation with the actual released/unreleased API.

## Changes

- Retain Emscripten outbound frames while the browser socket is connecting and
  after preparation/FFI send failures.
- Track early warm-up and steady-state Fortress confirmation lag separately:
  the first 60 frames remain bounded by the 20-frame prediction window, while
  steady/final lag remains capped at 8 clean or 13 impaired/soak frames.
- Independently validate exact load conservation, multi-frame polling, queue
  ceilings, adaptive buffering, and required schema with negative controls.
- Compile complete Rust documentation examples containing Unicode ellipses and
  make broken MkDocs links blocking.
- Correct stale client, event, protocol, transport, WebAssembly, migration, and
  installation documentation, including explicit `0.8.0` versus `main` usage.

## Validation

- [x] `cargo fmt`
- [x] `cargo clippy --all-targets --all-features -- -D warnings`
- [x] `cargo test --all-features`
- [x] Nested Godot fixture format, Clippy, and eight unit tests with Godot 4.5
- [x] Emscripten-target Clippy with warnings denied
- [x] Documentation snippet extraction, strict MkDocs rendering, Markdown lint,
  and typos
- [x] Godot E2E validator negative controls and CI policy tests
