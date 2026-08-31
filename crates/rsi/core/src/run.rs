use crate::settings::{AgentSettings, AgentSettingsContract};
use crate::{Result, RsiError, RunningRsi};
use futures_util::StreamExt as _;
use rsi_agent_composition_protocol::{
    AgentComposition, AgentCompositionContract, AgentSessionDraft,
};
use rsi_agent_session_protocol::{
    AgentPresetId, SessionFact, SessionFactBody, SessionHeader, SessionId, TurnId, TurnOutcome,
};
use rsi_agent_turn_protocol::{
    SubmitImage, SubmitSession, SubmitTurn, TurnService, TurnServiceContract, TurnUpdate,
};
use rsi_ai_protocol::{
    ContentDelta, ImageCallContract, ImageRequest, LanguageCall, LanguageCallContract,
    LanguageEvent, ModelRef,
};
use rsi_sandbox::SandboxMode;
use rsi_tools_protocol::ToolContent;
use rsi_workspace::{WorkspaceRegistry, WorkspaceRegistryContract};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

/// Standard output encoding selected by one invocation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OutputMode {
    /// Visible model text and `media:<MediaId>` lines.
    #[default]
    Text,
    /// Versioned session, raw Fact, and outcome envelopes.
    Jsonl,
}

/// Fresh or resumed durable session selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionSelection {
    /// Creates one session, optionally with a caller-preallocated identity.
    Fresh {
        /// Canonicalized before the durable header is created.
        cwd: PathBuf,
        /// Explicit identity used for text-mode recoverability.
        session_id: Option<SessionId>,
        /// Explicit Agent preset, or `None` to resolve the current catalog default.
        agent_preset_id: Option<AgentPresetId>,
    },
    /// Resumes one exact session using its durable workspace as authority.
    Resume {
        /// Exact durable identity.
        session_id: SessionId,
        /// Optional assertion that must resolve to the durable workspace.
        cwd: Option<PathBuf>,
    },
}

/// One complete Headless turn request.
#[derive(Clone, Debug)]
pub struct RunOptions {
    /// Exact ordinary user text, including a leading slash when present.
    pub task: String,
    /// Fresh or resume selection.
    pub session: SessionSelection,
    /// Invocation-only exact model override.
    pub model: Option<ModelRef>,
    /// Invocation-only sandbox override.
    pub sandbox: Option<SandboxMode>,
    /// Output encoding used by the binary renderer.
    pub output: OutputMode,
}

/// One direct Image turn request for the standard library surface.
#[derive(Clone, Debug)]
pub struct RunImageOptions {
    /// Fresh or resume selection.
    pub session: SessionSelection,
    /// Exact Image deployment and model.
    pub model: ModelRef,
    /// Complete provider-neutral request.
    pub request: ImageRequest,
}

/// Completed owned interval and rendering evidence for one invocation.
#[derive(Clone, Debug)]
pub struct RunReport {
    completion: RunCompletion,
    facts: Vec<Arc<SessionFact>>,
}

/// Terminal durability evidence without retaining the streamed Fact interval.
#[derive(Clone, Debug)]
pub struct RunCompletion {
    session_id: SessionId,
    turn_id: TurnId,
    accepted_seq: u64,
    outcome: TurnOutcome,
    durable_seq: u64,
    cancellation_requested: bool,
}

/// One application-visible live event emitted by the standard runner.
#[derive(Clone, Debug)]
pub enum RunEvent {
    /// A turn was accepted into one exact session.
    Session {
        /// Exact session identity.
        session_id: SessionId,
        /// Exact turn identity.
        turn_id: TurnId,
        /// Sequence assigned to the accepted Fact.
        accepted_seq: u64,
    },
    /// A raw Fact entered this process-owned live interval.
    Fact {
        /// Exact session identity.
        session_id: SessionId,
        /// Unmodified Fact payload.
        fact: Arc<SessionFact>,
        /// Durable watermark at live publication time.
        durable_seq: u64,
    },
    /// The terminal Fact and its prefix became durable.
    Outcome {
        /// Exact session identity.
        session_id: SessionId,
        /// Exact turn identity.
        turn_id: TurnId,
        /// Canonical terminal outcome.
        outcome: TurnOutcome,
        /// Durable watermark observed before emission.
        durable_seq: u64,
    },
}

impl RunEvent {
    /// Encodes one version-2 JSONL envelope.
    pub fn json_line(&self) -> std::result::Result<String, serde_json::Error> {
        match self {
            Self::Session {
                session_id,
                turn_id,
                accepted_seq,
            } => serde_json::to_string(&SessionEnvelope {
                version: 2,
                kind: "session",
                session_id,
                turn_id,
                accepted_seq: *accepted_seq,
            }),
            Self::Fact {
                session_id,
                fact,
                durable_seq,
            } => serde_json::to_string(&LiveFactEnvelope {
                version: 2,
                kind: "fact",
                session_id,
                fact: fact.as_ref(),
                durable_seq: *durable_seq,
            }),
            Self::Outcome {
                session_id,
                turn_id,
                outcome,
                durable_seq,
            } => serde_json::to_string(&OutcomeEnvelope {
                version: 2,
                kind: "outcome",
                session_id,
                turn_id,
                outcome,
                durable_seq: *durable_seq,
            }),
        }
    }
}

