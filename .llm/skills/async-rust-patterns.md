# Async Rust Patterns

Practical reference for tokio, async/await, task spawning, select!, and channels as used in this codebase.

## Tokio Runtime Setup

```rust
// In tests — use the macro
#[tokio::test]
async fn my_test() { /* ... */ }

// Multi-threaded test
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn my_mt_test() { /* ... */ }

// In binaries
#[tokio::main]
async fn main() { /* ... */ }
```

## Async Trait Methods

MSRV is 1.75.0. AFIT (async fn in trait) is stabilized but `async-trait` is
still used here for object safety and compatibility:

```rust
use async_trait::async_trait;

#[async_trait]
pub trait Transport: Send + 'static {
    async fn send(&mut self, message: String) -> Result<(), SignalFishError>;
    async fn recv(&mut self) -> Option<Result<String, SignalFishError>>;
    async fn close(&mut self) -> Result<(), SignalFishError>;
}

#[async_trait]
impl Transport for MyTransport {
    async fn send(&mut self, message: String) -> Result<(), SignalFishError> {
        // implementation
    }
    // ...
}
```

Note: `async_trait` boxes the futures. For I/O-bound transports this overhead
is negligible.

## Task Spawning

```rust
// Spawn a task — must be 'static + Send
let handle = tokio::spawn(async move {
    // captured variables must be Send
    do_work().await
});

// Await the result
let result = handle.await?; // JoinError if panicked

// Detach (fire-and-forget) — store handle or it will be cancelled on drop
let _handle = tokio::spawn(background_task());
```

### Transport Loop in SignalFishClient

`SignalFishClient::start` spawns a background transport loop that multiplexes
outgoing commands and incoming server messages:

```rust
// Simplified from src/client.rs
loop {
    tokio::select! {
        cmd = cmd_rx.recv() => {
            match cmd {
                Some(msg) => {
                    let json = serde_json::to_string(&msg)?;
                    transport.send(json).await?;
                }
                None => { transport.close().await?; break; }
            }
        }
        _ = &mut shutdown_rx => {
            transport.close().await?;
            break;
        }
        incoming = transport.recv() => {
            match incoming {
                Some(Ok(text)) => { /* deserialize and emit event */ }
                Some(Err(e)) => { /* emit Disconnected and break */ break; }
                None => { /* clean close, emit Disconnected */ break; }
            }
        }
    }
}
```

`transport.recv()` MUST be cancel-safe because `select!` may cancel it.

## tokio::select!

Use `select!` to race multiple async operations:

```rust
tokio::select! {
    incoming = transport.recv() => {
        match incoming {
            Some(Ok(text)) => handle_message(text).await,
            Some(Err(e)) => return Err(e),
            None => break, // clean close
        }
    }
    _ = shutdown_rx => {
        transport.close().await?;
        break;
    }
    _ = tokio::time::sleep(Duration::from_secs(30)) => {
        client.ping()?;
    }
}
```

Important: branches are polled in random order by default. Use `biased;` at
the top to poll top-to-bottom (useful for priority).

## Channels

### mpsc — multiple producer, single consumer

```rust
use tokio::sync::mpsc;

let (tx, mut rx) = mpsc::channel::<SignalFishEvent>(256); // buffer size

// Sender (cloneable, can move into tasks)
tx.send(event).await?;       // async, back-pressures when full
tx.try_send(event)?;         // non-blocking, returns Err if full

// Receiver
while let Some(event) = rx.recv().await { /* ... */ }
```

### oneshot — single message

```rust
use tokio::sync::oneshot;

let (tx, rx) = oneshot::channel::<()>();
tokio::spawn(async move { tx.send(()).ok(); });
rx.await?; // RecvError if sender dropped
```

### unbounded — for command channels

```rust
// SignalFishClient uses unbounded for client→transport commands
let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<ClientMessage>();
cmd_tx.send(msg).ok(); // never blocks — used in sync client methods
```

## Timeouts

```rust
use tokio::time::{timeout, Duration};

match timeout(Duration::from_secs(5), transport.recv()).await {
    Ok(Some(Ok(msg))) => handle(msg),
    Ok(Some(Err(e))) => return Err(e),
    Ok(None) => { /* closed */ }
    Err(_elapsed) => return Err(SignalFishError::Timeout),
}
```

## Mutex in Async

Use `tokio::sync::Mutex` when holding the guard across `.await`:

```rust
use tokio::sync::Mutex;
use std::sync::Arc;

let shared = Arc::new(Mutex::new(State::default()));

// In async context
let mut guard = shared.lock().await;
guard.do_thing();
// guard released at end of scope
```

Use `std::sync::Mutex` only when the critical section has no `.await` calls.
Use `std::sync::atomic::AtomicBool` for simple boolean flags (used for
`connected` and `authenticated` in `ClientState`).

## Common Pitfalls

- **Don't block in async**: avoid `std::thread::sleep`, blocking file I/O,
  or CPU-heavy loops without `spawn_blocking`
- **`Send` bounds**: types held across `.await` must be `Send` if using
  multi-threaded runtime
- **Cancellation safety**: if a future is dropped mid-await, partial work is
  lost. `Transport::recv` must be cancel-safe.
- **JoinHandle drops**: dropping a `JoinHandle` does NOT cancel the task.
  `SignalFishClient::drop` calls `task.abort()` explicitly.

```rust
// Graceful shutdown pattern used in SignalFishClient
let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

// Signal shutdown
shutdown_tx.send(()).ok();

// Background loop exits via select! branch:
_ = &mut shutdown_rx => {
    transport.close().await?;
    break;
}
```
