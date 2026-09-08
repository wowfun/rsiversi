use async_trait::async_trait;
use rsi_agent_session_protocol::*;
use rsi_agent_turn_protocol::*;
use rsi_meta::*;
use rsi_session_protocol::*;
use std::sync::{Arc, Mutex};
#[path = "submission/commands.rs"]
mod commands;
#[path = "submission/sources.rs"]
mod sources;

fn missing<T>() -> rsi_session_protocol::Result<T> {
    Err(rsi_session_protocol::SessionError::Backend(
        "injected status outage".into(),
    ))
}
#[derive(Debug, Default)]
struct Backend {
    block_source: std::sync::atomic::AtomicBool,
    active_source: std::sync::atomic::AtomicUsize,
    commands: Mutex<Vec<SessionCommandInvocation>>,
    command_receipt: Mutex<Option<SessionCommandReceipt>>,
    requests: Mutex<Vec<SubmitInput>>,
    facts: Mutex<Vec<SessionFact>>,
    history_requests: Mutex<Vec<Option<u64>>>,
    cancel: Mutex<Vec<CancelTarget>>,
    header: Mutex<Option<SessionHeader>>,
    resolution: std::sync::atomic::AtomicUsize,
}
#[derive(Debug)]
struct Service(Arc<Backend>);
#[async_trait]
impl SessionService for Service {
    async fn create(
        &self,
        r: CreateSession,
    ) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
        *self.0.header.lock().unwrap() = Some(
            SessionHeader::new(
                r.session_id,
                1,
                "/tmp",
                AgentPresetId::new("test").unwrap(),
                FrozenAgentSettings::new(
                    "test",
                    "system",
                    rsi_ai_protocol::ModelRef::new("test", "model").unwrap(),
                    rsi_sandbox::SandboxMode::WorkspaceWrite,
                    false,
                )
                .unwrap(),
            )
            .unwrap(),
        );
        Ok(self.0.clone())
    }
    async fn attach(&self, id: &SessionId) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
        if self
            .0
            .header
            .lock()
            .unwrap()
            .as_ref()
            .is_none_or(|h| h.session_id() != id)
        {
            return Err(rsi_session_protocol::SessionError::NotFound(id.to_string()));
        }
        Ok(self.0.clone())
    }
    async fn list_recent(
        &self,
        _: Option<&RecentSessionCursor>,
        _: usize,
    ) -> rsi_session_protocol::Result<RecentSessionPage> {
        missing()
    }
}
#[async_trait]
impl SessionHandle for Backend {
    async fn draft_snapshot(
        &self,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::SessionDraftView> {
        panic!("unexpected draft snapshot")
    }
    async fn select_preset(
        &self,
        _: rsi_session_protocol::SelectDraftPreset,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::SessionDraftView> {
        panic!("unexpected preset selection")
    }

    async fn commands(
        &self,
    ) -> rsi_session_protocol::Result<rsi_agent_session_protocol::SessionCommandsView> {
        Ok(SessionCommandsView::new(
            CommandRevision::Draft { revision: 0 },
            vec![
                SessionCommandDescriptor::new(
                    ContributionId::new("fixture.plan").unwrap(),
                    "plan",
                    "Plan on or off",
                    true,
                )
                .unwrap(),
            ],
        )
        .unwrap())
    }
    async fn execute_command(
        &self,
        invocation: rsi_agent_session_protocol::SessionCommandInvocation,
    ) -> rsi_session_protocol::Result<rsi_agent_session_protocol::SessionCommandReceipt> {
        self.commands.lock().unwrap().push(invocation.clone());
        Err(rsi_session_protocol::SessionError::CommandOutcomeUnknown {
            request_id: invocation.request_id,
        })
    }
    async fn command_status(
        &self,
        _: &rsi_agent_session_protocol::DomainRequestId,
    ) -> rsi_session_protocol::Result<Option<rsi_agent_session_protocol::SessionCommandReceipt>>
    {
        Ok(self.command_receipt.lock().unwrap().clone())
    }

    async fn header(&self) -> rsi_session_protocol::Result<SessionHeader> {
        Ok(self.header.lock().unwrap().clone().unwrap())
    }
    async fn submit(&self, req: SubmitInput) -> rsi_session_protocol::Result<MessageReceipt> {
        let err = rsi_session_protocol::SessionError::MessageOutcomeUnknown {
            session: self.header().await?.session_id().to_string(),
            message: req.message_id.to_string(),
        };
        let id = req.message_id.clone();
        self.requests.lock().unwrap().push(req);
        if self.resolution.load(std::sync::atomic::Ordering::SeqCst) > 0 {
            self.receipt(&id).await
        } else {
            Err(err)
        }
    }
    async fn message_status(&self, id: &MessageId) -> rsi_session_protocol::Result<MessageReceipt> {
        match self.resolution.load(std::sync::atomic::Ordering::SeqCst) {
            1 => Err(rsi_session_protocol::SessionError::NotFound(id.to_string())),
            2 => self.receipt(id).await,
            _ => missing(),
        }
    }
    async fn read_message(
        &self,
        _: &MessageId,
        _: u64,
    ) -> rsi_session_protocol::Result<AgentMessage> {
        missing()
    }
    async fn generate_image(
        &self,
        _: SubmitDirectImage,
    ) -> rsi_session_protocol::Result<TurnReceipt> {
        missing()
    }
    async fn cancel(
        &self,
        t: CancelTarget,
        _: Option<String>,
    ) -> rsi_session_protocol::Result<CancelResult> {
        self.cancel.lock().unwrap().push(t);
        Ok(CancelResult {
            accepted: true,
            already_terminal: false,
        })
    }
    async fn history_before(
        &self,
        before: Option<u64>,
        limit: usize,
    ) -> rsi_session_protocol::Result<SessionHistoryPage> {
        self.history_requests.lock().unwrap().push(before);
        if limit == 1 && self.block_source.load(std::sync::atomic::Ordering::SeqCst) {
            struct Active<'a>(&'a std::sync::atomic::AtomicUsize);
            impl Drop for Active<'_> {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                }
            }
            self.active_source
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _active = Active(&self.active_source);
            std::future::pending::<()>().await;
        }
        let facts = self.facts.lock().unwrap();
        let durable_seq = facts.last().map_or(0, SessionFact::seq);
        let before_seq = before.unwrap_or(durable_seq + 1);
        let eligible: Vec<_> = facts
            .iter()
            .filter(|fact| fact.seq() < before_seq)
            .cloned()
            .collect();
        let start = eligible.len().saturating_sub(limit);
        Ok(SessionHistoryPage {
            before_seq,
            facts: eligible[start..].to_vec(),
            durable_seq,
            has_more: start > 0,
        })
    }
    async fn inspect(
        &self,
    ) -> rsi_session_protocol::Result<rsi_agent_store_protocol::StoreSessionInspection> {
        use rsi_agent_store_protocol::{
            StoreAgentSessionStatus, StoreAgentSubtreeSnapshot, StoreSessionInspection,
        };
        let header = self.header().await?;
        Ok(StoreSessionInspection {
            tree: StoreAgentSubtreeSnapshot {
                session: StoreAgentSessionStatus {
                    session_id: header.session_id().clone(),
                    durable_control_seq: 1,
                    has_open_turn: false,
                    has_active_activation: false,
                    has_waking_message: false,
                },
                descendants: vec![],
            },
            header,
            durable_fact_seq: self
                .facts
                .lock()
                .unwrap()
                .last()
                .map_or(0, SessionFact::seq),
            durable_control_seq: 1,
            pending: vec![],
            active_turn_id: None,
            activation_phase: None,
        })
    }
    async fn observe(
        &self,
        _: ObservationCursor,
    ) -> rsi_session_protocol::Result<SessionObservationStream> {
        Ok(Box::pin(futures_util::stream::pending()))
    }
    async fn observe_projections(
        &self,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::ProjectionStream> {
        Ok(Box::pin(futures_util::stream::pending()))
    }
    async fn observe_interactions(&self) -> rsi_session_protocol::Result<InteractionStream> {
        Ok(Box::pin(futures_util::stream::pending()))
    }
    async fn pending_questions(
        &self,
    ) -> rsi_session_protocol::Result<Vec<rsi_user_questions_protocol::QuestionRequest>> {
        missing()
    }
    async fn answer_question(
        &self,
        _: &str,
        _: rsi_user_questions_protocol::QuestionAnswer,
    ) -> rsi_session_protocol::Result<bool> {
        missing()
    }
    async fn pending_approvals(
        &self,
    ) -> rsi_session_protocol::Result<Vec<rsi_approval_protocol::ApprovalRequest>> {
        missing()
    }
    async fn answer_approval(
        &self,
        _: &SessionId,
        _: &str,
        _: rsi_approval_protocol::ApprovalDecision,
    ) -> rsi_session_protocol::Result<bool> {
        missing()
    }
}
impl Backend {
    async fn receipt(&self, id: &MessageId) -> rsi_session_protocol::Result<MessageReceipt> {
        Ok(MessageReceipt {
            session_id: self.header().await?.session_id().clone(),
            message_id: id.clone(),
            accepted_control_seq: 1,
            observed_fact_seq: 0,
            state: MessageState::Pending,
        })
    }
}
#[derive(Debug)]
struct Unused;
#[async_trait]
impl rsi_workspace_protocol::WorkspaceRegistry for Unused {
    async fn get(
        &self,
        _: &rsi_workspace_protocol::WorkspaceId,
    ) -> rsi_workspace_protocol::Result<rsi_workspace_protocol::WorkspaceRecord> {
        unreachable!()
    }
    async fn list(
        &self,
        _: Option<rsi_workspace_protocol::WorkspaceCursor>,
        _: usize,
    ) -> rsi_workspace_protocol::Result<rsi_workspace_protocol::WorkspacePage> {
        unreachable!()
    }
    async fn get_or_create(
        &self,
        _: &std::path::Path,
    ) -> rsi_workspace_protocol::Result<rsi_workspace_protocol::WorkspaceRecord> {
        unreachable!()
    }
    async fn status(
        &self,
        _: &rsi_workspace_protocol::WorkspaceId,
    ) -> rsi_workspace_protocol::Result<rsi_workspace_protocol::WorkspaceStatus> {
        unreachable!()
    }
    async fn delete_registration(
        &self,
        _: &rsi_workspace_protocol::WorkspaceId,
    ) -> rsi_workspace_protocol::Result<bool> {
        unreachable!()
    }
}
#[async_trait]
impl rsi_ai_protocol::LanguageModels for Unused {
    async fn list_models(
        &self,
        _: Option<&rsi_ai_protocol::ModelRef>,
        _: usize,
    ) -> std::result::Result<rsi_ai_protocol::LanguageModelPage, rsi_ai_protocol::ModelsError> {
        unreachable!()
    }
}
#[async_trait]
impl rsi_settings_protocol::SettingsAccess for Unused {
    async fn read(
        &self,
        _: &str,
    ) -> rsi_settings_protocol::Result<rsi_settings_protocol::SettingsSnapshot> {
        unreachable!()
    }
    async fn replace(
        &self,
        _: &str,
        _: &rsi_settings_protocol::SettingsVersion,
        _: serde_json::Value,
    ) -> rsi_settings_protocol::Result<rsi_settings_protocol::SettingsSnapshot> {
        unreachable!()
    }
    async fn clear(
        &self,
        _: &str,
        _: &rsi_settings_protocol::SettingsVersion,
    ) -> rsi_settings_protocol::Result<rsi_settings_protocol::SettingsSnapshot> {
        unreachable!()
    }
}
#[derive(Debug)]
struct Providers(Arc<Backend>);
#[async_trait]
impl PluginFactory for Providers {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let _s = plan
            .context()
            .provide_local::<SessionContract>(Arc::new(Service(self.0.clone())))
            .unwrap();
        let _w = plan
            .context()
            .provide_local::<rsi_workspace_protocol::WorkspaceRegistryContract>(Arc::new(Unused))
            .unwrap();
        let _m = plan
            .context()
            .provide_local::<rsi_ai_protocol::LanguageModelsContract>(Arc::new(Unused))
            .unwrap();
        let _se = plan
            .context()
            .provide_local::<rsi_settings_protocol::SettingsAccessContract>(Arc::new(Unused))
            .unwrap();
        plan.defer(
            "withdraw fake domains",
            Box::new(move || {
                Box::pin(async move {
                    drop((_s, _w, _m, _se));
                    Ok(())
                })
            }),
        )
    }
}