impl RunReport {
    /// Exact durable session identity.
    pub const fn session_id(&self) -> &SessionId {
        &self.completion.session_id
    }

    /// Exact submitted turn identity.
    pub const fn turn_id(&self) -> &TurnId {
        &self.completion.turn_id
    }

    /// Raw Facts observed in this process-owned interval.
    pub fn facts(&self) -> &[Arc<SessionFact>] {
        &self.facts
    }

    /// Canonical terminal outcome.
    pub const fn outcome(&self) -> &TurnOutcome {
        &self.completion.outcome
    }

    /// Durable watermark observed before returning.
    pub const fn durable_seq(&self) -> u64 {
        self.completion.durable_seq
    }

    /// Whether the caller-owned cancellation token fired during the turn.
    pub const fn cancellation_requested(&self) -> bool {
        self.completion.cancellation_requested
    }

    /// Stable process exit code for this completed report.
    pub const fn exit_code(&self) -> u8 {
        self.completion.exit_code()
    }

    /// Returns the non-collecting terminal completion.
    pub const fn completion(&self) -> &RunCompletion {
        &self.completion
    }

    /// Renders visible text and Media references in original Fact order.
    pub fn text_output(&self) -> String {
        let mut output = String::new();
        for fact in &self.facts {
            match fact.body() {
                SessionFactBody::ModelEvent {
                    event:
                        LanguageEvent::ContentDelta {
                            delta: ContentDelta::Text(text),
                            ..
                        },
                    ..
                } => output.push_str(text),
                SessionFactBody::ToolResult { result, .. } => {
                    for content in &result.content {
                        if let ToolContent::Image { media } = content {
                            append_line(&mut output, &format!("media:{}", media.id));
                        }
                    }
                }
                SessionFactBody::ImageOutput { media, .. } => {
                    append_line(&mut output, &format!("media:{}", media.id));
                }
                _ => {}
            }
        }
        output
    }

    /// Renders version-2 JSONL envelopes without replacing raw Fact payloads.
    pub fn jsonl_output(&self) -> std::result::Result<String, serde_json::Error> {
        let mut lines = Vec::with_capacity(self.facts.len() + 2);
        lines.push(serde_json::to_string(&SessionEnvelope {
            version: 2,
            kind: "session",
            session_id: &self.completion.session_id,
            turn_id: &self.completion.turn_id,
            accepted_seq: self.completion.accepted_seq,
        })?);
        for fact in &self.facts {
            lines.push(serde_json::to_string(&LiveFactEnvelope {
                version: 2,
                kind: "fact",
                session_id: &self.completion.session_id,
                fact: fact.as_ref(),
                durable_seq: self.completion.durable_seq,
            })?);
        }
        lines.push(serde_json::to_string(&OutcomeEnvelope {
            version: 2,
            kind: "outcome",
            session_id: &self.completion.session_id,
            turn_id: &self.completion.turn_id,
            outcome: &self.completion.outcome,
            durable_seq: self.completion.durable_seq,
        })?);
        Ok(format!("{}\n", lines.join("\n")))
    }
}

impl RunCompletion {
    /// Exact durable session identity.
    pub const fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Exact submitted turn identity.
    pub const fn turn_id(&self) -> &TurnId {
        &self.turn_id
    }

    /// Canonical terminal outcome.
    pub const fn outcome(&self) -> &TurnOutcome {
        &self.outcome
    }

    /// Durable watermark observed before returning.
    pub const fn durable_seq(&self) -> u64 {
        self.durable_seq
    }

    /// Whether the caller-owned cancellation token fired during the turn.
    pub const fn cancellation_requested(&self) -> bool {
        self.cancellation_requested
    }

    /// Stable process exit code for this completion.
    pub const fn exit_code(&self) -> u8 {
        if self.cancellation_requested {
            130
        } else if matches!(self.outcome, TurnOutcome::Completed) {
            0
        } else {
            1
        }
    }
}

impl RunningRsi {
    /// Submits and observes exactly one turn until terminal durability.
    pub async fn run_turn(
        &self,
        options: RunOptions,
        cancellation: CancellationToken,
    ) -> Result<RunReport> {
        let mut facts = Vec::new();
        let completion = self
            .run_turn_observed(options, cancellation, |event| {
                if let RunEvent::Fact { fact, .. } = event {
                    facts.push(Arc::clone(fact));
                }
                Ok(())
            })
            .await?;
        Ok(RunReport { completion, facts })
    }

