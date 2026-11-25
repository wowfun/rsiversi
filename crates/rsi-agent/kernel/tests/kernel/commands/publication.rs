use super::*;
use rsi_agent_composition_protocol::{
    ContributionError, DraftCommandPreparation, DraftPublicationReceipt, PublicationObserver,
    PublicationReceipt,
};
use std::time::Duration;

#[tokio::test]
async fn queued_input_uses_the_last_unstarted_preset_and_keeps_its_identity_on_restart() {
    use rsi_agent_turn_protocol::{ClaimMessage, SessionCommands as _, SessionProjections as _};
    #[derive(Debug)]
    struct Presets;
    #[async_trait]
    impl AgentComposition for Presets {
        async fn default_preset_id(&self) -> Result<AgentPresetId, AgentCompositionError> {
            TestComposition.default_preset_id().await
        }
        async fn pin(
            &self,
            id: &AgentPresetId,
            _: Option<&rsi_agent_composition_protocol::AgentGenerationSeed>,
        ) -> Result<AgentCompositionPin, AgentCompositionError> {
            let pin = test_pin(id);
            Ok(if id.as_str() == "without-echo" {
                pin.without_preset_tools(Some(Arc::new(["echo".to_owned()].into())))
            } else {
                pin
            })
        }
    }
    let store = Arc::new(MemoryStore::new());
    let open =
        || AgentKernel::recover_with_clock(store.clone(), Arc::new(Presets), Arc::new(FixedClock));
    let initial = open().await.unwrap();
    let worker = initial.start_workers();
    let origin = header("pre-turn-preset");
    let id = origin.session_id();
    let message = mailbox_message("queued");
    initial
        .submit_message(SubmitMessage {
            session: fresh(origin.clone()),
            message: message.clone(),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let before = store.session_summary(id).await.unwrap();
    assert_eq!(before.durable_fact_seq, 0);
    assert!(
        test_pin(origin.agent_preset_id())
            .tools()
            .definition("echo")
            .is_some()
    );
    let next = AgentPresetId::new("without-echo").unwrap();
    let cut = initial
        .select_unstarted_preset(id, next.clone(), before.durable_control_seq)
        .await
        .unwrap();
    assert_eq!(cut, before.durable_control_seq + 1);
    let view = initial.projection_snapshot(id).await.unwrap();
    assert_eq!(view.selected_preset(), Some(&next));
    assert_eq!(view.header_sha256(), origin.fingerprint().unwrap());
    assert_eq!(initial.prepare_resume(id).await.unwrap().preset_id(), &next);
    assert_eq!(store.session_summary(id).await.unwrap().header, origin);
    let child = initial
        .create_plugin_child(rsi_agent_turn_protocol::CreatePluginChild {
            source: rsi_agent_turn_protocol::PluginChildSource::Session(Box::new(origin.clone())),
            session_id: SessionId::new("idle-plugin-child").unwrap(),
            directory: None,
            task_name: "review".into(),
            plugin: "fixture.delegator".into(),
            preset: None,
        })
        .await
        .unwrap();
    assert_eq!(child.agent_preset_id(), &next);
    initial.shutdown(worker).await.unwrap();

    let restarted = open().await.unwrap();
    let worker = restarted.start_workers();
    assert_eq!(
        restarted
            .projection_snapshot(id)
            .await
            .unwrap()
            .selected_preset(),
        Some(&next)
    );
    let prepared = restarted.prepare_resume(id).await.unwrap();
    assert_eq!(prepared.preset_id(), &next);
    restarted
        .claim_message(ClaimMessage {
            session: prepared,
            message_id: message.message_id.clone(),
            path: AgentPath::root(),
            activation_id: ActivationId::new("activation").unwrap(),
            turn_id: TurnId::new("first").unwrap(),
            step_id: StepId::new("first-step").unwrap(),
        })
        .await
        .unwrap();
    let _executor = restarted.register("preset-reader".into()).unwrap();
    let claim = restarted
        .claim("preset-reader", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let active = restarted.composition(&claim).unwrap();
    assert_eq!(active.preset_id(), &next);
    assert!(active.tools().definition("echo").is_none());
    assert!(active.tools().definition("fixture_control").is_some());
    let after = store.session_summary(id).await.unwrap();
    assert!(matches!(
        restarted
            .select_unstarted_preset(
                id,
                origin.agent_preset_id().clone(),
                after.durable_control_seq
            )
            .await,
        Err(TurnError::SessionBusy)
    ));
    restarted
        .finish_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    restarted.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn simultaneous_command_preparation_publishes_one_baseline_and_no_facts() {
    use rsi_agent_turn_protocol::SessionCommands;
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let header = header("command-baseline");
    let prepare = || match fresh(header.clone()) {
        SubmitSession::Fresh(prepared) => kernel.publish_baseline(prepared),
        _ => unreachable!(),
    };
    let (first, second) = tokio::join!(prepare(), prepare());
    assert_eq!(first.unwrap(), 1);
    assert_eq!(second.unwrap(), 1);
    let summary = store.session_summary(header.session_id()).await.unwrap();
    assert_eq!(summary.header, header);
    assert_eq!(summary.durable_fact_seq, 0);
    assert_eq!(summary.durable_control_seq, 1);
}

#[derive(Debug)]
struct Observer {
    store: Arc<dyn SessionStore>,
    blocked: bool,
    fail: bool,
    entered: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
    confirmed: AtomicUsize,
    rejected: AtomicUsize,
    uncertain: AtomicUsize,
}
impl Observer {
    fn new(fixture: &Fixture, blocked: bool, fail: bool) -> Arc<Self> {
        let observer = Arc::new(Self {
            store: fixture.store.clone(),
            blocked,
            fail,
            entered: tokio::sync::Semaphore::new(0),
            release: tokio::sync::Semaphore::new(0),
            confirmed: AtomicUsize::new(0),
            rejected: AtomicUsize::new(0),
            uncertain: AtomicUsize::new(0),
        });
        *fixture.callback.observer.lock().unwrap() = Some(observer.clone());
        observer
    }
    async fn finish(&self) -> ContributionResult<()> {
        self.entered.add_permits(1);
        if self.blocked {
            self.release.acquire().await.unwrap().forget();
        }
        if self.fail {
            return Err(ContributionError::Invalid("lost acknowledgement".into()));
        }
        self.confirmed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
#[async_trait]
impl PublicationObserver for Observer {
    async fn committed(&self, receipt: &PublicationReceipt<'_>) -> ContributionResult<()> {
        let (seq, commit) = receipt.domain_commit.expect("atomic domain receipt");
        let durable = self
            .store
            .read_domain_request(receipt.session_id, commit.request_id().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(durable.seq(), seq);
        let AgentControlRecordBody::DomainStateCommitted { commit: stored } = durable.body() else {
            panic!("canonical command control")
        };
        assert_eq!(stored, commit);
        assert!(receipt.facts.is_empty());
        self.finish().await
    }
    async fn draft_committed(
        &self,
        receipt: &DraftPublicationReceipt<'_>,
    ) -> ContributionResult<()> {
        assert_eq!(receipt.receipt.state_sha256(), receipt.state_sha256());
        assert!(matches!(
            self.store.header(receipt.session_id).await,
            Err(StoreError::NotFound(_))
        ));
        self.finish().await
    }
    async fn rejected(&self) -> ContributionResult<()> {
        self.rejected.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn unconfirmed(&self) {
        self.uncertain.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn committed_observer_releases_admission_before_waiting() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let observer = Observer::new(&fixture, true, false);
    let first = fixture.invocation("first-publication", true).await;
    let kernel = fixture.kernel.clone();
    let session = kernel.prepare_resume(&fixture.session_id).await.unwrap();
    let task = tokio::spawn(async move { kernel.execute(session, first).await });
    observer.entered.acquire().await.unwrap().forget();
    let second = fixture.invocation("second-publication", false).await;
    tokio::time::timeout(
        Duration::from_secs(1),
        fixture.kernel.execute(
            fixture
                .kernel
                .prepare_resume(&fixture.session_id)
                .await
                .unwrap(),
            second,
        ),
    )
    .await
    .expect("observer must not retain mutation admission")
    .unwrap();
    observer.release.add_permits(1);
    task.await.unwrap().unwrap();
    assert_eq!(observer.confirmed.load(Ordering::SeqCst), 1);
    assert_eq!(observer.uncertain.load(Ordering::SeqCst), 0);
    fixture.stop().await;
}

#[tokio::test]
async fn lost_command_ack_preserves_receipt_and_retry_never_replays_callback() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let observer = Observer::new(&fixture, false, true);
    let input = fixture.invocation("lost-command-ack", true).await;
    assert!(
        fixture
            .kernel
            .execute(
                fixture
                    .kernel
                    .prepare_resume(&fixture.session_id)
                    .await
                    .unwrap(),
                input.clone()
            )
            .await
            .is_err()
    );
    let receipt = fixture
        .kernel
        .query(&fixture.session_id, &input.request_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fixture
            .kernel
            .execute(
                fixture
                    .kernel
                    .prepare_resume(&fixture.session_id)
                    .await
                    .unwrap(),
                input
            )
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 1);
    assert_eq!(observer.uncertain.load(Ordering::SeqCst), 1);
    assert_eq!(observer.rejected.load(Ordering::SeqCst), 0);
    let context = fixture.callback.contexts.lock().unwrap()[0].clone();
    let evidence = context.outcome.unwrap().evidence().unwrap();
    assert!(
        matches!(evidence.as_ref(), rsi_agent_composition_protocol::CommandOutcomeEvidence::Durable { commit, control_seq, .. }
        if commit == receipt.commit() && *control_seq == receipt.control_seq())
    );
    // The captured reader cannot outlive the owning command callback/admission.
    assert!(context.history.unwrap().facts.read(0, 1).await.is_err());
    fixture.stop().await;
}

#[tokio::test]
async fn draft_publication_cancellation_preserves_swap_and_cross_draft_rejects() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let mut draft =
        AgentSessionDraft::new(header("publication-draft"), fixture.composition.clone())
            .await
            .unwrap();
    let observer = Observer::new(&fixture, true, false);
    let input = SessionCommandInvocation {
        command: draft.command_descriptors()[0].id().clone(),
        request_id: DomainRequestId::new("draft-publication").unwrap(),
        expected_revision: draft.revision(),
        arguments: CommandArguments::new(true.into()).unwrap(),
    };
    let DraftCommandPreparation::Run(prepared) = draft.prepare_command(input.clone()).unwrap()
    else {
        panic!("new command")
    };
    let applied = draft.apply_command(prepared.execute(CancellationToken::new()).await.unwrap());
    // The atomic swap is already visible before any notification I/O.
    let receipt = draft.command_receipt(&input.request_id).unwrap();
    assert_eq!(
        receipt.state_sha256(),
        draft.freeze().initial_commit().unwrap().request_sha256()
    );
    let task = tokio::spawn(applied.finish());
    observer.entered.acquire().await.unwrap().forget();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let evidence = fixture.callback.contexts.lock().unwrap()[0]
        .outcome
        .as_ref()
        .unwrap()
        .evidence()
        .unwrap();
    assert!(
        matches!(evidence.as_ref(), rsi_agent_composition_protocol::CommandOutcomeEvidence::Draft { receipt: stored, .. } if stored == &receipt)
    );
    assert_eq!(observer.uncertain.load(Ordering::SeqCst), 1);
    assert!(
        matches!(draft.prepare_command(input).unwrap(), DraftCommandPreparation::Completed(found) if found == receipt)
    );

    let rejected = Observer::new(&fixture, false, false);
    let input = SessionCommandInvocation {
        command: receipt.command().clone(),
        request_id: DomainRequestId::new("wrong-draft-publication").unwrap(),
        expected_revision: draft.revision(),
        arguments: CommandArguments::new(false.into()).unwrap(),
    };
    let DraftCommandPreparation::Run(prepared) = draft.prepare_command(input).unwrap() else {
        panic!("new command")
    };
    let mut other = AgentSessionDraft::new(
        header("other-publication-draft"),
        fixture.composition.clone(),
    )
    .await
    .unwrap();
    assert!(
        other
            .apply_command(prepared.execute(CancellationToken::new()).await.unwrap())
            .finish()
            .await
            .is_err()
    );
    assert_eq!(rejected.rejected.load(Ordering::SeqCst), 1);
    assert_eq!(rejected.uncertain.load(Ordering::SeqCst), 0);
    assert!(matches!(
        fixture.callback.contexts.lock().unwrap()[1]
            .outcome
            .as_ref()
            .unwrap()
            .evidence()
            .as_deref(),
        Some(rsi_agent_composition_protocol::CommandOutcomeEvidence::Rejected)
    ));
    fixture.stop().await;
}