#[tokio::test]
async fn unknown_submission_retains_its_identity_across_user_retries() {
    for resolution in [1, 2] {
        verify_retry(resolution).await;
    }
}
async fn verify_retry(resolution: usize) {
    let rt = Runtime::with_execution(
        RuntimeLimits::default(),
        Execution::native(tokio::runtime::Handle::current()),
    )
    .unwrap();
    let backend = Arc::new(Backend::default());
    let root = rt.root();
    let providers = root
        .apply(
            ResolvedFactory::linked(
                "providers",
                "test",
                UpdateMode::RestartRequired,
                Arc::new(Providers(backend.clone())),
            ),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(providers.snapshot().state, FiberState::Active);
    let fiber = root
        .apply(
            ResolvedFactory::linked(
                "web",
                "test",
                UpdateMode::RestartRequired,
                Arc::new(rsi_web::WebApplicationFactory),
            ),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(fiber.snapshot().state, FiberState::Active);
    let app = root
        .lookup_local::<rsi_web::WebApplicationContract>()
        .unwrap();
    app.command(r#"{"action":"create","pane":0,"workspace":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","trust":false}"#).await.unwrap();
    let view: serde_json::Value = serde_json::from_slice(app.view().unwrap().as_bytes()).unwrap();
    let generation = view["panes"][0]["generation"].as_str().unwrap();
    let input=serde_json::json!({"action":"submit","pane":0,"generation":generation,"text":"perform effect once","steer":false}).to_string();
    for _ in 0..2 {
        assert!(app.command(&input).await.is_err());
    }
    assert_eq!(
        backend.requests.lock().unwrap().len(),
        1,
        "status outage must not replay a possibly accepted request"
    );
    let original = backend.requests.lock().unwrap()[0].clone();
    let session = view["panes"][0]["session"].clone();
    // Replacing the surface must preserve both the original request and its admission.
    app.command(&serde_json::json!({"action":"open","pane":0,"session":session}).to_string())
        .await
        .unwrap();
    let view: serde_json::Value = serde_json::from_slice(app.view().unwrap().as_bytes()).unwrap();
    let generation = view["panes"][0]["generation"].as_str().unwrap();
    let model = rsi_ai_protocol::ModelRef::new("other", "new-model").unwrap();
    app.command(
        &serde_json::json!({"action":"model","pane":0,"generation":generation,"model":model})
            .to_string(),
    )
    .await
    .unwrap();
    backend
        .resolution
        .store(resolution, std::sync::atomic::Ordering::SeqCst);
    app.command(&serde_json::json!({"action":"submit","pane":0,"generation":generation,"text":"edited next draft","steer":true}).to_string()).await.unwrap();
    {
        let requests = backend.requests.lock().unwrap();
        assert_eq!(requests.len(), if resolution == 1 { 2 } else { 1 });
        assert!(
            requests
                .iter()
                .all(|request| serde_json::to_value(request).unwrap()
                    == serde_json::to_value(&original).unwrap()),
            "retry changed identity, content, model, or delivery"
        );
    }
    let view: serde_json::Value = serde_json::from_slice(app.view().unwrap().as_bytes()).unwrap();
    assert_eq!(view["panes"][0]["draft"], "edited next draft");
    assert!(view["panes"][0]["unresolved_text"].is_null());
    app.command(&serde_json::json!({"action":"submit","pane":0,"generation":generation,"text":"edited next draft","steer":false}).to_string()).await.unwrap();
    {
        let requests = backend.requests.lock().unwrap();
        let last = requests.last().unwrap();
        assert_ne!(last.message_id, original.message_id);
        assert_eq!(
            last.content,
            vec![SessionInput::Text {
                text: "edited next draft".into()
            }]
        );
        assert_eq!(last.model, Some(model));
    }
    assert!(rt.shutdown().await.is_clean());
}

#[tokio::test]
async fn failed_navigation_at_draft_capacity_keeps_current_draft_editable() {
    let rt = Runtime::with_execution(
        RuntimeLimits::default(),
        Execution::native(tokio::runtime::Handle::current()),
    )
    .unwrap();
    let backend = Arc::new(Backend::default());
    let root = rt.root();
    let providers = root
        .apply(
            ResolvedFactory::linked(
                "providers",
                "test",
                UpdateMode::RestartRequired,
                Arc::new(Providers(backend.clone())),
            ),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(providers.snapshot().state, FiberState::Active);
    let fiber = root
        .apply(
            ResolvedFactory::linked(
                "web",
                "test",
                UpdateMode::RestartRequired,
                Arc::new(rsi_web::WebApplicationFactory),
            ),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(fiber.snapshot().state, FiberState::Active);
    let app = root
        .lookup_local::<rsi_web::WebApplicationContract>()
        .unwrap();

    let mut current = serde_json::Value::Null;
    for index in 0..64 {
        app.command(r#"{"action":"create","pane":0,"workspace":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","trust":false}"#).await.unwrap();
        current = serde_json::from_slice(app.view().unwrap().as_bytes()).unwrap();
        if index < 63 {
            app.command(&serde_json::json!({"action":"draft","pane":0,"generation":current["panes"][0]["generation"],"text":"saved"}).to_string()).await.unwrap();
        }
    }
    assert!(
        app.command(r#"{"action":"open","pane":0,"session":"missing"}"#)
            .await
            .is_err()
    );
    app.command(&serde_json::json!({"action":"draft","pane":0,"generation":current["panes"][0]["generation"],"text":"still editable"}).to_string()).await.unwrap();
    let view: serde_json::Value = serde_json::from_slice(app.view().unwrap().as_bytes()).unwrap();
    assert_eq!(view["panes"][0]["draft"], "still editable");
    assert_eq!(
        view["panes"][0]["generation"],
        current["panes"][0]["generation"]
    );
    assert!(rt.shutdown().await.is_clean());
}

#[tokio::test]
async fn returning_to_live_restarts_history_at_the_live_projection() {
    let rt = Runtime::with_execution(
        RuntimeLimits::default(),
        Execution::native(tokio::runtime::Handle::current()),
    )
    .unwrap();
    let backend = Arc::new(Backend::default());
    let root = rt.root();
    let providers = root
        .apply(
            ResolvedFactory::linked(
                "providers",
                "test",
                UpdateMode::RestartRequired,
                Arc::new(Providers(backend.clone())),
            ),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(providers.snapshot().state, FiberState::Active);
    let fiber = root
        .apply(
            ResolvedFactory::linked(
                "web",
                "test",
                UpdateMode::RestartRequired,
                Arc::new(rsi_web::WebApplicationFactory),
            ),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(fiber.snapshot().state, FiberState::Active);
    let app = root
        .lookup_local::<rsi_web::WebApplicationContract>()
        .unwrap();

    app.command(r#"{"action":"create","pane":0,"workspace":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","trust":false}"#).await.unwrap();
    let view: serde_json::Value = serde_json::from_slice(app.view().unwrap().as_bytes()).unwrap();
    let session = view["panes"][0]["session"].clone();
    *backend.facts.lock().unwrap() = (1..=300)
        .map(|seq| {
            SessionFact::new(
                seq,
                1,
                SessionFactBody::TurnTerminal {
                    turn_id: TurnId::new(format!("turn-{seq}")).unwrap(),
                    outcome: TurnOutcome::Completed,
                },
            )
            .unwrap()
        })
        .collect();
    app.command(&serde_json::json!({"action":"open","pane":0,"session":session}).to_string())
        .await
        .unwrap();
    let view: serde_json::Value = serde_json::from_slice(app.view().unwrap().as_bytes()).unwrap();
    let generation = &view["panes"][0]["generation"];
    let command =
        |action| serde_json::json!({"action":action,"pane":0,"generation":generation}).to_string();
    app.command(&command("history")).await.unwrap();
    let prior: serde_json::Value = serde_json::from_slice(app.view().unwrap().as_bytes()).unwrap();
    app.command(&command("history")).await.unwrap();
    app.command(&command("live")).await.unwrap();
    app.command(&command("history")).await.unwrap();
    let again: serde_json::Value = serde_json::from_slice(app.view().unwrap().as_bytes()).unwrap();
    assert_eq!(
        prior["panes"][0]["transcript"],
        again["panes"][0]["transcript"]
    );
    assert_eq!(
        *backend.history_requests.lock().unwrap(),
        [Some(301), Some(173), Some(45), Some(173)]
    );
    assert!(rt.shutdown().await.is_clean());
}