    /// Submits and observes exactly one direct Image turn until terminal durability.
    pub async fn run_image(
        &self,
        options: RunImageOptions,
        cancellation: CancellationToken,
    ) -> Result<RunReport> {
        let turns = required::<TurnServiceContract>(&self.host, "Agent Turn service")?;
        let image = required::<ImageCallContract>(&self.host, "Image router")?;
        let workspace = required::<WorkspaceRegistryContract>(&self.host, "Workspace registry")?;
        let settings = required::<AgentSettingsContract>(&self.host, "Headless Agent Settings")?;
        let composition =
            required::<AgentCompositionContract>(&self.host, "Agent composition service")?;
        options
            .model
            .validate()
            .map_err(|error| RsiError::Boot(error.to_string()))?;
        options
            .request
            .validate()
            .map_err(|error| RsiError::Boot(error.to_string()))?;
        image
            .describe(&options.model)
            .map_err(|error| RsiError::Boot(error.to_string()))?;
        let session = resolve_session(
            options.session,
            turns.as_ref(),
            None,
            workspace.as_ref(),
            settings.as_ref(),
            composition,
        )
        .await?;
        let submitted = turns
            .submit_image(SubmitImage {
                session,
                model: options.model,
                request: options.request,
            })
            .await
            .map_err(|error| RsiError::Run(error.to_string()))?;
        let mut observation = turns
            .observe(
                &submitted.session_id,
                submitted.accepted_seq.saturating_sub(1),
            )
            .await
            .map_err(|error| RsiError::Run(error.to_string()))?;
        let mut facts = Vec::new();
        let mut observer = |event: &RunEvent| {
            if let RunEvent::Fact { fact, .. } = event {
                facts.push(Arc::clone(fact));
            }
            Ok(())
        };
        let completion = observe_turn(
            turns.as_ref(),
            submitted,
            &mut observation,
            cancellation,
            &mut observer,
        )
        .await?;
        Ok(RunReport { completion, facts })
    }

    /// Runs one turn while synchronously publishing its live owned interval.
    ///
    /// Observer failure stops output, requests turn cancellation, and returns a
    /// runtime failure rather than silently losing part of the stream.
    pub async fn run_turn_observed<F>(
        &self,
        options: RunOptions,
        cancellation: CancellationToken,
        mut observer: F,
    ) -> Result<RunCompletion>
    where
        F: FnMut(&RunEvent) -> Result<()>,
    {
        validate_task(&options.task)?;
        let turns = required::<TurnServiceContract>(&self.host, "Agent Turn service")?;
        let language = required::<LanguageCallContract>(&self.host, "Language router")?;
        let workspace = required::<WorkspaceRegistryContract>(&self.host, "Workspace registry")?;
        let settings = required::<AgentSettingsContract>(&self.host, "Headless Agent Settings")?;
        let composition =
            required::<AgentCompositionContract>(&self.host, "Agent composition service")?;

        if let Some(model) = &options.model {
            language
                .describe(model)
                .map_err(|error| RsiError::Boot(error.to_string()))?;
        }

        let session = resolve_session(
            options.session,
            turns.as_ref(),
            Some(language.as_ref()),
            workspace.as_ref(),
            settings.as_ref(),
            composition,
        )
        .await?;

        let submitted = turns
            .submit(SubmitTurn {
                session,
                text: options.task,
                model: options.model,
                sandbox: options.sandbox,
            })
            .await
            .map_err(|error| RsiError::Run(error.to_string()))?;
        if let Err(error) = observer(&RunEvent::Session {
            session_id: submitted.session_id.clone(),
            turn_id: submitted.turn_id.clone(),
            accepted_seq: submitted.accepted_seq,
        }) {
            let _ignored = request_cancel(
                turns.as_ref(),
                &submitted,
                "run observer rejected the session event",
            )
            .await;
            return Err(error);
        }
        let mut observation = match turns
            .observe(
                &submitted.session_id,
                submitted.accepted_seq.saturating_sub(1),
            )
            .await
        {
            Ok(observation) => observation,
            Err(error) => {
                let _ignored = request_cancel(
                    turns.as_ref(),
                    &submitted,
                    "run could not establish turn observation",
                )
                .await;
                return Err(RsiError::Run(error.to_string()));
            }
        };

        observe_turn(
            turns.as_ref(),
            submitted,
            &mut observation,
            cancellation,
            &mut observer,
        )
        .await
    }
}

