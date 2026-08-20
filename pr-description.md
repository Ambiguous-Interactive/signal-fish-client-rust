## Summary

Clarifies the connection phase and traffic-counter contracts shared by the
async and polling clients. Applications can now distinguish client ownership
from transport readiness, and `ClientStats` counts at stable transport/decode
boundaries instead of backend completion or application delivery.

Closes #104.

## Changes

- Add `ClientSnapshot::transport_ready` and matching async, polling, and
  `SignalFishClientApi` accessors; emit `Connected` exactly once on the first
  driver observation of `Transport::is_ready()`.
- Preserve FIFO command admission while connecting, require deferred async
  readiness to wake registered I/O, and reset every connection phase on
  terminal paths.
- Count `game_data_sent` at exact frame ownership transfer, including
  accepted-Pending and accepted-then-error sends without double-counting.
- Count `game_data_received` immediately after successful text or binary
  protocol decode, before message validation, sequence accountability, stale,
  quarantine, or event suppression; preserve physical-binary admission checks
  that reject a frame before logical decoding.
- Document the valid phase tuples, counter conservation limits, excluded
  traffic, and cross-peer diagnostic caveats across the public guides,
  changelog, roadmap, canonical context, and focused agent skills.

## Validation

- [x] Focused async and polling readiness tests
- [x] Focused ownership-transfer and decoded-receipt tests
- [x] Async/polling frame, snapshot, and statistics parity tests
- [x] Repository documentation and workflow policy validators
- [x] `cargo fmt && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features`
- [ ] Hosted required checks and review feedback
