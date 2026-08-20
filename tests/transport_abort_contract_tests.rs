#![cfg(all(feature = "tokio-runtime", feature = "polling-client"))]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use signal_fish_client::{
    SignalFishClient, SignalFishConfig, SignalFishError, SignalFishEvent, SignalFishPollingClient,
    Transport, TransportFrame,
};
use tokio::sync::mpsc::error::TryRecvError;

#[derive(Clone, Copy, Debug)]
enum HangPoint {
    AcceptedSend,
    Close,
    CloseError,
}

#[derive(Default)]
struct AbortEvidence {
    accepted_sends: AtomicUsize,
    completed_sends: AtomicUsize,
    close_polls: AtomicUsize,
    abort_calls: AtomicUsize,
    backend_activity: AtomicUsize,
    post_abort_polls: AtomicUsize,
    resource_live: AtomicBool,
    dropped: AtomicBool,
}

struct HangingResourceTransport {
    hang_point: HangPoint,
    retained: Option<TransportFrame>,
    aborted: bool,
    evidence: Arc<AbortEvidence>,
}

impl HangingResourceTransport {
    fn new(hang_point: HangPoint) -> (Self, Arc<AbortEvidence>) {
        let evidence = Arc::new(AbortEvidence {
            resource_live: AtomicBool::new(true),
            ..AbortEvidence::default()
        });
        (
            Self {
                hang_point,
                retained: None,
                aborted: false,
                evidence: Arc::clone(&evidence),
            },
            evidence,
        )
    }
}

impl Drop for HangingResourceTransport {
    fn drop(&mut self) {
        self.evidence.dropped.store(true, Ordering::Release);
    }
}

impl Transport for HangingResourceTransport {
    fn begin_poll_cycle(&mut self) {
        if self.aborted {
            self.evidence
                .post_abort_polls
                .fetch_add(1, Ordering::AcqRel);
        }
    }

