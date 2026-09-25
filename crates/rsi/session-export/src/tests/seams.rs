use super::*;
use rsi_agent_store_protocol::{Result as StoreResult, *};
use rsi_session_protocol::SessionError;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn unpublished_draft_supports_every_section_combination_in_both_formats() {
    let sections = [
        ExportInclude::Header,
        ExportInclude::Messages,
        ExportInclude::Reasoning,
        ExportInclude::ProviderInputEvidence,
        ExportInclude::LastProviderRequest,
        ExportInclude::LastProviderResponse,
    ];
    for mask in 1..64 {
        for format in [ExportFormat::Json, ExportFormat::Markdown] {
            let mut include = sections
                .into_iter()
                .enumerate()
                .filter_map(|(index, section)| (mask & (1 << index) != 0).then_some(section))
                .collect::<std::collections::BTreeSet<_>>();
            if include.contains(&ExportInclude::Reasoning) {
                include.insert(ExportInclude::Messages);
            }
            let options = ExportOptions { format, include };
            let header = header();
            let mut source =
                empty(header.clone(), options.clone(), CancellationToken::new()).unwrap();
            let start = source.next().await.unwrap().unwrap();
            assert!(
                matches!(&start, ExportEvent::Start {through_seq, header_sha256, options: accepted, ..}
                if through_seq == "0" && *header_sha256 == header.fingerprint().unwrap() && *accepted == options)
            );
            let source = Box::pin(futures_util::stream::once(async { Ok(start) }).chain(source));
            let mut bytes = Vec::new();
            write_stream(source, &mut bytes).await.unwrap();
            let text = String::from_utf8(bytes).unwrap();
            assert!(!text.contains("hidden instructions"));
            if format == ExportFormat::Json {
                let value: Value = serde_json::from_str(&text).unwrap();
                for (section, key) in [
                    (ExportInclude::Header, "header"),
                    (ExportInclude::Messages, "messages"),
                    (
                        ExportInclude::ProviderInputEvidence,
                        "provider_input_evidence",
                    ),
                    (ExportInclude::LastProviderRequest, "last_provider_request"),
                    (
                        ExportInclude::LastProviderResponse,
                        "last_provider_response",
                    ),
                ] {
                    assert_eq!(value.get(key).is_some(), options.has(section));
                }
                if options.has(ExportInclude::Messages) {
                    assert_eq!(value["messages"], json!([]));
                }
                if options.has(ExportInclude::ProviderInputEvidence) {
                    assert_eq!(value["provider_input_evidence"], json!([]));
                }
                for key in ["last_provider_request", "last_provider_response"] {
                    if let Some(diagnostic) = value.get(key) {
                        assert_eq!(diagnostic["reason"], "no_completed_conversation");
                    }
                }
            } else {
                assert_eq!(
                    text.contains("# messages"),
                    options.has(ExportInclude::Messages)
                );
                assert_eq!(
                    text.contains("no_completed_conversation"),
                    options.has(ExportInclude::LastProviderRequest)
                        || options.has(ExportInclude::LastProviderResponse)
                );
            }
        }
    }
}

#[tokio::test]
async fn cancellation_emits_one_error_then_eof_and_releases_pending_read_and_lease() {
    let store = Arc::new(PausedStore {
        inner: MemoryStore::new(),
        entered: tokio::sync::Notify::default(),
        pending: Arc::default(),
        leases: Arc::default(),
    });
    let header = header();
    append(&store.inner, &header, 0, vec![accepted("never returned")]).await;
    let stop = CancellationToken::new();
    let mut source = export(
        store.clone(),
        header,
        ExportOptions::default(),
        stop.clone(),
    )
    .await
    .unwrap();
    assert!(matches!(
        source.next().await.unwrap().unwrap(),
        ExportEvent::Start { .. }
    ));
    // Poll until the producer is suspended inside an actual Store read.
    let next = async {
        loop {
            let event = source.next().await.expect("read pending before EOF");
            assert!(matches!(event.unwrap(), ExportEvent::Chunk { .. }));
        }
    };
    tokio::select! {
        biased;
        () = store.entered.notified() => {},
        () = next => panic!("blocked read unexpectedly completed"),
    }
    assert_eq!(store.pending.load(Ordering::SeqCst), 1);
    assert_eq!(store.leases.load(Ordering::SeqCst), 1);
    stop.cancel();
    assert!(
        matches!(source.next().await, Some(Err(SessionError::Backend(message))) if message == "Session export stopped")
    );
    assert!(source.next().await.is_none());
    assert_eq!(store.pending.load(Ordering::SeqCst), 0);
    assert_eq!(store.leases.load(Ordering::SeqCst), 0);
    let stop = CancellationToken::new();
    stop.cancel();
    let mut draft = empty(super::header(), ExportOptions::default(), stop).unwrap();
    assert!(matches!(draft.next().await, Some(Err(_))));
    assert!(draft.next().await.is_none());
}

