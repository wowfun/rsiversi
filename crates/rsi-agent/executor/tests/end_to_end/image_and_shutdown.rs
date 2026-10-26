use super::*;

#[tokio::test]
async fn image_prepare_cancellation_releases_the_only_execution_lane() {
    for release_with_cancel in [false, true] {
        let stack = BaseStack::activate().await;
        let entered = Arc::new(Notify::new());
        let release = CancellationToken::new();
        *stack.image.prepare_gate.lock().unwrap() = Some((entered.clone(), release.clone()));
        let language = Arc::new(LanguageFixture {
            outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(answer_script())])),
            requests: Mutex::new(vec![]),
            starts: Arc::new(AtomicUsize::new(0)),
            store: stack.store.clone(),
            retry_policy: RetryPolicy::default(),
        });
        let language_fiber = stack
            .activate_language("test.language.image-cancel", language.clone())
            .await;
        let executor_fiber = stack
            .activate_executor_with_config(
                json!({"executor_id":"image-prepare-cancel", "maximum_active_turns":1}),
            )
            .await;
        let turns = stack
            .runtime
            .root()
            .lookup_local::<TurnServiceContract>()
            .unwrap();
        let submitted = turns
            .submit_image(SubmitImage {
                turn_id: client_turn_id(),
                session: stack.fresh(header()).await,
                model: ModelRef::new("deployment", "image-model").unwrap(),
                request: ImageRequest::new("pending preparation", 1).unwrap(),
            })
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        turns
            .cancel(&submitted.session_id, &submitted.turn_id, None)
            .await
            .unwrap();
        if release_with_cancel {
            release.cancel();
        }
        assert_eq!(
            wait_for_outcome(&turns, &submitted).await,
            TurnOutcome::Cancelled
        );
        let next = stack
            .submit_fresh(&turns, "after-image-cancel", "answer")
            .await;
        assert_eq!(
            wait_for_outcome(&turns, &next).await,
            TurnOutcome::Completed
        );
        assert_eq!(language.starts.load(Ordering::SeqCst), 1);
        release.cancel();
        tokio::task::yield_now().await;
        assert_eq!(stack.image.starts.load(Ordering::SeqCst), 0);
        let page = stack
            .store
            .read_facts(
                &submitted.session_id,
                0,
                64,
                rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
            )
            .await
            .unwrap();
        assert!(!page.facts.iter().any(|fact| matches!(
            fact.body(),
            SessionFactBody::ImageIntent { .. } | SessionFactBody::ImageStarted { .. }
        )));
        drop(turns);
        stack.dispose(language_fiber, executor_fiber).await;
    }
}

#[tokio::test]
async fn host_stop_during_image_prepare_never_starts_or_publishes_an_intent() {
    let stack = BaseStack::activate().await;
    let entered = Arc::new(Notify::new());
    let release = CancellationToken::new();
    *stack.image.prepare_gate.lock().unwrap() = Some((entered.clone(), release.clone()));
    let language = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::new()),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.image-stop", language)
        .await;
    let executor_fiber = stack.activate_executor("image-prepare-stop").await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let submitted = turns
        .submit_image(SubmitImage {
            turn_id: client_turn_id(),
            session: stack.fresh(header()).await,
            model: ModelRef::new("deployment", "image-model").unwrap(),
            request: ImageRequest::new("pending preparation", 1).unwrap(),
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    let report = tokio::time::timeout(std::time::Duration::from_secs(1), executor_fiber.dispose())
        .await
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    release.cancel();
    tokio::task::yield_now().await;
    assert_eq!(stack.image.starts.load(Ordering::SeqCst), 0);
    let page = stack
        .store
        .read_facts(
            &submitted.session_id,
            0,
            64,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap();
    assert!(!page.facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::ImageIntent { .. } | SessionFactBody::ImageStarted { .. }
    )));
    drop(turns);
    stack.dispose(language_fiber, executor_fiber).await;
}

