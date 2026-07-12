# Shared Client Core and Drivers

Reference for preserving semantic parity between `SignalFishClient` and
`SignalFishPollingClient`.

## Ownership Boundary

`ClientCore` is the single transport-independent protocol state machine. It
owns:

- authentication command construction and every common command shape;
- protocol-v2/v3 guards and binary-format validation;
- text and binary frame decoding;
- delivery accountability and violation policy;
- state transitions, snapshots, counters, and last-server-error attribution;
- conversion from `ServerMessage` to ordered `SignalFishEvent` values.

The drivers own scheduling only. The async driver owns Tokio channels,
backpressured event delivery, its transport task, and shutdown timeout. The
polling driver owns its bounded queue, noop-waker polling, readiness timing,
pending transport sends, and close progress.

Never add protocol interpretation or state mutation directly to a driver. Add
it to `ClientCore`, then exercise it through both drivers in the parity matrix.

## Locking Rule

The async handle shares `ClientCore` with its transport task through a standard
mutex. Hold that lock only for short synchronous core calls. Release it before:

- transport polling or an `.await`;
- waiting for command-channel capacity;
- sending an event into the backpressured event channel.

The polling driver owns the core directly and must remain compatible with
non-`Send` transports.

## Common Public API

`SignalFishClientApi` is object-safe. Common synchronous commands take
`&mut self`; diagnostics and snapshots take `&self`. Trait methods use concrete
owned argument types. Do not add:

- generic methods or `impl Trait` arguments;
- async methods;
- associated transport types;
- `Send` or `Sync` supertraits;
- driver-private command/effect types in public signatures.

Waiting sends and `shutdown` remain async-driver-specific. `poll`, `close`, and
`is_closing` remain polling-driver-specific.

## Adding a Common Command

1. Add one `ClientOperation` variant.
2. Construct and validate its exact wire command in `ClientCore::prepare`.
3. Add matching mutable methods to `SignalFishClientApi` and both inherent
   clients; the driver methods only enqueue the prepared `CoreCommand`.
4. Add the operation to the data-driven parity matrix and verify exact frames,
   returned errors, queue capacity, snapshots, and statistics.
5. Update public docs and `CHANGELOG.md` when consumer-visible.

## Parity Testing

Use deterministic barriers around the async driver. Compare complete ordered
frames and events rather than selected fields. Cover:

- pre-negotiation, relay-floor, and v3 modes;
- JSON and binary representations;
- full queues, pending sends, disconnects, and close metadata;
- all violation policies and authoritative quarantine rebaselines;
- coherent snapshots and cumulative statistics.

`Connected` timing and driver lifecycle calls are intentionally different and
belong in driver-specific tests.
