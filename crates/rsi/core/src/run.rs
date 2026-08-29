use crate::settings::{AgentSettings, AgentSettingsContract};
use crate::{Result, RsiError, RunningRsi};
use futures_util::StreamExt as _;
use rsi_agent_session_protocol::{
    SessionFact, SessionFactBody, SessionHeader, SessionId, TurnId, TurnOutcome,
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
    session_id: SessionId,
    turn_id: TurnId,
    accepted_seq: u64,
    facts: Vec<SessionFact>,
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
        fact: Box<SessionFact>,
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
    /// Encodes one version-1 JSONL envelope.
    pub fn json_line(&self) -> std::result::Result<String, serde_json::Error> {
        match self {
            Self::Session {
                session_id,
                turn_id,
                accepted_seq,
            } => serde_json::to_string(&SessionEnvelope {
                version: 1,
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
                version: 1,
                kind: "fact",
                session_id,
                fact,
                durable_seq: *durable_seq,
            }),
            Self::Outcome {
                session_id,
                turn_id,
                outcome,
                durable_seq,
            } => serde_json::to_string(&OutcomeEnvelope {
                version: 1,
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
        &self.session_id
    }

    /// Exact submitted turn identity.
    pub const fn turn_id(&self) -> &TurnId {
        &self.turn_id
    }

    /// Raw Facts observed in this process-owned interval.
    pub fn facts(&self) -> &[SessionFact] {
        &self.facts
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

    /// Stable process exit code for this completed report.
    pub const fn exit_code(&self) -> u8 {
        if self.cancellation_requested {
            130
        } else if matches!(self.outcome, TurnOutcome::Completed) {
            0
        } else {
            1
        }
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

    /// Renders version-1 JSONL envelopes without replacing raw Fact payloads.
    ///
    /// Every Fact carries the report's final durable watermark. Live
    /// [`RunEvent::Fact`] envelopes use the same shape with their publication-time
    /// watermark instead.
    pub fn jsonl_output(&self) -> std::result::Result<String, serde_json::Error> {
        let mut lines = Vec::with_capacity(self.facts.len() + 2);
        lines.push(serde_json::to_string(&SessionEnvelope {
            version: 1,
            kind: "session",
            session_id: &self.session_id,
            turn_id: &self.turn_id,
            accepted_seq: self.accepted_seq,
        })?);
        for fact in &self.facts {
            lines.push(serde_json::to_string(&LiveFactEnvelope {
                version: 1,
                kind: "fact",
                session_id: &self.session_id,
                fact,
                durable_seq: self.durable_seq,
            })?);
        }
        lines.push(serde_json::to_string(&OutcomeEnvelope {
            version: 1,
            kind: "outcome",
            session_id: &self.session_id,
            turn_id: &self.turn_id,
            outcome: &self.outcome,
            durable_seq: self.durable_seq,
        })?);
        Ok(format!("{}\n", lines.join("\n")))
    }
}

impl RunningRsi {
    /// Submits and observes exactly one turn until terminal durability.
    pub async fn run_turn(
        &self,
        options: RunOptions,
        cancellation: CancellationToken,
    ) -> Result<RunReport> {
        self.run_turn_observed(options, cancellation, |_| Ok(()))
            .await
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
        let mut observer = |_event: &RunEvent| Ok(());
        observe_turn(
            turns.as_ref(),
            submitted,
            &mut observation,
            cancellation,
            &mut observer,
        )
        .await
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
    ) -> Result<RunReport>
    where
        F: FnMut(&RunEvent) -> Result<()>,
    {
        validate_task(&options.task)?;
        let turns = required::<TurnServiceContract>(&self.host, "Agent Turn service")?;
        let language = required::<LanguageCallContract>(&self.host, "Language router")?;
        let workspace = required::<WorkspaceRegistryContract>(&self.host, "Workspace registry")?;
        let settings = required::<AgentSettingsContract>(&self.host, "Headless Agent Settings")?;

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
) -> Result<SubmitSession> {
    match selection {
        SessionSelection::Fresh { cwd, session_id } => {
            let cwd = canonical_directory(&cwd).await?;
            workspace
                .get_or_create(&cwd)
                .await
                .map_err(|error| RsiError::Boot(error.to_string()))?;
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
            let cwd = cwd
                .to_str()
                .ok_or_else(|| RsiError::Boot("canonical workspace path is not UTF-8".into()))?;
            Ok(SubmitSession::Fresh(
                SessionHeader::new(session_id, now_ms()?, cwd, profile)
                    .map_err(|error| RsiError::Boot(error.to_string()))?,
            ))
        }
        SessionSelection::Resume { session_id, cwd } => {
            let header = turns
                .session_header(&session_id)
                .await
                .map_err(|error| RsiError::Boot(error.to_string()))?;
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
            workspace
                .get_or_create(&durable_cwd)
                .await
                .map_err(|error| RsiError::Boot(error.to_string()))?;
            if let Some(language) = language {
                language
                    .describe(header.profile().default_model())
                    .map_err(|error| RsiError::Boot(error.to_string()))?;
            }
            Ok(SubmitSession::Resume(session_id))
        }
    }
}

async fn observe_turn(
    turns: &dyn TurnService,
    submitted: rsi_agent_turn_protocol::SubmittedTurn,
    observation: &mut rsi_agent_turn_protocol::TurnObservation,
    cancellation: CancellationToken,
    observer: &mut impl FnMut(&RunEvent) -> Result<()>,
) -> Result<RunReport> {
    let mut facts = Vec::new();
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
            let report = RunReport {
                session_id: submitted.session_id,
                turn_id: submitted.turn_id,
                accepted_seq: submitted.accepted_seq,
                facts,
                outcome: outcome.clone(),
                durable_seq,
                cancellation_requested: cancel_requested,
            };
            observer(&RunEvent::Outcome {
                session_id: report.session_id.clone(),
                turn_id: report.turn_id.clone(),
                outcome: report.outcome.clone(),
                durable_seq,
            })?;
            return Ok(report);
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
                let fact = *fact;
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
                    fact: Box::new(fact.clone()),
                    durable_seq: observed_durable,
                }) {
                    let _ignored =
                        request_cancel(turns, &submitted, "run observer rejected a Fact event")
                            .await;
                    return Err(error);
                }
                facts.push(fact);
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
