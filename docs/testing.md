# Deterministic Testing

Integration tests for a multiplayer SDK fail in two ways that have nothing to
do with your code: a deadline that was *probably* long enough on the CI
runner, and a `sleep` that was *probably* long enough for the event you were
awaiting. This page explains which clocks the SDK actually reads and how to
remove wall-clock luck from your tests.

## The time model: who reads which clock

Every SDK deadline is derived from one of three clock sources, and each can
be made deterministic:

| Driver | Clock source | How to control it |
|---|---|---|
| [`SignalFishClient`](client.md#signalfishclient) (async) | [`tokio::time`](https://docs.rs/tokio/latest/tokio/time/) only — `shutdown_timeout` and every internal deadline are tokio timers | `#[tokio::test(start_paused = true)]` or `tokio::time::pause()` virtualizes **every** SDK deadline, with no SDK configuration |
| [`SignalFishPollingClient`](client.md#signalfishpollingclient) | `std::time::Instant`, sampled only inside your `poll()` / `close()` calls | The cadence is already yours; deadlines inside `close()` are bounded by `SignalFishConfig::shutdown_timeout` — use a tiny (or `Duration::ZERO`) budget for determinism |
| Godot adapter | Samples per `begin_poll_cycle` (one per polling-client `poll()` call) | Inject the cadence: call `poll()` from a fixed-point in your game loop rather than on a timer |

The practical consequence for the async driver: the SDK never reads the wall
clock directly, so a paused tokio clock freezes the entire driver. A
`shutdown_timeout` of 5 seconds elapses in zero real milliseconds the moment
you `tokio::time::advance(Duration::from_secs(5)).await`.

!!! note "Enable tokio's test-util feature"
    `start_paused` lives behind tokio's `test-util` feature, which is for
    test builds only. Add it to your dev-dependencies:
    `tokio = { version = "1", features = ["full", "test-util"] }`.

## Recipe: mock transport + paused time

Transport implementations are channel-driven objects, so they work unchanged
under a paused clock. The example below proves the shutdown deadline end to
end: the graceful close can never complete, so teardown can only come from
the budget expiry aborting the transport. The test completes in microseconds
of real time while asserting exact virtual-time behavior — and a regression
that turns the abort into a hang fails the exact-elapsed assertion (via the
shutdown watchdog) instead of passing vacuously.

```rust
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use signal_fish_client::{
    SignalFishClient, SignalFishConfig, SignalFishError, Transport, TransportFrame,
};

/// A scripted transport whose graceful close never completes: teardown can
/// only come from the shutdown budget expiring and aborting the transport.
struct HangingCloseTransport {
    incoming: VecDeque<Result<TransportFrame, SignalFishError>>,
    aborted: Arc<AtomicBool>,
}

impl Transport for HangingCloseTransport {
    fn poll_send(
        &mut self,
        _cx: &mut Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> Poll<Result<(), SignalFishError>> {
        frame.take(); // backend ownership transfers here
        Poll::Ready(Ok(()))
    }

    fn poll_recv(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<TransportFrame, SignalFishError>>> {
        match self.incoming.pop_front() {
            Some(result) => Poll::Ready(Some(result)),
            None => Poll::Pending,
        }
    }

    fn poll_close(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), SignalFishError>> {
        Poll::Pending // graceful close never completes
    }

    fn abort(&mut self) {
        self.aborted.store(true, Ordering::Relaxed);
    }
}

#[tokio::test(start_paused = true)]
async fn shutdown_deadline_is_fully_virtual() {
    let aborted = Arc::new(AtomicBool::new(false));
    let transport = HangingCloseTransport {
        incoming: VecDeque::from(vec![]),
        aborted: Arc::clone(&aborted),
    };
    let config = SignalFishConfig::new("mb_app_abc123")
        .with_shutdown_timeout(std::time::Duration::from_millis(250));
    let (mut client, _events) = SignalFishClient::start(transport, config);

    let started = tokio::time::Instant::now();
    client.shutdown().await;

    // Tokio auto-advanced the paused clock to the budget deadline while the
    // close was parked: virtual elapsed is exactly the 250ms budget, with
    // zero real time spent sleeping.
    let elapsed = started.elapsed();
    assert_eq!(
        elapsed,
        std::time::Duration::from_millis(250),
        "shutdown must resolve at the virtual budget deadline"
    );
    // The expiry (not a graceful close) ended the shutdown: the budget
    // aborted the transport.
    assert!(aborted.load(Ordering::Relaxed));
}
```

Three rules keep this honest:

- **Waiting is `advance`, never `sleep`.** Under paused time a plain
  `tokio::time::sleep` resolves only via tokio's *auto-advance*, which fires
  when every task on the runtime is parked. That makes `sleep(ms)` a
  deterministic "wait until the driver parks" primitive — but if you need
  time to move while your test task could still run, drive it explicitly
  with `tokio::time::advance(d).await` (the advancer-task pattern:
  `tokio::spawn` a task that sleeps and then lets the budget fire, so a fast
  path wins the race and a slow path hits the deterministically-advanced
  deadline).
- **Mocks must not read the wall clock.** Script responses through channels
  or `std::sync::atomic` flags, and have `poll_recv` return
  `Poll::Pending` with no waker rather than sleeping. A mock that uses
  `std::thread::sleep` or `std::time::Instant` deadlines drags real time
  back into a paused test.
- **Real I/O stays on the real clock.** The built-in
  [`WebSocketTransport`](transport.md) performs actual TCP/TLS work; a
  paused clock does not freeze the network, so a connect deadline could fire
  while real I/O is still in flight. Keep end-to-end tests against a live
  server on real time and use generous, direction-safe budgets (a sleep that
  strictly exceeds the deadline it must outlive), or put a mock behind
  `SignalFishClient::start` when you need virtual time.

## Recipe: polling-client cadence control

`SignalFishPollingClient` has no background task and no timer of its own:
time only moves when you call `poll()`. That makes your test the clock.

```rust,ignore
use std::time::Duration;

use signal_fish_client::{SignalFishConfig, SignalFishPollingClient};

// A zero close budget makes teardown synchronous and exact: the close can
// never linger on a std-time deadline.
let mut client = SignalFishPollingClient::new(
    transport,
    SignalFishConfig::new("mb_app_abc123")
        .with_shutdown_timeout(Duration::ZERO),
);

// One poll per game tick: the cadence is the test's, not the SDK's.
let events = client.poll();
for event in events {
    // handle event
}

client.close();
```

Details that matter for deterministic polling tests:

- **`close()` emits no events.** Completion is observed through
  [`snapshot()`](client.md#signalfishpollingclient), which is coherent the
  moment `close()` returns.
- **The default close policy abandons client-owned queued work.** Flush is
  an explicit opt-in (`PollingClosePolicy::Flush`); with a
  `Duration::ZERO`-bounded budget the close is synchronous and exact.
- **The Godot adapter samples per `begin_poll_cycle`.** One `poll()` call is
  one scheduling cycle; tests inject cadence by calling `poll()` in a fixed
  loop rather than sleeping between calls. See
  [WebAssembly (WASM)](wasm.md).

## Why there is no injectable Clock API

We evaluated a `ClockFn`-style abstraction (as used by rollback netcode
frameworks such as fortress-rollback) and deliberately did not build one.
The async driver reads **only** `tokio::time`, which tokio itself lets you
pause and advance; the polling driver is caller-driven, so its clock is your
game loop. Together those two properties already virtualize every production
clock read, and a public clock trait would add API surface that downstream
users do not need — anyone requiring virtual time gets it from tokio with
zero SDK coupling. If you need `std`-clock injection on the polling driver
(for example, to simulate queue-age aging without calling `poll()`), please
[open an issue](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues).