async fn resolve_session(
    selection: SessionSelection,
    turns: &dyn TurnService,
    language: Option<&dyn LanguageCall>,
    workspace: &dyn WorkspaceRegistry,
    settings: &dyn AgentSettings,
    composition: Arc<dyn AgentComposition>,
) -> Result<SubmitSession> {
    match selection {
        SessionSelection::Fresh {
            cwd,
            session_id,
            agent_preset_id,
        } => {
            let agent_preset_id = match agent_preset_id {
                Some(agent_preset_id) => agent_preset_id,
                None => composition
                    .default_preset_id()
                    .await
                    .map_err(|error| RsiError::Boot(error.to_string()))?,
            };
            let cwd = canonical_directory(&cwd).await?;
            let profile = settings.profile().clone();
            if let Some(language) = language {
                language
                    .describe(profile.default_model())
                    .map_err(|error| RsiError::Boot(error.to_string()))?;
            }
            let session_id = match session_id {
                Some(session_id) => session_id,
                None => generated_session_id()?,
            };
            let canonical_cwd = cwd
                .to_str()
                .ok_or_else(|| RsiError::Boot("canonical workspace path is not UTF-8".into()))?;
            let header = SessionHeader::new(
                session_id,
                now_ms()?,
                canonical_cwd,
                agent_preset_id,
                profile,
            )
            .map_err(|error| RsiError::Boot(error.to_string()))?;
            let draft = AgentSessionDraft::new(header, composition)
                .await
                .map_err(|error| RsiError::Boot(error.to_string()))?;
            workspace
                .get_or_create(&cwd)
                .await
                .map_err(|error| RsiError::Boot(error.to_string()))?;
            Ok(SubmitSession::Fresh(draft.into_fresh()))
        }
        SessionSelection::Resume { session_id, cwd } => {
            let prepared = turns
                .prepare_resume(&session_id)
                .await
                .map_err(|error| RsiError::Boot(error.to_string()))?;
            let header = prepared.header();
            let durable_cwd = canonical_directory(Path::new(header.canonical_cwd())).await?;
            if durable_cwd.to_str() != Some(header.canonical_cwd()) {
                return Err(RsiError::Boot(
                    "durable session workspace no longer resolves to its canonical path".into(),
                ));
            }
            if let Some(candidate) = cwd {
                let candidate = canonical_directory(&candidate).await?;
                if candidate != durable_cwd {
                    return Err(RsiError::Boot(
                        "--cwd does not match the resumed session workspace".into(),
                    ));
                }
            }
            if let Some(language) = language {
                language
                    .describe(header.profile().default_model())
                    .map_err(|error| RsiError::Boot(error.to_string()))?;
            }
            workspace
                .get_or_create(&durable_cwd)
                .await
                .map_err(|error| RsiError::Boot(error.to_string()))?;
            Ok(SubmitSession::Resume(prepared))
        }
    }
}

async fn observe_turn(
    turns: &dyn TurnService,
    submitted: rsi_agent_turn_protocol::SubmittedTurn,
    observation: &mut rsi_agent_turn_protocol::TurnObservation,
    cancellation: CancellationToken,
    observer: &mut impl FnMut(&RunEvent) -> Result<()>,
) -> Result<RunCompletion> {
    let mut durable_seq = submitted.accepted_seq.saturating_sub(1);
    let mut terminal: Option<(u64, TurnOutcome)> = None;
    let mut cancel_requested = false;
    loop {
        if let Some((terminal_seq, outcome)) = &terminal
            && durable_seq >= *terminal_seq
        {
            let stored_outcome = turns
                .outcome(&submitted.session_id, &submitted.turn_id)
                .await
                .map_err(|error| RsiError::Run(error.to_string()))?;
            if stored_outcome.as_ref() != Some(outcome) {
                return Err(RsiError::Run(
                    "terminal Fact and Turn outcome observation disagree".into(),
                ));
            }
            let completion = RunCompletion {
                session_id: submitted.session_id,
                turn_id: submitted.turn_id,
                accepted_seq: submitted.accepted_seq,
                outcome: outcome.clone(),
                durable_seq,
                cancellation_requested: cancel_requested,
            };
            observer(&RunEvent::Outcome {
                session_id: completion.session_id.clone(),
                turn_id: completion.turn_id.clone(),
                outcome: completion.outcome.clone(),
                durable_seq,
            })?;
            return Ok(completion);
        }

        let update = tokio::select! {
            update = observation.next() => update,
            () = cancellation.cancelled(), if !cancel_requested => {
                request_cancel(turns, &submitted, "run cancellation requested").await?;
                cancel_requested = true;
                continue;
            }
        };
        let update = update
            .ok_or_else(|| RsiError::Run("Agent observation ended before terminal Fact".into()))?
            .map_err(|error| RsiError::Run(error.to_string()))?;
        match update {
            TurnUpdate::Fact {
                fact,
                durable_seq: observed_durable,
            } => {
                durable_seq = durable_seq.max(observed_durable);
                if let SessionFactBody::TurnTerminal { turn_id, outcome } = fact.body()
                    && turn_id == &submitted.turn_id
                {
                    if observed_durable < fact.seq() {
                        return Err(RsiError::Run(
                            "Agent published a terminal Fact before its prefix was durable".into(),
                        ));
                    }
                    terminal = Some((fact.seq(), outcome.clone()));
                }
                if let Err(error) = observer(&RunEvent::Fact {
                    session_id: submitted.session_id.clone(),
                    fact: Arc::clone(&fact),
                    durable_seq: observed_durable,
                }) {
                    let _ignored =
                        request_cancel(turns, &submitted, "run observer rejected a Fact event")
                            .await;
                    return Err(error);
                }
            }
            TurnUpdate::Durable {
                durable_seq: observed_durable,
            } => durable_seq = durable_seq.max(observed_durable),
        }
    }
}