#[tokio::test]
async fn producer_rejects_changed_header_before_emitting_start() {
    let store = Arc::new(MemoryStore::new());
    let header = header();
    append(&store, &header, 0, vec![accepted("private")]).await;
    let changed = SessionHeader::new(
        header.session_id().clone(),
        2,
        "/changed",
        header.agent_preset_id().clone(),
        header.settings().clone(),
    )
    .unwrap();
    store.take_fact_read_cursors();
    assert!(
        matches!(export(store.clone(), changed, ExportOptions::default(), CancellationToken::new()).await,
        Err(SessionError::Invalid(message)) if message == "export Header changed")
    );
    assert!(store.take_fact_read_cursors().is_empty());
}

#[tokio::test]
async fn producer_rejects_each_changed_inherited_binding_before_emitting_start() {
    let store = Arc::new(MemoryStore::new());
    let parent = header();
    let invoking = TurnId::new("spawn").unwrap();
    let mut spawn = accepted("not inherited");
    if let SessionFactBody::TurnAccepted { turn_id, .. } = &mut spawn {
        *turn_id = invoking.clone();
    }
    append(
        &store,
        &parent,
        0,
        vec![
            accepted("inherited"),
            SessionFactBody::TurnTerminal {
                turn_id: turn(),
                outcome: TurnOutcome::Completed,
                result: None,
            },
            spawn,
        ],
    )
    .await;
    let boundary = store
        .resolve_fork_boundary(parent.session_id(), &invoking, ForkTurnSelection::All)
        .await
        .unwrap();
    for field in 0..7 {
        let mut origin = ForkOrigin {
            parent_session_id: parent.session_id().clone(),
            root_session_id: parent.session_id().clone(),
            path: AgentPath::new(vec![field + 1]).unwrap(),
            task_name: format!("child-{field}"),
            parent_header_fingerprint: parent.fingerprint().unwrap(),
            invoking_turn_id: invoking.clone(),
            resolved_after_seq: boundary.resolved_after_seq,
            resolved_terminal_seq: boundary.resolved_terminal_seq,
            terminal_prefix_sha256: boundary.terminal_prefix_sha256.clone(),
            resolved_terminal_control_seq: boundary.resolved_terminal_control_seq,
            terminal_control_prefix_sha256: boundary.terminal_control_prefix_sha256.clone(),
            requested_turns: ForkTurnSelection::All,
            effective_turns: boundary.effective_turns,
        };
        match field {
            0 => origin.parent_header_fingerprint = "b".repeat(64),
            1 => origin.resolved_after_seq += 1,
            2 => origin.resolved_terminal_seq += 1,
            3 => origin.terminal_prefix_sha256 = "b".repeat(64),
            4 => origin.resolved_terminal_control_seq += 1,
            5 => origin.terminal_control_prefix_sha256 = "b".repeat(64),
            _ => origin.effective_turns += 1,
        }
        let child = parent
            .forked_child(
                SessionId::new(format!("child-{field}")).unwrap(),
                2,
                origin,
                ModelSelection::baseline(parent.settings()),
            )
            .unwrap();
        append(&store, &child, 0, vec![accepted("child")]).await;
        assert!(
            matches!(export(store.clone(), child, ExportOptions::default(), CancellationToken::new()).await,
            Err(SessionError::Invalid(message)) if message == "export inherited history binding changed"),
            "field {field}"
        );
    }
}

