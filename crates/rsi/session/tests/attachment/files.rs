use super::*;
use rsi_agent_session_protocol::AgentMessageContent;
use rsi_media_protocol::{FileRead, FileRef, MediaId};

#[derive(Debug, Default)]
struct FileTurns(Mutex<Vec<AgentMessageContent>>);

#[async_trait]
impl TurnService for FileTurns {
    async fn submit_message(&self, request: SubmitMessage) -> TurnResult<MessageReceipt> {
        *self.0.lock().await = request.message.content;
        Ok(MessageReceipt {
            session_id: request.session.session_id().clone(),
            message_id: request.message.message_id,
            accepted_control_seq: 1,
            observed_fact_seq: 0,
            state: MessageState::Pending,
        })
    }
    async fn prepare_resume(&self, _: &SessionId) -> TurnResult<PreparedResumeSession> {
        unreachable!()
    }
    async fn submit(&self, _: SubmitTurn) -> TurnResult<SubmittedTurn> {
        unreachable!()
    }
    async fn submit_image(&self, _: SubmitImage) -> TurnResult<SubmittedTurn> {
        unreachable!()
    }
    async fn cancel(
        &self,
        _: &SessionId,
        _: &TurnId,
        _: Option<String>,
    ) -> TurnResult<CancelResult> {
        unreachable!()
    }
    async fn observe(&self, _: &SessionId, _: u64) -> TurnResult<TurnObservation> {
        unreachable!()
    }
    async fn outcome(
        &self,
        _: &SessionId,
        _: &TurnId,
    ) -> TurnResult<Option<rsi_agent_session_protocol::TurnOutcome>> {
        unreachable!()
    }
    async fn session_header(&self, _: &SessionId) -> TurnResult<SessionHeader> {
        unreachable!()
    }
}

#[derive(Debug)]
struct FileReader {
    valid: bool,
    calls: AtomicUsize,
}

#[async_trait]
impl FileRead for FileReader {
    async fn verify(&self, _: &FileRef) -> rsi_media_protocol::Result<()> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.valid {
            Ok(())
        } else {
            Err(MediaError::Corrupt("changed bytes".into()))
        }
    }
    fn local_path(&self, _: &FileRef) -> Option<std::path::PathBuf> {
        panic!("Session admission retains identity, never a host path")
    }
}

#[tokio::test]
async fn file_input_is_verified_before_mailbox_admission_and_keeps_its_identity() {
    let workspace = tempfile::tempdir().unwrap();
    let file = FileRef::new(MediaId::new("d".repeat(64)).unwrap(), "empty.txt", 0).unwrap();
    for available in [None, Some(false), Some(true)] {
        let turns = Arc::new(FileTurns::default());
        let supplied = Arc::new(std::sync::RwLock::new(None::<Arc<dyn FileRead>>));
        let reader = available.map(|valid| {
            Arc::new(FileReader {
                valid,
                calls: AtomicUsize::new(0),
            })
        });
        let application = LocalSessionService::new(
            rsi_meta::Execution::native(tokio::runtime::Handle::current()),
            Arc::new(UnavailableCommands),
            Arc::new(UnavailableProjections),
            turns.clone(),
            Arc::new(MemoryStore::new()),
            rsi_agent_context::ContextBudget::default(),
            Arc::new(AvailableComposition),
            Arc::new(AvailableWorkspace::at(workspace.path())),
            Arc::new(TextSettings),
            Arc::new(AvailableLanguage),
            Arc::new(UnavailableImage),
            Arc::new(UnavailableMedia),
            Arc::new(NoApprovalControl),
        )
        .with_files({
            let supplied = supplied.clone();
            move || supplied.read().unwrap().clone()
        });
        let handle = application
            .create(CreateSession {
                fork: None,
                workspace_id: workspace_id(),
                session_id: SessionId::new("file-input").unwrap(),
                agent_preset_id: Some(AgentPresetId::new("image-preset").unwrap()),
            })
            .await
            .unwrap();
        *supplied.write().unwrap() = reader.clone().map(|reader| reader as Arc<dyn FileRead>);
        let result = handle
            .submit(SubmitInput {
                origin: rsi_session_protocol::SubmissionOrigin::User,
                attribution: None,
                reasoning_effort: None,
                delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
                message_id: MessageId::new("file-message").unwrap(),
                content: vec![SessionInput::File { file: file.clone() }]
                    .try_into()
                    .unwrap(),
                model: None,
                sandbox: None,
            })
            .await;
        assert_eq!(result.is_ok(), available == Some(true), "{result:?}");
        if let Some(reader) = reader {
            assert_eq!(reader.calls.load(Ordering::Relaxed), 1);
        }
        let captured = turns.0.lock().await;
        if available == Some(true) {
            assert_eq!(
                *captured,
                vec![AgentMessageContent::File { file: file.clone() }]
            );
        } else {
            assert!(captured.is_empty());
        }
    }
}
