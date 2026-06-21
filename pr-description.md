## Summary

This PR introduces protocol v3 mesh signaling support (opt-in), improves reconnect/mesh reliability, and adds stronger wire-level conformance coverage, along with substantial documentation and devcontainer compatibility improvements.

## User-Facing Changes

- Added protocol v3 signaling surface:
  - New signaling/event/message types for mesh workflows.
  - New client APIs for signaling (`send_offer`, `send_answer`, `send_ice_candidate`, transport status reporting, and related helpers).
  - Optional mesh runtime components (`MeshSession`, `MeshController`, `WebRtcDriver`) and a new `mesh_session` example.
- Kept relay-first behavior stable:
  - Existing relay users are unaffected unless mesh is explicitly enabled.
  - Protocol negotiation and compatibility handling were expanded and hardened.
- Updated game start flow:
  - Game start is explicit via `start_game()` instead of auto-start-on-ready.
  - New server error code mappings around start permissions/readiness and signaling constraints.
- Improved reconnect and mesh event correctness:
  - Better replay/folding of batched missed mesh events during reconnect.
  - Fixed transport status and ICE pre-gather edge cases to avoid false or missing state transitions.
- Expanded protocol quality checks:
  - Added golden-wire sample fixtures and byte-level conformance tests for v2/v3.
  - Added negotiation robustness and polling parity test coverage.

## Documentation and Developer Experience

- Added/expanded docs across protocol, events, errors, concepts, examples, and getting started.
- Added dedicated guides: protocol versioning and mesh usage.
- Improved devcontainer behavior for cross-platform setups (Windows/macOS/Linux/WSL/Codespaces/remote Docker).
- Added CI checks/workflows for protocol sync and devcontainer compatibility validation.

## Validation

- Branch includes broad automated coverage additions in protocol, client behavior, wire conformance, negotiation robustness, and CI policy checks.