async fn request_cancel(
    turns: &dyn TurnService,
    submitted: &rsi_agent_turn_protocol::SubmittedTurn,
    reason: &str,
) -> Result<()> {
    turns
        .cancel(
            &submitted.session_id,
            &submitted.turn_id,
            Some(reason.into()),
        )
        .await
        .map(|_| ())
        .map_err(|error| RsiError::Run(error.to_string()))
}

fn required<C: rsi_meta::LocalContract>(
    host: &rsi_host::RunningHost,
    name: &str,
) -> Result<std::sync::Arc<C::Service>> {
    host.lookup_local::<C>()
        .ok_or_else(|| RsiError::Boot(format!("{name} is unavailable")))
}

async fn canonical_directory(path: &Path) -> Result<PathBuf> {
    let canonical = tokio::fs::canonicalize(path)
        .await
        .map_err(|error| RsiError::Boot(format!("workspace `{}`: {error}", path.display())))?;
    let metadata = tokio::fs::metadata(&canonical)
        .await
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    if !metadata.is_dir() {
        return Err(RsiError::Boot("workspace path is not a directory".into()));
    }
    Ok(canonical)
}

fn generated_session_id() -> Result<SessionId> {
    let mut entropy = [0_u8; 16];
    getrandom::fill(&mut entropy)
        .map_err(|error| RsiError::Run(format!("OS entropy failed: {error}")))?;
    SessionId::new(format!("session-{:032x}", u128::from_le_bytes(entropy)))
        .map_err(|error| RsiError::Run(error.to_string()))
}

fn now_ms() -> Result<u64> {
    let value = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| RsiError::Run(error.to_string()))?;
    Ok(u64::try_from(value.as_millis()).unwrap_or(u64::MAX).max(1))
}

fn validate_task(task: &str) -> Result<()> {
    if task.is_empty() || task.len() > rsi_agent_session_protocol::MAXIMUM_TURN_TEXT_BYTES {
        return Err(RsiError::Boot(format!(
            "task must contain 1..={} UTF-8 bytes",
            rsi_agent_session_protocol::MAXIMUM_TURN_TEXT_BYTES
        )));
    }
    Ok(())
}

fn append_line(output: &mut String, line: &str) {
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    output.push_str(line);
    output.push('\n');
}

#[derive(Serialize)]
struct SessionEnvelope<'a> {
    version: u32,
    #[serde(rename = "type")]
    kind: &'static str,
    session_id: &'a SessionId,
    turn_id: &'a TurnId,
    accepted_seq: u64,
}

#[derive(Serialize)]
struct LiveFactEnvelope<'a> {
    version: u32,
    #[serde(rename = "type")]
    kind: &'static str,
    session_id: &'a SessionId,
    fact: &'a SessionFact,
    durable_seq: u64,
}