    fn poll_send(
        &mut self,
        _cx: &mut Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> Poll<Result<(), SignalFishError>> {
        if self.aborted {
            self.evidence
                .post_abort_polls
                .fetch_add(1, Ordering::AcqRel);
            return Poll::Ready(Err(SignalFishError::TransportClosed));
        }
        if self.retained.is_some() {
            return Poll::Pending;
        }
        let Some(accepted) = frame.take() else {
            return Poll::Ready(Ok(()));
        };
        self.evidence.accepted_sends.fetch_add(1, Ordering::AcqRel);
        self.evidence
            .backend_activity
            .fetch_add(1, Ordering::AcqRel);
        if matches!(self.hang_point, HangPoint::AcceptedSend) {
            self.retained = Some(accepted);
            Poll::Pending
        } else {
            self.evidence.completed_sends.fetch_add(1, Ordering::AcqRel);
            Poll::Ready(Ok(()))
        }
    }

    fn poll_recv(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<TransportFrame, SignalFishError>>> {
        if self.aborted {
            self.evidence
                .post_abort_polls
                .fetch_add(1, Ordering::AcqRel);
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    }

    fn poll_close(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), SignalFishError>> {
        if self.aborted {
            self.evidence
                .post_abort_polls
                .fetch_add(1, Ordering::AcqRel);
            return Poll::Ready(Ok(()));
        }
        self.evidence.close_polls.fetch_add(1, Ordering::AcqRel);
        self.evidence
            .backend_activity
            .fetch_add(1, Ordering::AcqRel);
        if matches!(self.hang_point, HangPoint::CloseError) {
            Poll::Ready(Err(SignalFishError::TransportSend(
                "scripted close failure".into(),
            )))
        } else {
            Poll::Pending
        }
    }

    fn abort(&mut self) {
        if self.aborted {
            return;
        }
        self.aborted = true;
        self.retained = None;
        self.evidence.abort_calls.fetch_add(1, Ordering::AcqRel);
        self.evidence.resource_live.store(false, Ordering::Release);
    }
}

fn assert_abandoned(
    evidence: &AbortEvidence,
    hang_point: HangPoint,
    expected_accepted_sends: usize,
) {
    assert_eq!(
        evidence.abort_calls.load(Ordering::Acquire),
        1,
        "abort must run exactly once for {hang_point:?}"
    );
    assert!(
        !evidence.resource_live.load(Ordering::Acquire),
        "abort must release the backend resource for {hang_point:?}"
    );
    assert_eq!(
        evidence.accepted_sends.load(Ordering::Acquire),
        expected_accepted_sends,
        "the expected outbound frame must reach the transport for {hang_point:?}"
    );
    assert_eq!(
        evidence.completed_sends.load(Ordering::Acquire),
        usize::from(matches!(
            hang_point,
            HangPoint::Close | HangPoint::CloseError
        )),
        "an accepted hanging send must not complete during abandonment"
    );
    assert_eq!(
        evidence.close_polls.load(Ordering::Acquire) > 0,
        matches!(hang_point, HangPoint::Close | HangPoint::CloseError),
        "graceful close is reached only after backend-owned sends complete"
    );
    assert_eq!(
        evidence.post_abort_polls.load(Ordering::Acquire),
        0,
        "the client driver must never poll after abort"
    );
}

#[tokio::test]
async fn async_deadline_abandons_accepted_send_and_hanging_close() {
    for hang_point in [
        HangPoint::AcceptedSend,
        HangPoint::Close,
        HangPoint::CloseError,
    ] {
        let (transport, evidence) = HangingResourceTransport::new(hang_point);
        let config = SignalFishConfig::new("abort-contract")
            .with_shutdown_timeout(Duration::from_millis(10));
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Connected)
        ));
        for _ in 0..100 {
            if evidence.accepted_sends.load(Ordering::Acquire) == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(evidence.accepted_sends.load(Ordering::Acquire), 1);
        client.shutdown().await;

        assert_abandoned(&evidence, hang_point, 1);
        assert!(
            evidence.dropped.load(Ordering::Acquire),
            "the async task must drop the abandoned transport"
        );
        let activity_after_shutdown = evidence.backend_activity.load(Ordering::Acquire);
        client.shutdown().await;
        tokio::task::yield_now().await;
        assert_eq!(
            evidence.backend_activity.load(Ordering::Acquire),
            activity_after_shutdown,
            "no backend activity may resume after async abandonment"
        );
        assert_eq!(evidence.post_abort_polls.load(Ordering::Acquire), 0);

        let remaining: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        assert!(remaining.len() <= 1, "only the terminal event may remain");
        assert!(remaining
            .iter()
            .all(|event| matches!(event, SignalFishEvent::Disconnected { .. })));
        assert!(
            matches!(events.try_recv(), Err(TryRecvError::Disconnected)),
            "the event producer must terminate after abandonment"
        );
    }
}

#[test]
fn polling_deadline_abandons_accepted_send_and_hanging_close() {
    for hang_point in [
        HangPoint::AcceptedSend,
        HangPoint::Close,
        HangPoint::CloseError,
    ] {
        let (transport, evidence) = HangingResourceTransport::new(hang_point);
        let config =
            SignalFishConfig::new("abort-contract").with_shutdown_timeout(Duration::from_millis(5));
        let mut client = SignalFishPollingClient::new(transport, config);

        let events = client.poll();
        assert!(events
            .iter()
            .any(|event| matches!(event, SignalFishEvent::Connected)));
        client.close();
        if matches!(hang_point, HangPoint::CloseError) {
            assert!(!client.is_closing());
        } else {
            assert!(client.is_closing());
            std::thread::sleep(Duration::from_millis(10));
            assert!(client.poll().is_empty());
        }

        assert_abandoned(&evidence, hang_point, 1);
        assert_eq!(
            client.polling_stats().close_deadline_expirations,
            u64::from(!matches!(hang_point, HangPoint::CloseError))
        );
        let activity_after_deadline = evidence.backend_activity.load(Ordering::Acquire);
        assert!(client.poll().is_empty());
        assert_eq!(
            evidence.backend_activity.load(Ordering::Acquire),
            activity_after_deadline,
            "no backend activity may resume after polling abandonment"
        );
        assert_eq!(evidence.post_abort_polls.load(Ordering::Acquire), 0);
        assert!(!client.is_closing());
    }
}

#[tokio::test]
async fn dropping_either_client_aborts_its_owned_transport() {
    let (unpolled_transport, unpolled_evidence) =
        HangingResourceTransport::new(HangPoint::AcceptedSend);
    let (unpolled_client, _events) = SignalFishClient::start(
        unpolled_transport,
        SignalFishConfig::new("unpolled-async-drop-contract"),
    );
    drop(unpolled_client);
    tokio::task::yield_now().await;
    assert_abandoned(&unpolled_evidence, HangPoint::AcceptedSend, 0);
    assert!(unpolled_evidence.dropped.load(Ordering::Acquire));

    let (async_transport, async_evidence) = HangingResourceTransport::new(HangPoint::AcceptedSend);
    let (async_client, mut events) = SignalFishClient::start(
        async_transport,
        SignalFishConfig::new("async-drop-contract"),
    );
    assert!(matches!(
        events.recv().await,
        Some(SignalFishEvent::Connected)
    ));
    drop(async_client);
    let dropped = tokio::time::timeout(Duration::from_secs(1), async {
        while !async_evidence.dropped.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        dropped.is_ok(),
        "aborted async task must promptly drop its transport guard"
    );
    assert_abandoned(&async_evidence, HangPoint::AcceptedSend, 1);

    let (polling_transport, polling_evidence) =
        HangingResourceTransport::new(HangPoint::AcceptedSend);
    let mut polling_client = SignalFishPollingClient::new(
        polling_transport,
        SignalFishConfig::new("polling-drop-contract"),
    );
    let _ = polling_client.poll();
    drop(polling_client);
    assert_abandoned(&polling_evidence, HangPoint::AcceptedSend, 1);
    assert!(polling_evidence.dropped.load(Ordering::Acquire));
}
