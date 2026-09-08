use rsi_service_host::QuestionBroker;
use rsi_user_questions_protocol::{
    Question, QuestionAnswer, QuestionError, QuestionRequest, UserQuestions,
};
use tokio_util::sync::CancellationToken;

fn request(id: &str) -> QuestionRequest {
    QuestionRequest {
        id: id.into(),
        session_id: "root".into(),
        turn_id: "turn".into(),
        questions: vec![Question {
            id: "choice".into(),
            prompt: "Choose a name".into(),
            options: vec!["one".into(), "two".into()],
        }],
    }
}
fn answer(text: &str) -> QuestionAnswer {
    QuestionAnswer {
        answers: vec![text.into()],
    }
}

#[tokio::test]
async fn dropping_an_old_settled_waiter_cannot_remove_a_reused_identity() {
    let broker = QuestionBroker::default();
    let mut original = Box::pin(broker.ask(request("reused"), CancellationToken::new()));
    assert!(futures_util::poll!(&mut original).is_pending());
    assert!(
        broker
            .answer("root", "reused", answer("old"))
            .await
            .unwrap()
    );
    for index in 0..256 {
        let id = format!("receipt-{index}");
        let mut waiter = Box::pin(broker.ask(request(&id), CancellationToken::new()));
        assert!(futures_util::poll!(&mut waiter).is_pending());
        assert!(
            broker
                .answer("root", &id, answer("eviction"))
                .await
                .unwrap()
        );
        waiter.await.unwrap();
    }
    let mut replacement = Box::pin(broker.ask(request("reused"), CancellationToken::new()));
    assert!(futures_util::poll!(&mut replacement).is_pending());
    drop(original);
    assert_eq!(broker.pending("root").await.unwrap(), [request("reused")]);
    assert!(
        broker
            .answer("root", "reused", answer("new"))
            .await
            .unwrap()
    );
    assert_eq!(replacement.await.unwrap(), answer("new"));
}
async fn pending(broker: &QuestionBroker, count: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while broker.pending("root").await.unwrap().len() != count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn same_host_reconnect_replays_exact_request_and_receipts_are_idempotent() {
    let broker = QuestionBroker::default();
    let waiting = tokio::spawn({
        let broker = broker.clone();
        async move {
            broker
                .ask(request("question"), CancellationToken::new())
                .await
        }
    });
    pending(&broker, 1).await;
    tokio::time::advance(std::time::Duration::from_hours(24)).await;
    assert!(!waiting.is_finished());
    assert_eq!(
        broker.clone().pending("root").await.unwrap(),
        [request("question")]
    );
    assert!(broker.pending("other").await.unwrap().is_empty());
    assert!(
        !broker
            .answer("other", "question", answer("free text"))
            .await
            .unwrap()
    );
    assert!(
        broker
            .answer(
                "root",
                "question",
                QuestionAnswer {
                    answers: vec!["a".into(), "b".into()]
                }
            )
            .await
            .is_err()
    );
    assert_eq!(broker.pending("root").await.unwrap().len(), 1);
    assert!(
        broker
            .answer("root", "question", answer("free text"))
            .await
            .unwrap()
    );
    assert!(
        broker
            .answer("root", "question", answer("free text"))
            .await
            .unwrap()
    );
    assert_eq!(
        broker.answer("root", "question", answer("different")).await,
        Err(QuestionError::Conflict)
    );
    assert_eq!(waiting.await.unwrap().unwrap(), answer("free text"));
    assert!(
        !QuestionBroker::default()
            .answer("root", "question", answer("free text"))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn cancellation_drop_and_shutdown_remove_waiters_without_answers() {
    for action in ["cancel", "drop", "stop"] {
        let broker = QuestionBroker::default();
        let cancellation = CancellationToken::new();
        let waiting = tokio::spawn({
            let broker = broker.clone();
            let cancellation = cancellation.clone();
            async move { broker.ask(request("q"), cancellation).await }
        });
        pending(&broker, 1).await;
        match action {
            "cancel" => cancellation.cancel(),
            "drop" => waiting.abort(),
            "stop" => broker.stop(),
            _ => unreachable!(),
        }
        assert!(!matches!(waiting.await, Ok(Ok(_))));
        if action == "stop" {
            assert_eq!(broker.pending("root").await, Err(QuestionError::Cancelled));
        } else {
            pending(&broker, 0).await;
            assert!(!broker.answer("root", "q", answer("late")).await.unwrap());
        }
    }
}

#[tokio::test]
async fn concurrent_answers_and_input_bounds_are_enforced_before_settlement() {
    let broker = QuestionBroker::default();
    let mut oversized = request("bad");
    oversized.questions[0].prompt = "x".repeat(65_536);
    assert!(
        broker
            .ask(oversized, CancellationToken::new())
            .await
            .is_err()
    );
    let mut duplicate = request("bad");
    duplicate.questions.push(duplicate.questions[0].clone());
    assert!(
        broker
            .ask(duplicate, CancellationToken::new())
            .await
            .is_err()
    );
    let waiting = tokio::spawn({
        let broker = broker.clone();
        async move { broker.ask(request("race"), CancellationToken::new()).await }
    });
    pending(&broker, 1).await;
    let (left, right) = tokio::join!(
        broker.answer("root", "race", answer("left")),
        broker.answer("root", "race", answer("right"))
    );
    assert_ne!(left.is_ok(), right.is_ok());
    assert_eq!(
        waiting.await.unwrap().unwrap(),
        if left.is_ok() {
            answer("left")
        } else {
            answer("right")
        }
    );
}

#[tokio::test]
async fn live_requests_and_settled_receipts_have_separate_hard_bounds() {
    let broker = QuestionBroker::default();
    let mut waits = Vec::new();
    for index in 0..256 {
        let broker = broker.clone();
        waits.push(tokio::spawn(async move {
            broker
                .ask(request(&format!("q-{index}")), CancellationToken::new())
                .await
        }));
    }
    pending(&broker, 256).await;
    assert_eq!(
        broker
            .ask(request("excess"), CancellationToken::new())
            .await,
        Err(QuestionError::Capacity)
    );
    for index in 0..256 {
        assert!(
            broker
                .answer("root", &format!("q-{index}"), answer("yes"))
                .await
                .unwrap()
        );
    }
    for wait in waits {
        assert_eq!(wait.await.unwrap().unwrap(), answer("yes"));
    }
    let last = tokio::spawn({
        let broker = broker.clone();
        async move { broker.ask(request("last"), CancellationToken::new()).await }
    });
    pending(&broker, 1).await;
    assert!(broker.answer("root", "last", answer("yes")).await.unwrap());
    last.await.unwrap().unwrap();
    assert!(!broker.answer("root", "q-0", answer("yes")).await.unwrap());
    assert!(broker.answer("root", "q-1", answer("yes")).await.unwrap());
}
