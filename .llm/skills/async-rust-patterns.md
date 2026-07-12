# Async Rust Patterns

Practical reference for Tokio, polling transports, task spawning, selection,
and channels in this crate.

The crate's current MSRV is Rust 1.85.0.

## Runtime Setup

```rust
#[tokio::test]
async fn my_test() { /* ... */ }

#[tokio::main]
async fn main() { /* ... */ }
```

The async client requires a continuously driven Tokio runtime. Frame-driven
applications that do not drive one continuously use `SignalFishPollingClient`.

## Poll-Based Transport, Async Driver

`Transport` contains object-safe polling methods, not async trait methods:

```rust,ignore
pub trait Transport {
    fn poll_send(
        &mut self,
        cx: &mut Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> Poll<Result<(), SignalFishError>>;

    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<TransportFrame, SignalFishError>>>;

    fn poll_close(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), SignalFishError>>;
}
```

There is no trait-level `Send` bound and no `async-trait` usage. Internal async
adapters use `std::future::poll_fn`:

```rust,ignore
let received = std::future::poll_fn(|cx| transport.poll_recv(cx)).await;
```

`SignalFishClient::start` requires `T: Transport + Send + 'static` at the
spawn boundary. The polling client requires only `T: Transport`, allowing
main-thread-only transports.

When `poll_send` takes the caller's `Option<TransportFrame>`, the transport has
accepted ownership. It must retain that exact send across `Pending` until it
returns `Ready`; never restart it from a replacement frame. See
[Transport Abstraction](transport-abstraction.md).

## Task Spawning

```rust
let handle = tokio::spawn(async move {
    do_work().await
});
let result = handle.await?;
```

Everything held across an `.await` inside a spawned task must be `Send`.
Dropping a `JoinHandle` detaches rather than cancels; abort explicitly when the
task must stop.

## Transport Loop Shape

The async driver adapts poll methods to futures and multiplexes them with
commands and shutdown:

```rust,ignore
loop {
    tokio::select! {
        command = command_rx.recv() => { /* encode and send frame */ }
        incoming = std::future::poll_fn(|cx| transport.poll_recv(cx)) => {
            match incoming {
                Some(Ok(TransportFrame::Text(text))) => { /* JSON decode */ }
                Some(Ok(TransportFrame::Binary(bytes))) => { /* strict decode */ }
                Some(Err(error)) => { /* disconnect */ }
                None => { /* peer close */ }
            }
        }
        _ = &mut shutdown_rx => { /* drive poll_close */ }
    }
}
```

The transport object retains partial receive/send state, so cancellation of the
temporary `poll_fn` future must not discard accepted bytes or frames.

## `tokio::select!`

Branches are randomized by default. Use `biased;` only where priority is part
of the behavior, such as preferring an already-ready event delivery over a
simultaneous shutdown.

Do not place a waiting send future in a `select!` unless cancellation semantics
are explicitly safe. At the public client API level, cancellation before a
bounded command-channel send completes means the command was not queued.

## Channels

### Event channel

Events use bounded `mpsc` and `send().await`. A full channel backpressures the
transport loop; it is not a `try_send`-and-drop path. Shutdown may preempt one
blocked event as documented by the client.

```rust
let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(256);
event_tx.send(event).await?;
while let Some(event) = event_rx.recv().await { /* handle */ }
```

### Command channel

Synchronous commands use `try_send` and fail with `SendBufferFull` or
`NotConnected`. Waiting variants use `send().await` to pace callers.

```rust,ignore
match command_tx.try_send(command) {
    Ok(()) => Ok(()),
    Err(TrySendError::Full(_)) => Err(SignalFishError::SendBufferFull {
        capacity: command_tx.max_capacity(),
    }),
    Err(TrySendError::Closed(_)) => Err(SignalFishError::NotConnected),
}
```

Never introduce an unbounded command backlog or silent command drop.

## Wakers and Polling

A transport polled by the async driver receives a real runtime waker. If it
returns `Pending`, it must arrange for that waker to be woken when progress is
possible. Forwarding the waker into an underlying `Stream`/`Sink` poll usually
does this automatically.

The polling client uses a noop waker and calls the methods again on the next
game-loop tick. A transport may therefore support both models with the same
state machine.

Do not create and immediately drop a readiness future on every poll: dropping
it may unregister its waker. Retain the future/state across polls or poll the
underlying primitive directly.

## Timeouts and Shutdown

Keep a `JoinHandle` by mutable reference when timing it out:

```rust
let mut task = tokio::spawn(background_work());
if tokio::time::timeout(Duration::from_secs(1), &mut task)
    .await
    .is_err()
{
    task.abort();
    let _ = task.await;
}
```

Graceful transport shutdown is itself multi-poll. Internal code adapts
`poll_close` with `poll_fn` and awaits it until ready; the client timeout may
still abort a transport whose close never makes progress.

## Synchronization

Use `tokio::sync::Mutex` when holding a guard across `.await`. Use
`std::sync::Mutex` for short, non-awaiting critical sections, and atomics for
simple counters/flags. Never hold a blocking mutex guard across `.await`.

## Common Pitfalls

- Blocking the runtime thread with `std::thread::sleep` or blocking I/O.
- Putting `Send` on `Transport` itself and excluding valid polling backends.
- Taking an outbound frame, returning `Pending`, then forgetting it.
- Replaying `start_send` after `Pending`, duplicating a frame.
- Returning `Pending` under a real runtime without registering a waker.
- Treating binary application frames as ignorable WebSocket control frames.
- Starting close more than once or discarding pending close state.
- Assuming dropping a `JoinHandle` cancels its task.
