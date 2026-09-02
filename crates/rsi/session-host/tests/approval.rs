use rsi_agent_session_protocol::SessionId;
use rsi_approval_protocol::{ApprovalAnswerer, ApprovalDecision, ApprovalRequest, ApprovalSubject};
use rsi_session::SessionApprovalControl;
use rsi_session_host::ApprovalBroker;
use tokio_util::sync::CancellationToken;

fn request(id: &str) -> ApprovalRequest {
    ApprovalRequest {
        subject: ApprovalSubject::new("session-1", "turn-1", "effect-1").unwrap(),
        id: id.into(),
        action: "write file".into(),
        reason: "mutation requested".into(),
    }
}

async fn wait_pending(broker: &ApprovalBroker, count: usize) -> Vec<ApprovalRequest> {
    let session = SessionId::new("session-1").unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let pending = SessionApprovalControl::pending(broker, &session)
                .await
                .unwrap();
            if pending.len() == count {
                return pending;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("pending approval became visible")
}

#[tokio::test]
async fn pending_is_session_scoped_and_first_valid_client_answer_wins() {
    let broker = ApprovalBroker::new();
    let waiting = tokio::spawn({
        let broker = broker.clone();
        async move {
            ApprovalAnswerer::answer(&broker, request("approval-1"), CancellationToken::new()).await
        }
    });
    assert_eq!(wait_pending(&broker, 1).await, vec![request("approval-1")]);
    assert!(
        SessionApprovalControl::pending(&broker, &SessionId::new("session-2").unwrap())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !SessionApprovalControl::answer(
            &broker,
            &SessionId::new("session-2").unwrap(),
            "approval-1",
            ApprovalDecision::Deny,
        )
        .await
        .unwrap()
    );

    let session = SessionId::new("session-1").unwrap();
    assert!(
        SessionApprovalControl::answer(
            &broker,
            &session,
            "approval-1",
            ApprovalDecision::AllowOnce,
        )
        .await
        .unwrap()
    );
    assert!(
        !SessionApprovalControl::answer(&broker, &session, "approval-1", ApprovalDecision::Deny,)
            .await
            .unwrap()
    );
    let outcome = waiting.await.unwrap().unwrap().unwrap();
    assert_eq!(outcome.decision, ApprovalDecision::AllowOnce);
    assert_eq!(outcome.answerer, "rsi.session-host");
    assert!(wait_pending(&broker, 0).await.is_empty());
}

#[tokio::test]
async fn concurrent_answers_have_exactly_one_winner() {
    let broker = ApprovalBroker::new();
    let waiting = tokio::spawn({
        let broker = broker.clone();
        async move {
            ApprovalAnswerer::answer(
                &broker,
                request("approval-concurrent"),
                CancellationToken::new(),
            )
            .await
        }
    });
    wait_pending(&broker, 1).await;
    let session = SessionId::new("session-1").unwrap();
    let (allowed, denied) = tokio::join!(
        SessionApprovalControl::answer(
            &broker,
            &session,
            "approval-concurrent",
            ApprovalDecision::AllowOnce,
        ),
        SessionApprovalControl::answer(
            &broker,
            &session,
            "approval-concurrent",
            ApprovalDecision::Deny,
        ),
    );
    let allowed = allowed.unwrap();
    let denied = denied.unwrap();
    assert_ne!(allowed, denied, "exactly one concurrent answer must win");
    let outcome = waiting.await.unwrap().unwrap().unwrap();
    assert_eq!(
        outcome.decision,
        if allowed {
            ApprovalDecision::AllowOnce
        } else {
            ApprovalDecision::Deny
        }
    );
    assert!(wait_pending(&broker, 0).await.is_empty());
}

#[tokio::test]
async fn cancellation_and_host_stop_remove_live_requests_without_a_default_answer() {
    let broker = ApprovalBroker::new();
    let cancellation = CancellationToken::new();
    let cancelled = tokio::spawn({
        let broker = broker.clone();
        let cancellation = cancellation.clone();
        async move { ApprovalAnswerer::answer(&broker, request("cancelled"), cancellation).await }
    });
    wait_pending(&broker, 1).await;
    cancellation.cancel();
    assert!(cancelled.await.unwrap().is_err());
    assert!(wait_pending(&broker, 0).await.is_empty());

    let stopped = tokio::spawn({
        let broker = broker.clone();
        async move {
            ApprovalAnswerer::answer(&broker, request("stopped"), CancellationToken::new()).await
        }
    });
    wait_pending(&broker, 1).await;
    broker.stop();
    assert!(stopped.await.unwrap().is_err());
    assert!(
        SessionApprovalControl::pending(&broker, &SessionId::new("session-1").unwrap())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn dropping_the_waiter_removes_its_pending_request() {
    let broker = ApprovalBroker::new();
    let waiting = tokio::spawn({
        let broker = broker.clone();
        async move {
            ApprovalAnswerer::answer(&broker, request("dropped-waiter"), CancellationToken::new())
                .await
        }
    });
    wait_pending(&broker, 1).await;

    waiting.abort();
    assert!(waiting.await.unwrap_err().is_cancelled());
    assert!(wait_pending(&broker, 0).await.is_empty());

    let replacement_cancellation = CancellationToken::new();
    let replacement = tokio::spawn({
        let broker = broker.clone();
        let replacement_cancellation = replacement_cancellation.clone();
        async move {
            ApprovalAnswerer::answer(&broker, request("dropped-waiter"), replacement_cancellation)
                .await
        }
    });
    wait_pending(&broker, 1).await;
    replacement_cancellation.cancel();
    assert!(replacement.await.unwrap().is_err());
    assert!(wait_pending(&broker, 0).await.is_empty());
}
