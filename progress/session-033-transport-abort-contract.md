# Session 033 — Enforceable Transport Abandonment

## Scope and Priority

Issue #106 was the highest gameplay-impacting actionable issue after PR #115
merged issue #104. The hosted audit found no open/draft/dependency PR. The only
other agent branch is a five-commit, 30-commit-behind standalone Fortress
experiment superseded by the merged session-018 Godot browser suite; issue #82
retains it as later performance research input. Issue #90 remains an
administrator-owned governance blocker.

## Contract Decision

`Transport::abort` is required for the forthcoming breaking 0.11 release. It
must promptly, without blocking or panicking, release or safely detach backend
work, discard retained accepted sends and wakers, revoke shared-backend
registrations, and be idempotent. Completed cleanup is not repeated; failed
external cleanup can retry safely, and callback storage must remain live if
freeing it would risk use-after-free. After abort, client drivers perform no
more poll-cycle/send/receive/close calls. Only repeated abort, `is_ready`,
`close_info`, `diagnostics`, and destruction remain valid. Resource-free
stateless transports may implement an explicit no-op; built-in transports
additionally fuse later polling to terminal results.

Graceful termination still finishes a backend-owned send before close under
one configured deadline. Deadline expiry or a close error invokes abort and
does not imply send completion or peer delivery.

## Ownership Enforcement

- The async transport task wraps its backend in an armed ownership guard.
  Graceful close success disarms it; explicit abort disarms before delegating,
  while task cancellation, panic, watchdog cancellation, or handle drop invokes
  backend abort from the guard. The guard is armed synchronously before spawn,
  so cancellation before the task's first poll is also covered.
- The polling client aborts on deadline expiry and close error, aborts from
  `Drop` unless graceful close completed, and returns from a closed poll before
  even `begin_poll_cycle` can touch the backend.
- Native WebSocket, Emscripten, and Godot implementations already own explicit
  abort paths. Native and Godot tests now prove idempotency and their stronger
  fused post-abort behavior.

## Invariant-to-Evidence Matrix

| Invariant | Source | Executable evidence |
| --- | --- | --- |
| Every implementor makes an explicit abort decision. | Required `Transport::abort`; all repository implementations and examples migrated. | Workspace all-target/all-feature compilation and Clippy. |
| Accepted backend work precedes graceful close but is abandoned at the deadline. | Async `finish_send_and_close_bounded`; polling `ClosePhase::Flushing`. | Custom hanging-resource transport in accepted-send and close modes for both drivers; existing deterministic polling close models. |
| Cancellation/drop cannot bypass backend abandonment. | Async `AbortOnDropTransport`; polling `Drop`. | Async before-first-poll and active handle-drop plus polling owner-drop tests with shared resource evidence. |
| Close failure immediately invokes abort and terminates logical I/O. | Both drivers abort on `poll_close` error. | Async/polling `CloseError` cases assert one abort and zero deadline count for polling. |
| No work resumes after abort. | Async guard disarm; polling closed early return. | Probe counts every post-abort transport poll; repeated shutdown/poll stays at zero activity and the async producer channel is disconnected. |
| Built-in transports remain compliant. | Native stream drop/fused state; Godot one-shot force-close; Emscripten authorized cleanup/safe-leak policy. | Native peer disconnect plus repeated abort/fused polls; Godot fake backend abort-once/fused polls; Emscripten source policy proves abort closes before guarded deletion and preserves state on failure. |

## Documentation and Migration

The public trait docs, transport/client/concepts/WASM guides, changelog,
canonical context, focused skills, and channel/mesh examples describe the
breaking requirement and allowed calls. The migration guide distinguishes
resource-owning cleanup from a valid explicit no-op and warns that abort must
not wait for a peer handshake.

## Verification

The focused contract suite passes for async and polling accepted send, close
hang, close error, and owner-drop paths. A direct unit test proves the
configured inner deadline calls abort before the async guard is dropped, so the
outer task watchdog cannot mask a regression. The mandatory workspace workflow,
quick all-configuration check, documentation/policy validators, strict MkDocs
build, and three independent adversarial review tracks pass locally. Hosted
PR #116's first run reached ten green aggregates, then exposed one
configuration-specific documentation defect: the new WebSocket example was
compiled without its feature in the no-default-feature doctest lane. The
example now has an item-level `transport-websocket` gate, and both exact
no-default workspace tests and all-feature doctests pass locally. Final hosted
checks on corrected head `4559905` are all green: CI `32426236185`, Coverage
`32426236161`, Docs Validation `32426236202`, Examples Validation
`32426236163`, Godot Web `32426236239`, No Panics `32426236208`, Security
`32426236191`, Semver Checks `32426236192`, Unused Deps `32426236171`, WASM
`32426236198`, and Workflow Lint `32426236233`. PR #116 has zero inline review
threads or actionable reviewer findings. Copilot's quota-exhausted comment is
the known repository-administration blocker tracked in issue #90.