fn partial_image_script() -> Vec<std::result::Result<ImageEvent, AiError>> {
    vec![
        Ok(ImageEvent::OutputStarted {
            index: 0,
            mime_type: "image/png".into(),
        }),
        Ok(ImageEvent::OutputChunk {
            index: 0,
            sequence: 1,
            bytes: vec![1, 2, 3],
        }),
        Ok(ImageEvent::OutputFinished { index: 0 }),
        Ok(ImageEvent::OutputStarted {
            index: 1,
            mime_type: "image/png".into(),
        }),
        Ok(ImageEvent::OutputChunk {
            index: 1,
            sequence: 1,
            bytes: vec![4, 5, 6],
        }),
        Ok(ImageEvent::OutputFinished { index: 1 }),
        Err(AiError::new(
            ErrorKind::OutputValidation,
            ErrorPhase::Assemble,
            DispatchStatus::Dispatched,
            "third image failed validation",
        )
        .unwrap()),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn image_turn_flushes_each_media_ref_and_preserves_partial_failure() {
    let stack = BaseStack::activate().await;
    stack
        .image
        .events
        .lock()
        .unwrap()
        .push_back(partial_image_script());
    let language = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::new()),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.unused", language)
        .await;
    let executor_fiber = stack.activate_executor("executor-image").await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let submitted = turns
        .submit_image(SubmitImage {
            turn_id: client_turn_id(),
            session: stack.fresh(header()).await,
            model: ModelRef::new("deployment", "image-model").unwrap(),
            request: ImageRequest::new("draw three tiles", 3).unwrap(),
        })
        .await
        .unwrap();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(outcome) = turns
                .outcome(&submitted.session_id, &submitted.turn_id)
                .await
                .unwrap()
            {
                break outcome;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let TurnOutcome::PartialFailed {
        media,
        code,
        message,
    } = outcome
    else {
        panic!("expected partial Image failure")
    };
    assert_eq!(media.len(), 2);
    assert_eq!(code, ErrorKind::OutputValidation.code());
    assert_eq!(message, "third image failed validation");
    assert_eq!(stack.media.imports.load(Ordering::Acquire), 2);

    let page = stack
        .store
        .read_facts(
            &submitted.session_id,
            0,
            64,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap();
    let outputs = page
        .facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::ImageOutput { index, media, .. } => Some((*index, media.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[0].0, 0);
    assert_eq!(outputs[1].0, 1);
    assert_eq!(outputs[0].1, media[0]);
    assert_eq!(outputs[1].1, media[1]);
    let encoded = serde_json::to_string(&page.facts).unwrap();
    assert!(!encoded.contains("[1,2,3]"));
    assert!(matches!(
        page.facts.last().unwrap().body(),
        SessionFactBody::TurnTerminal {
            outcome: TurnOutcome::PartialFailed { media, .. },
            ..
        } if media.len() == 2
    ));

    drop(turns);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executor_shutdown_releases_a_claimed_nonterminal_turn_without_reclaiming_it() {
    let stack = BaseStack::activate().await;
    let waiting_after_first = Arc::new(Notify::new());
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::GatedStream {
            events: answer_script(),
            waiting_after_first: Arc::clone(&waiting_after_first),
            release: Arc::new(Notify::new()),
        }])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.shutdown", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-shutdown").await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    turns
        .submit(SubmitTurn {
            reasoning_effort: None,
            turn_id: client_turn_id(),
            session: stack.fresh(header()).await,
            text: "remain nonterminal".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        waiting_after_first.notified(),
    )
    .await
    .expect("executor did not claim and start the turn");

    let report = tokio::time::timeout(std::time::Duration::from_secs(1), executor_fiber.dispose())
        .await
        .expect("executor shutdown must not reclaim the stopped turn");
    assert!(report.is_clean(), "{report:?}");

    drop(turns);
    stack.dispose(language_fiber, executor_fiber).await;
}