#[derive(Serialize)]
struct OutcomeEnvelope<'a> {
    version: u32,
    #[serde(rename = "type")]
    kind: &'static str,
    session_id: &'a SessionId,
    turn_id: &'a TurnId,
    outcome: &'a TurnOutcome,
    durable_seq: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use rsi_agent_composition_protocol::{
        AgentCompositionError, AgentCompositionPin, AgentGenerationOwner,
    };
    use rsi_agent_presets::{AgentPresetCatalog, AgentPresetCatalogConfig, COMPOSITION_FILE};
    use rsi_agent_session_protocol::FrozenAgentProfile;
    use rsi_agent_store_protocol::{AppendBatch, SessionStore as _};
    use rsi_agent_store_sqlite::SqliteStore;
    use rsi_agent_turn_protocol::{
        CancelResult, PreparedResumeSession, ResumeAdmissionIssuer, SubmittedTurn, TurnError,
        TurnObservation,
    };
    use rsi_tools_protocol::{
        PreparedToolCall, RetainedToolResult, ToolCall, ToolDefinition, ToolError,
        ToolResultIdentity, ToolRuntime,
    };
    use rsi_workspace::{
        WorkspaceError, WorkspaceId, WorkspaceRecord, WorkspaceRegistryContract, WorkspaceStatus,
    };
    use std::collections::BTreeMap;
    use std::fmt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct EmptyTools;

    #[async_trait]
    impl ToolRuntime for EmptyTools {
        fn definitions(&self) -> Vec<ToolDefinition> {
            Vec::new()
        }

        fn prepare(
            &self,
            _invocation_id: &str,
            _call: ToolCall,
        ) -> rsi_tools_protocol::Result<Box<dyn PreparedToolCall>> {
            Err(ToolError::Execution("unused test Tool catalog".into()))
        }

        fn query(
            &self,
            _identity: &ToolResultIdentity,
        ) -> rsi_tools_protocol::Result<RetainedToolResult> {
            Err(ToolError::Execution("unused test Tool catalog".into()))
        }

        async fn wait(
            &self,
            _identity: &ToolResultIdentity,
            _cancellation: CancellationToken,
        ) -> rsi_tools_protocol::Result<RetainedToolResult> {
            Err(ToolError::Execution("unused test Tool catalog".into()))
        }

        fn commit(&self, _identity: &ToolResultIdentity) -> rsi_tools_protocol::Result<()> {
            Err(ToolError::Execution("unused test Tool catalog".into()))
        }
    }

    #[derive(Debug)]
    struct DropOwner(Arc<AtomicUsize>);

    impl Drop for DropOwner {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn test_pin(
        preset_id: &AgentPresetId,
        owner: Arc<dyn AgentGenerationOwner>,
    ) -> AgentCompositionPin {
        AgentCompositionPin::new(
            preset_id.clone(),
            "a".repeat(64),
            Arc::new(EmptyTools),
            owner,
        )
        .unwrap()
    }

    #[derive(Debug)]
    struct FailingComposition;

    #[async_trait]
    impl AgentComposition for FailingComposition {
        async fn default_preset_id(
            &self,
        ) -> std::result::Result<AgentPresetId, AgentCompositionError> {
            Ok(AgentPresetId::new("standard").unwrap())
        }

        async fn pin(
            &self,
            preset_id: &AgentPresetId,
        ) -> std::result::Result<AgentCompositionPin, AgentCompositionError> {
            Err(AgentCompositionError::Unavailable {
                preset_id: preset_id.clone(),
                reason: "test preset source is unavailable".into(),
            })
        }
    }

    #[derive(Debug)]
    struct TrackingComposition {
        drops: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl AgentComposition for TrackingComposition {
        async fn default_preset_id(
            &self,
        ) -> std::result::Result<AgentPresetId, AgentCompositionError> {
            Ok(AgentPresetId::new("standard").unwrap())
        }

        async fn pin(
            &self,
            preset_id: &AgentPresetId,
        ) -> std::result::Result<AgentCompositionPin, AgentCompositionError> {
            Ok(test_pin(
                preset_id,
                Arc::new(DropOwner(Arc::clone(&self.drops))),
            ))
        }
    }

    #[derive(Debug)]
    struct StubSettings(FrozenAgentProfile);

    impl AgentSettings for StubSettings {
        fn profile(&self) -> &FrozenAgentProfile {
            &self.0
        }
    }

    #[derive(Debug)]
    struct RecordingWorkspace {
        get_or_create_calls: AtomicUsize,
    }

    impl RecordingWorkspace {
        fn new() -> Self {
            Self {
                get_or_create_calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl WorkspaceRegistry for RecordingWorkspace {
        async fn list(&self) -> Vec<WorkspaceRecord> {
            Vec::new()
        }

        async fn get_or_create(
            &self,
            _path: &Path,
        ) -> std::result::Result<WorkspaceRecord, WorkspaceError> {
            self.get_or_create_calls.fetch_add(1, Ordering::AcqRel);
            Err(WorkspaceError::Storage(
                "test workspace registration rejected".into(),
            ))
        }

        async fn status(
            &self,
            _id: &WorkspaceId,
        ) -> std::result::Result<WorkspaceStatus, WorkspaceError> {
            Err(WorkspaceError::InvalidInput("unused test operation".into()))
        }

        async fn delete_registration(
            &self,
            _id: &WorkspaceId,
        ) -> std::result::Result<bool, WorkspaceError> {
            Err(WorkspaceError::InvalidInput("unused test operation".into()))
        }
    }

    struct StubTurns {
        header: SessionHeader,
        issuer: ResumeAdmissionIssuer,
        drops: Arc<AtomicUsize>,
        reject_preparation: bool,
    }

    impl fmt::Debug for StubTurns {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.debug_struct("StubTurns").finish_non_exhaustive()
        }
    }

    #[async_trait]
    impl TurnService for StubTurns {
        async fn prepare_resume(
            &self,
            _session_id: &SessionId,
        ) -> rsi_agent_turn_protocol::Result<PreparedResumeSession> {
            if self.reject_preparation {
                return Err(TurnError::Composition(
                    "test preset source is unavailable".into(),
                ));
            }
            self.issuer.issue(
                self.header.clone(),
                test_pin(
                    self.header.agent_preset_id(),
                    Arc::new(DropOwner(Arc::clone(&self.drops))),
                ),
            )
        }

        async fn submit(
            &self,
            _request: SubmitTurn,
        ) -> rsi_agent_turn_protocol::Result<SubmittedTurn> {
            unreachable!("resolve_session does not submit")
        }

        async fn submit_image(
            &self,
            _request: SubmitImage,
        ) -> rsi_agent_turn_protocol::Result<SubmittedTurn> {
            unreachable!("resolve_session does not submit")
        }

        async fn cancel(
            &self,
            _session_id: &SessionId,
            _turn_id: &TurnId,
            _reason: Option<String>,
        ) -> rsi_agent_turn_protocol::Result<CancelResult> {
            unreachable!("resolve_session does not cancel")
        }

        async fn observe(
            &self,
            _session_id: &SessionId,
            _after_seq: u64,
        ) -> rsi_agent_turn_protocol::Result<TurnObservation> {
            unreachable!("resolve_session does not observe")
        }

        async fn outcome(
            &self,
            _session_id: &SessionId,
            _turn_id: &TurnId,
        ) -> rsi_agent_turn_protocol::Result<Option<TurnOutcome>> {
            unreachable!("resolve_session does not read outcomes")
        }

        async fn session_header(
            &self,
            _session_id: &SessionId,
        ) -> rsi_agent_turn_protocol::Result<SessionHeader> {
            unreachable!("resume preparation owns the Header read")
        }
    }

    fn profile() -> FrozenAgentProfile {
        FrozenAgentProfile::new(
            "headless",
            "system",
            ModelRef::new("deployment", "model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap()
    }

    fn resume_header(cwd: &Path) -> SessionHeader {
        SessionHeader::new(
            SessionId::new("session-resume-preparation").unwrap(),
            1,
            cwd.to_str().unwrap(),
            AgentPresetId::new("standard").unwrap(),
            profile(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn fresh_generation_failure_precedes_workspace_registration() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = RecordingWorkspace::new();
        let settings = StubSettings(profile());
        let turns = StubTurns {
            header: resume_header(temporary.path()),
            issuer: ResumeAdmissionIssuer::new(),
            drops: Arc::new(AtomicUsize::new(0)),
            reject_preparation: true,
        };

        let result = resolve_session(
            SessionSelection::Fresh {
                cwd: temporary.path().into(),
                session_id: Some(SessionId::new("session-fresh-preparation").unwrap()),
                agent_preset_id: Some(AgentPresetId::new("missing-preset").unwrap()),
            },
            &turns,
            None,
            &workspace,
            &settings,
            Arc::new(FailingComposition),
        )
        .await;

        assert!(matches!(result, Err(RsiError::Boot(_))));
        assert_eq!(workspace.get_or_create_calls.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn cold_resume_generation_failure_precedes_workspace_registration() {
        let temporary = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(temporary.path()).unwrap();
        let workspace = RecordingWorkspace::new();
        let settings = StubSettings(profile());
        let turns = StubTurns {
            header: resume_header(&canonical),
            issuer: ResumeAdmissionIssuer::new(),
            drops: Arc::new(AtomicUsize::new(0)),
            reject_preparation: true,
        };

        let result = resolve_session(
            SessionSelection::Resume {
                session_id: turns.header.session_id().clone(),
                cwd: Some(canonical),
            },
            &turns,
            None,
            &workspace,
            &settings,
            Arc::new(FailingComposition),
        )
        .await;

        assert!(matches!(result, Err(RsiError::Boot(_))));
        assert_eq!(workspace.get_or_create_calls.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn workspace_failure_drops_the_unsubmitted_resume_pin() {
        let temporary = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(temporary.path()).unwrap();
        let workspace = RecordingWorkspace::new();
        let settings = StubSettings(profile());
        let drops = Arc::new(AtomicUsize::new(0));
        let turns = StubTurns {
            header: resume_header(&canonical),
            issuer: ResumeAdmissionIssuer::new(),
            drops: Arc::clone(&drops),
            reject_preparation: false,
        };

        let result = resolve_session(
            SessionSelection::Resume {
                session_id: turns.header.session_id().clone(),
                cwd: Some(canonical),
            },
            &turns,
            None,
            &workspace,
            &settings,
            Arc::new(FailingComposition),
        )
        .await;

        assert!(matches!(result, Err(RsiError::Boot(_))));
        assert_eq!(workspace.get_or_create_calls.load(Ordering::Acquire), 1);
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn workspace_failure_drops_the_unsubmitted_fresh_pin() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = RecordingWorkspace::new();
        let settings = StubSettings(profile());
        let drops = Arc::new(AtomicUsize::new(0));
        let turns = StubTurns {
            header: resume_header(temporary.path()),
            issuer: ResumeAdmissionIssuer::new(),
            drops: Arc::new(AtomicUsize::new(0)),
            reject_preparation: true,
        };

        let result = resolve_session(
            SessionSelection::Fresh {
                cwd: temporary.path().into(),
                session_id: Some(SessionId::new("session-fresh-workspace-failure").unwrap()),
                agent_preset_id: Some(AgentPresetId::new("standard").unwrap()),
            },
            &turns,
            None,
            &workspace,
            &settings,
            Arc::new(TrackingComposition {
                drops: Arc::clone(&drops),
            }),
        )
        .await;

        assert!(matches!(result, Err(RsiError::Boot(_))));
        assert_eq!(workspace.get_or_create_calls.load(Ordering::Acquire), 1);
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }

    enum PresetSourceDamage {
        Delete,
        InvalidToml,
    }

    struct ColdProductFixture {
        paths: rsi_host::HostPaths,
        profile_path: PathBuf,
        canonical_workspace: PathBuf,
        preset_directory: PathBuf,
        catalog: AgentPresetCatalog,
        header: SessionHeader,
    }

    fn cold_product_fixture(root: &Path) -> ColdProductFixture {
        let config = root.join("config");
        let state = root.join("state");
        let cache = root.join("cache");
        let workspace_path = root.join("workspace");
        let preset_root = root.join("presets");
        let preset_directory = preset_root.join("recoverable");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir(&workspace_path).unwrap();
        std::fs::create_dir_all(&preset_directory).unwrap();
        std::fs::write(preset_directory.join(COMPOSITION_FILE), "format = 1\n").unwrap();
        std::fs::write(
            config.join("settings.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "rsi.agent": {
                    "default_model": {
                        "deployment": "unused",
                        "model": "unused"
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let profile_path = config.join("profile.toml");
        std::fs::write(&profile_path, "format = 1\n").unwrap();
        let paths = rsi_host::HostPaths::new(config, state, cache).unwrap();
        let preset_id = AgentPresetId::new("recoverable").unwrap();
        let catalog = AgentPresetCatalog::new(
            AgentPresetCatalogConfig::new(preset_id.clone()).with_system_root(&preset_root),
            crate::agent_preset::standard_agent_profile_compiler(&paths, false).unwrap(),
        )
        .unwrap();
        let canonical_workspace = std::fs::canonicalize(&workspace_path).unwrap();
        let header = SessionHeader::new(
            SessionId::new("session-product-cold-preset").unwrap(),
            1,
            canonical_workspace.to_str().unwrap(),
            preset_id,
            profile(),
        )
        .unwrap();
        ColdProductFixture {
            paths,
            profile_path,
            canonical_workspace,
            preset_directory,
            catalog,
            header,
        }
    }

    async fn append_terminal_product_session(
        paths: &rsi_host::HostPaths,
        header: &SessionHeader,
    ) -> Vec<SessionFact> {
        let turn_id = TurnId::new("turn-product-cold-preset").unwrap();
        let facts = vec![
            SessionFact::new(
                1,
                1,
                SessionFactBody::TurnAccepted {
                    turn_id: turn_id.clone(),
                    text: "already complete".into(),
                    model: None,
                    sandbox: SandboxMode::WorkspaceWrite,
                    require_approval: false,
                },
            )
            .unwrap(),
            SessionFact::new(
                2,
                2,
                SessionFactBody::TurnTerminal {
                    turn_id,
                    outcome: TurnOutcome::Completed,
                },
            )
            .unwrap(),
        ];
        let store = SqliteStore::open(paths.state().join("agent")).unwrap();
        store
            .append(AppendBatch {
                session_id: header.session_id().clone(),
                expected_seq: 0,
                header: Some(header.clone()),
                facts: facts.clone(),
            })
            .await
            .unwrap();
        drop(store);
        facts
    }

    async fn assert_cold_preset_damage_has_no_product_side_effect(damage: PresetSourceDamage) {
        let temporary = tempfile::tempdir().unwrap();
        let fixture = cold_product_fixture(temporary.path());
        let facts = append_terminal_product_session(&fixture.paths, &fixture.header).await;

        let running = RunningRsi::boot(
            crate::StandardComposition::new(fixture.paths.clone(), BTreeMap::new(), None)
                .with_agent_presets(fixture.catalog),
            &fixture.profile_path,
        )
        .await
        .unwrap();
        let workspaces = running
            .host
            .lookup_local::<WorkspaceRegistryContract>()
            .unwrap();
        assert!(workspaces.list().await.is_empty());

        match damage {
            PresetSourceDamage::Delete => {
                std::fs::remove_dir_all(&fixture.preset_directory).unwrap();
            }
            PresetSourceDamage::InvalidToml => {
                std::fs::write(
                    fixture.preset_directory.join(COMPOSITION_FILE),
                    "format = not-valid-toml\n",
                )
                .unwrap();
            }
        }
        let result = running
            .run_turn(
                RunOptions {
                    task: "must not start".into(),
                    session: SessionSelection::Resume {
                        session_id: fixture.header.session_id().clone(),
                        cwd: Some(fixture.canonical_workspace),
                    },
                    model: None,
                    sandbox: None,
                    output: OutputMode::Text,
                },
                CancellationToken::new(),
            )
            .await;
        let error = result.unwrap_err().to_string();
        assert!(error.contains("recoverable"), "error: {error}");
        assert!(error.contains("unavailable"), "error: {error}");
        assert!(workspaces.list().await.is_empty());
        drop(workspaces);
        assert!(running.shutdown().await.is_clean());

        let reopened = SqliteStore::open(fixture.paths.state().join("agent")).unwrap();
        assert_eq!(
            reopened.header(fixture.header.session_id()).await.unwrap(),
            fixture.header
        );
        let stored = reopened
            .read_facts(fixture.header.session_id(), 0, 8)
            .await
            .unwrap();
        assert_eq!(stored.facts, facts);
        assert!(stored.caught_up());
    }

    #[tokio::test]
    async fn deleted_cold_preset_has_no_workspace_or_session_store_side_effect() {
        assert_cold_preset_damage_has_no_product_side_effect(PresetSourceDamage::Delete).await;
    }

    #[tokio::test]
    async fn invalid_cold_preset_has_no_workspace_or_session_store_side_effect() {
        assert_cold_preset_damage_has_no_product_side_effect(PresetSourceDamage::InvalidToml).await;
    }
}