#[derive(Debug)]
struct PausedStore {
    inner: MemoryStore,
    entered: tokio::sync::Notify,
    pending: Arc<AtomicUsize>,
    leases: Arc<AtomicUsize>,
}
struct Released(Arc<AtomicUsize>);
impl Drop for Released {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait::async_trait]
impl SessionStore for PausedStore {
    async fn list_program_notices(
        &self,
        after: Option<&rsi_agent_store_protocol::StoreProgramNotice>,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<rsi_agent_store_protocol::StoreProgramNoticePage> {
        self.inner.list_program_notices(after, limit).await
    }

    async fn read_program_records(
        &self,
        session: &SessionId,
        run: &rsi_agent_session_protocol::ProgramRunId,
    ) -> rsi_agent_store_protocol::Result<Option<rsi_agent_store_protocol::StoreProgramRecords>>
    {
        self.inner.read_program_records(session, run).await
    }
    async fn program_run_for_creator(
        &self,
        session: &SessionId,
        turn: &TurnId,
    ) -> rsi_agent_store_protocol::Result<Option<rsi_agent_session_protocol::ProgramRunId>> {
        self.inner.program_run_for_creator(session, turn).await
    }
    async fn read_program_records_after(
        &self,
        session: &SessionId,
        run: &rsi_agent_session_protocol::ProgramRunId,
        after: u64,
    ) -> rsi_agent_store_protocol::Result<Option<rsi_agent_store_protocol::StoreProgramRecords>>
    {
        self.inner
            .read_program_records_after(session, run, after)
            .await
    }
    async fn list_active_program_runs(
        &self,
        after: Option<&rsi_agent_store_protocol::StoreProgramCursor>,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<rsi_agent_store_protocol::StoreProgramPage> {
        self.inner.list_active_program_runs(after, limit).await
    }

    async fn prepare_session(&self, session_id: &SessionId) -> StoreResult<SessionValidationLease> {
        let lease = self.inner.prepare_session(session_id).await?;
        self.leases.fetch_add(1, Ordering::SeqCst);
        Ok(SessionValidationLease::new((
            lease,
            Released(self.leases.clone()),
        )))
    }
    async fn read_watermarks(&self, session_id: &SessionId) -> StoreResult<StoreSessionWatermarks> {
        self.inner.read_watermarks(session_id).await
    }
    async fn read_fact_suffix(
        &self,
        session_id: &SessionId,
        limit: usize,
        maximum_bytes: usize,
    ) -> StoreResult<StoreFactSuffix> {
        self.inner
            .read_fact_suffix(session_id, limit, maximum_bytes)
            .await
    }
    async fn append(&self, batch: AppendBatch) -> StoreResult<AppendCommit> {
        self.inner.append(batch).await
    }
    async fn validate_session(&self, session_id: &SessionId) -> StoreResult<()> {
        self.inner.validate_session(session_id).await
    }
    async fn header(&self, session_id: &SessionId) -> StoreResult<SessionHeader> {
        self.inner.header(session_id).await
    }
    async fn read_facts(
        &self,
        session_id: &SessionId,
        after_seq: u64,
        limit: usize,
    ) -> StoreResult<StoreFactPage> {
        let _ = (session_id, after_seq, limit);
        let _read = Released(self.pending.clone());
        self.pending.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        std::future::pending().await
    }
    async fn read_facts_before(
        &self,
        session_id: &SessionId,
        exclusive_before_seq: u64,
        limit: usize,
    ) -> StoreResult<StoreBackwardFactPage> {
        self.inner
            .read_facts_before(session_id, exclusive_before_seq, limit)
            .await
    }
    async fn read_turn_facts(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
        after_seq: u64,
        limit: usize,
    ) -> StoreResult<StoreTurnFactPage> {
        self.inner
            .read_turn_facts(session_id, turn_id, after_seq, limit)
            .await
    }
    async fn read_turn_boundary(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
    ) -> StoreResult<StoreTurnBoundary> {
        self.inner.read_turn_boundary(session_id, turn_id).await
    }
    async fn list_open_turns(
        &self,
        session_id: &SessionId,
        after_accepted_seq: u64,
        limit: usize,
    ) -> StoreResult<StoreOpenTurnPage> {
        self.inner
            .list_open_turns(session_id, after_accepted_seq, limit)
            .await
    }
    async fn list_sessions(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> StoreResult<StoreSessionPage> {
        self.inner.list_sessions(after, limit).await
    }
    async fn list_recent_sessions(
        &self,
        after: Option<&StoreRecentSessionCursor>,
        limit: usize,
    ) -> StoreResult<StoreRecentSessionPage> {
        self.inner.list_recent_sessions(after, limit).await
    }
    async fn list_open_sessions(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> StoreResult<StoreSessionPage> {
        self.inner.list_open_sessions(after, limit).await
    }
    async fn put_cas(&self, bytes: Arc<[u8]>) -> StoreResult<CasObjectRef> {
        self.inner.put_cas(bytes).await
    }
    async fn read_cas(&self, object: &CasObjectRef) -> StoreResult<Arc<[u8]>> {
        self.inner.read_cas(object).await
    }
}
