//! A single definition source for model discovery, human previews and named spawn.
use super::*;
use rsi_agent_composition_protocol::{
    BeforeStepContext, BeforeStepContributor, BeforeStepOutput, ContributionContext,
    ContributionError, ContributionKind, ContributionRegistrarContract, ContributionRegistration,
    ContributionResult, SessionResourceReader,
};
use rsi_agent_session_protocol::{
    AgentMessageContent, ContributionId, InputMessageSource, ModelSelection, SessionFactBody,
    SessionHeader, SessionResourceDescriptor, SessionResourceValue, SpawnRoleReference,
    SpawnRoleSeed,
};
use rsi_agent_turn_protocol::SpawnRoleResolver;
use rsi_agent_workspace_context::{
    WorkspaceAgentDefinition, WorkspaceContext, WorkspaceContextContract, WorkspaceContextError,
};
use std::fmt::Write as _;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(super) struct Definitions {
    source: Arc<dyn WorkspaceContext>,
    inline: Arc<BTreeMap<String, DelegationRole>>,
    mentions: MentionHistory,
}
fn error(value: impl std::fmt::Display) -> ContributionError {
    ContributionError::Invalid(value.to_string())
}
fn source_error(value: WorkspaceContextError) -> ContributionError {
    match value {
        WorkspaceContextError::Capacity => ContributionError::Capacity,
        WorkspaceContextError::Closed => ContributionError::Closed,
        value => error(value),
    }
}
impl Definitions {
    pub(super) fn install(
        plan: &ActivationPlan,
        inline: Arc<BTreeMap<String, DelegationRole>>,
    ) -> rsi_meta::Result<Arc<Self>> {
        let source = Arc::new(Self {
            source: plan.local::<WorkspaceContextContract>()?,
            inline,
            mentions: MentionHistory::default(),
        });
        let registrar = plan.local::<ContributionRegistrarContract>()?;
        let context = plan.context().registration_context()?;
        let catalog = registrar
            .register(
                &context,
                ContributionRegistration::new(
                    ContributionId::new("rsi.available-agents").expect("static ID"),
                    10,
                    ContributionKind::BeforeStep(source.clone()),
                ),
            )
            .map_err(|e| MetaError::Activation(e.to_string()))?;
        let resources = registrar
            .register(
                &context,
                ContributionRegistration::new(
                    ContributionId::new("rsi.agents").expect("static ID"),
                    0,
                    ContributionKind::ResourceRead(source.clone()),
                ),
            )
            .map_err(|e| MetaError::Activation(e.to_string()))?;
        plan.defer(
            "withdraw Agent definitions",
            Box::new(move || {
                Box::pin(async move {
                    drop(catalog);
                    drop(resources);
                    Ok(())
                })
            }),
        )?;
        Ok(source)
    }
    async fn load(
        &self,
        header: &SessionHeader,
        execution: Option<&rsi_execution::ExecutionLease>,
        id: Option<&str>,
        stop: CancellationToken,
    ) -> ContributionResult<Vec<WorkspaceAgentDefinition>> {
        let mut entries = self
            .source
            .agents(
                header,
                execution,
                id,
                &self.inline.keys().cloned().collect(),
                stop,
            )
            .await
            .map_err(source_error)?;
        for (name, role) in self
            .inline
            .iter()
            .filter(|(name, _)| id.is_none_or(|id| *name == id))
        {
            if let Some(entry) = entries.iter_mut().find(|entry| &entry.name == name) {
                entry.seed = None;
                entry.description = "Unavailable: name conflicts with an inline role".into();
            } else {
                let text = role.persona.clone().unwrap_or_default();
                let sha256 = format!(
                    "{:x}",
                    Sha256::digest(serde_json::to_vec(role).map_err(error)?)
                );
                entries.push(WorkspaceAgentDefinition {
                    name: name.clone(),
                    description: format!("Configured {name} agent"),
                    text,
                    source: "inline role configuration".into(),
                    seed: Some(SpawnRoleSeed {
                        reference: SpawnRoleReference {
                            provider: "rsi.agents".into(),
                            name: name.clone(),
                        },
                        role: role.clone(),
                        model: None::<ModelSelection>,
                        source: "inline role configuration".into(),
                        sha256,
                    }),
                });
            }
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }
}
#[derive(Debug, Default)]
struct MentionHistory(std::sync::Mutex<std::collections::VecDeque<MentionCursor>>);
#[derive(Clone, Debug)]
struct MentionCursor {
    header_key: String,
    turn: rsi_agent_session_protocol::TurnId,
    accepted: u64,
    through: u64,
    names: Arc<BTreeSet<String>>,
}
impl MentionHistory {
    async fn read(
        &self,
        context: &ContributionContext,
        cancellation: CancellationToken,
    ) -> ContributionResult<Arc<BTreeSet<String>>> {
        let header_key = context.header.fingerprint().map_err(error)?;
        let matches = |item: &MentionCursor| {
            item.header_key == header_key
                && item.turn == context.turn_id
                && item.accepted == context.accepted_fact_seq
        };
        let previous = self
            .0
            .lock()
            .expect("Agent mention cache")
            .iter()
            .find(|item| matches(item) && item.through <= context.horizon.fact_seq)
            .cloned();
        let (mut cursor, mut mentions) = previous.map_or_else(
            || (context.accepted_fact_seq.saturating_sub(1), Arc::default()),
            |item| (item.through, item.names),
        );
        if cursor > context.horizon.fact_seq {
            return Err(error("Agent acceptance exceeds its Fact horizon"));
        }
        while cursor < context.horizon.fact_seq {
            let page = cancellation
                .run_until_cancelled(context.facts.read(cursor, 128))
                .await
                .ok_or(ContributionError::Closed)??;
            if page.through_seq <= cursor || page.through_seq > context.horizon.fact_seq {
                return Err(error("invalid Agent mention history horizon"));
            }
            for fact in page.facts {
                if fact.body().turn_id() != &context.turn_id {
                    continue;
                }
                let texts: Vec<&str> = match fact.body() {
                    SessionFactBody::TurnAccepted { text, .. } => vec![text],
                    SessionFactBody::InputMessageClaimed {
                        source: InputMessageSource::Human { .. },
                        content,
                        ..
                    } => content
                        .iter()
                        .filter_map(|part| {
                            if let AgentMessageContent::Text { text } = part {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                for text in texts {
                    for token in rsi_agent_workspace_context::skill_input::at_tokens(text) {
                        if !mentions.contains(token.name) {
                            Arc::make_mut(&mut mentions).insert(token.name.to_owned());
                        }
                        if mentions.len() > 4096 {
                            return Err(ContributionError::Capacity);
                        }
                    }
                }
            }
            cursor = page.through_seq;
        }

        let mut cache = self.0.lock().expect("Agent mention cache");
        cache.retain(|item| !matches(item));
        if cache.len() == 32 {
            cache.pop_front();
        }
        cache.push_back(MentionCursor {
            header_key,
            turn: context.turn_id.clone(),
            accepted: context.accepted_fact_seq,
            through: cursor,
            names: mentions.clone(),
        });
        Ok(mentions)
    }
}
fn descriptor(entry: &WorkspaceAgentDefinition) -> SessionResourceDescriptor {
    SessionResourceDescriptor {
        id: entry.name.clone(),
        name: entry.name.clone(),
        description: entry.description.clone(),
        source: entry.source.clone(),
        media_type: "text/markdown".into(),
        model_readable: entry.seed.is_some(),
    }
}
#[async_trait]
impl SpawnRoleResolver for Definitions {
    async fn resolve(
        &self,
        header: &SessionHeader,
        execution: Option<&rsi_execution::ExecutionLease>,
        reference: &SpawnRoleReference,
        cancellation: CancellationToken,
    ) -> rsi_agent_turn_protocol::Result<SpawnRoleSeed> {
        if reference.provider != "rsi.agents" {
            return Err(TurnError::Invalid(
                "unknown Agent definition provider".into(),
            ));
        }
        let entry = self
            .load(header, execution, Some(&reference.name), cancellation)
            .await
            .map_err(|e| match e {
                ContributionError::Capacity => TurnError::Capacity,
                ContributionError::Closed => TurnError::ShuttingDown,
                e => TurnError::Invalid(e.to_string()),
            })?
            .into_iter()
            .next()
            .ok_or_else(|| {
                TurnError::Invalid(format!(
                    "agent '{}' is absent from the current catalog",
                    reference.name
                ))
            })?;
        entry
            .seed
            .ok_or_else(|| TurnError::Invalid(format!("{}: {}", entry.source, entry.description)))
    }
}
#[async_trait]
impl SessionResourceReader for Definitions {
    async fn read(
        &self,
        header: &SessionHeader,
        execution: Option<&rsi_execution::ExecutionLease>,
        id: Option<&str>,
        cancellation: CancellationToken,
    ) -> ContributionResult<SessionResourceValue> {
        let entries = self.load(header, execution, id, cancellation).await?;
        let value = if id.is_some() {
            let entry = entries
                .into_iter()
                .next()
                .ok_or_else(|| error("agent definition was not found"))?;
            SessionResourceValue::Read {
                resource: descriptor(&entry),
                text: if entry.seed.is_some() {
                    entry.text
                } else {
                    format!("{}\n\n{}", entry.description, entry.text)
                },
            }
        } else {
            SessionResourceValue::List {
                entries: entries.iter().map(descriptor).collect(),
            }
        };
        value.validate().map_err(error)?;
        Ok(value)
    }
}
#[async_trait]
impl BeforeStepContributor for Definitions {
    async fn prepare(
        &self,
        context: &BeforeStepContext<'_>,
        cancellation: CancellationToken,
    ) -> ContributionResult<BeforeStepOutput> {
        let context = context.captured;
        let entries = match self
            .load(
                &context.header,
                context.execution.as_ref(),
                None,
                cancellation.clone(),
            )
            .await
        {
            Ok(entries) => entries,
            Err(error @ (ContributionError::Capacity | ContributionError::Closed)) => {
                return Err(error);
            }
            Err(error) => {
                return Ok(BeforeStepOutput {
                    instructions: format!(
                        "Agent catalog unavailable: {error}. Fresh spawn must validate its selected definition."
                    ),
                    ..BeforeStepOutput::default()
                });
            }
        };
        let mut text = String::from(
            "Available subagents (bounded listing; exact names outside the listing can also resolve):\n",
        );
        if entries.is_empty() {
            text.push_str("No definitions listed.\n");
        }
        for entry in &entries {
            let available = if entry.seed.is_some() {
                "available"
            } else {
                "unavailable"
            };
            let _ = writeln!(
                text,
                "- {} ({available}): {}",
                entry.name, entry.description
            );
            if let Some(model) = entry.seed.as_ref().and_then(|seed| seed.model.as_ref()) {
                let _ = writeln!(
                    text,
                    "  deployment: {}\n  model: {}",
                    model.model.deployment(),
                    model.model.model()
                );
                if let Some(effort) = &model.reasoning_effort {
                    let _ = writeln!(text, "  reasoning effort: {}", effort.as_str());
                }
            }
        }
        text.push_str("\nUse spawn_agent(role=exact name) to delegate. The definition is resolved when a new child is admitted. Human @name requests should be delegated by you; orchestrate and summarize the child's result.\n");
        let available: BTreeSet<_> = entries
            .iter()
            .filter(|entry| entry.seed.is_some())
            .map(|entry| entry.name.as_str())
            .collect();
        let names = self.mentions.read(context, cancellation).await?;
        let mentions: BTreeSet<_> = names
            .iter()
            .filter(|name| available.contains(name.as_str()))
            .collect();
        if !mentions.is_empty() {
            let _ = writeln!(
                text,
                "Explicit Agent mentions in this Turn's Human input: {}. Use these roles for the corresponding requested work; do not automatically split one task into multiple tasks.",
                serde_json::to_string(&mentions).map_err(error)?
            );
        }
        Ok(BeforeStepOutput {
            instructions: text,
            ..BeforeStepOutput::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn prepare(
        definitions: &Definitions,
        context: &ContributionContext,
    ) -> ContributionResult<BeforeStepOutput> {
        let selection = ModelSelection {
            model: context.header.settings().default_model().clone(),
            reasoning_effort: None,
        };
        let output = definitions
            .prepare(
                &BeforeStepContext {
                    selection: &selection,
                    producer: &ContributionId::new("rsi.available-agents").unwrap(),
                    previous_input_at_ms: None,
                    captured: context,
                    previous: None,
                    request_id: &rsi_agent_session_protocol::DomainRequestId::new("catalog")
                        .unwrap(),
                    tools: &[],
                },
                CancellationToken::new(),
            )
            .await?;
        assert!(
            output.contribution.inputs.is_empty(),
            "current catalog must not append history"
        );
        assert!(output.contribution.domains.is_empty());
        assert!(output.publications.is_empty());
        assert!(output.decision.is_none());
        assert!(output.tools.is_none());
        rsi_agent_session_protocol::validate_step_instructions(&[
            rsi_agent_session_protocol::StepInstruction {
                producer: ContributionId::new("rsi.available-agents").unwrap(),
                text: output.instructions.clone(),
            },
        ])
        .expect("the full supported catalog fits the current instruction budget");
        Ok(output)
    }
    #[derive(Debug)]
    struct CountedSource {
        inner: rsi_agent_workspace_context::LocalWorkspaceContext,
        calls: std::sync::atomic::AtomicUsize,
        fault: std::sync::Mutex<Option<rsi_agent_workspace_context::WorkspaceContextError>>,
    }
    #[async_trait]
    impl WorkspaceContext for CountedSource {
        async fn agents(
            &self,
            header: &SessionHeader,
            execution: Option<&rsi_execution::ExecutionLease>,
            id: Option<&str>,
            reserved: &BTreeSet<String>,
            cancellation: CancellationToken,
        ) -> Result<Vec<WorkspaceAgentDefinition>, rsi_agent_workspace_context::WorkspaceContextError>
        {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(error) = self.fault.lock().unwrap().clone() {
                return Err(error);
            }
            self.inner
                .agents(header, execution, id, reserved, cancellation)
                .await
        }
        async fn skills(
            &self,
            header: &SessionHeader,
            execution: Option<&rsi_execution::ExecutionLease>,
            id: Option<&str>,
            audience: rsi_agent_workspace_context::SkillAudience,
            cancellation: CancellationToken,
        ) -> Result<SessionResourceValue, rsi_agent_workspace_context::WorkspaceContextError>
        {
            self.inner
                .skills(header, execution, id, audience, cancellation)
                .await
        }
        async fn snapshot(
            &self,
            header: &SessionHeader,
            execution: Option<&rsi_execution::ExecutionLease>,
            requests: &rsi_agent_workspace_context::WorkspaceSkillRequests,
        ) -> Result<
            rsi_agent_workspace_context::WorkspaceContextSnapshot,
            rsi_agent_workspace_context::WorkspaceContextError,
        > {
            self.inner.snapshot(header, execution, requests).await
        }
    }
    #[derive(Debug, Default)]
    struct MentionFacts(
        std::sync::Mutex<Vec<u64>>,
        Vec<Arc<rsi_agent_session_protocol::SessionFact>>,
    );
    #[async_trait]
    impl rsi_agent_composition_protocol::ContributionFactReader for MentionFacts {
        async fn read(
            &self,
            after: u64,
            _: usize,
        ) -> ContributionResult<rsi_agent_composition_protocol::ContributionFactPage> {
            self.0.lock().unwrap().push(after);
            Ok(rsi_agent_composition_protocol::ContributionFactPage {
                facts: self
                    .1
                    .iter()
                    .filter(|fact| fact.seq() == after + 1)
                    .cloned()
                    .collect(),
                through_seq: after + 1,
            })
        }
    }
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // One ordered history proves cursor reuse across catalog edits and retirement.
    async fn mention_history_only_reads_the_suffix_and_rechecks_current_catalog() {
        use rsi_agent_session_protocol::{AgentPresetId, FrozenAgentSettings, StepId, TurnId};
        let temporary = tempfile::tempdir().unwrap();
        let header = SessionHeader::new_local(
            SessionId::new("mentions").unwrap(),
            1,
            temporary.path().to_str().unwrap(),
            AgentPresetId::new("standard").unwrap(),
            FrozenAgentSettings::new(
                "standard",
                "instructions",
                rsi_ai_protocol::ModelRef::new("route", "model").unwrap(),
                serde_json::from_value(json!("workspace-write")).unwrap(),
                false,
            )
            .unwrap(),
        )
        .unwrap();
        let definitions = Definitions {
            source: Arc::new(
                rsi_agent_workspace_context::LocalWorkspaceContext::new(
                    rsi_agent_workspace_context::WorkspaceContextConfig::default(),
                )
                .unwrap(),
            ),
            inline: Arc::new(BTreeMap::new()),
            mentions: MentionHistory::default(),
        };
        let fact = rsi_agent_session_protocol::SessionFact::new(
            1,
            1,
            SessionFactBody::TurnAccepted {
                turn_id: TurnId::new("turn").unwrap(),
                text: "@review and @later".into(),
                model: None,
                reasoning_effort: None,
                sandbox: serde_json::from_value(json!("workspace-write")).unwrap(),
                require_approval: false,
            },
        )
        .unwrap();
        let facts = Arc::new(MentionFacts(
            std::sync::Mutex::default(),
            vec![Arc::new(fact)],
        ));
        let mut context = ContributionContext {
            execution: None,
            header: Arc::new(header),
            turn_id: TurnId::new("turn").unwrap(),
            accepted_fact_seq: 1,
            step_id: StepId::new("step").unwrap(),
            horizon: rsi_agent_composition_protocol::ContributionHorizon {
                fact_seq: 2,
                control_seq: 0,
            },
            domains: Arc::new([]),
            facts: facts.clone(),
        };
        let root = temporary.path().join(".agents/agents");
        std::fs::create_dir_all(&root).unwrap();
        let review = root.join("review.md");
        let later = root.join("later.md");
        std::fs::write(
            &review,
            "---\ndescription: first review description\n---\nPersona",
        )
        .unwrap();
        for horizon in [2, 2, 3, 1] {
            context.horizon.fact_seq = horizon;
            let output = prepare(&definitions, &context).await.unwrap();
            assert!(output.instructions.contains("first review description"));
            assert!(output.instructions.contains("Human input: [\"review\"]"));
            assert_eq!(
                *definitions.mentions.0.lock().unwrap().back().unwrap().names,
                BTreeSet::from(["later".into(), "review".into()])
            );
            let cached = definitions
                .mentions
                .0
                .lock()
                .unwrap()
                .back()
                .unwrap()
                .names
                .clone();
            let repeated = definitions
                .mentions
                .read(&context, CancellationToken::new())
                .await
                .unwrap();
            assert!(Arc::ptr_eq(&cached, &repeated));
        }
        assert_eq!(*facts.0.lock().unwrap(), [0, 1, 2, 0]);
        std::fs::write(
            &review,
            "---\ndescription: changed review description\n---\nPersona",
        )
        .unwrap();
        std::fs::write(
            &later,
            "---\ndescription: newly available role\n---\nPersona",
        )
        .unwrap();
        let output = prepare(&definitions, &context).await.unwrap();
        assert!(output.instructions.contains("changed review description"));
        assert!(!output.instructions.contains("first review description"));
        assert!(
            output
                .instructions
                .contains("Human input: [\"later\",\"review\"]")
        );
        std::fs::remove_file(&review).unwrap();
        let output = prepare(&definitions, &context).await.unwrap();
        assert!(!output.instructions.contains("review description"));
        assert!(output.instructions.contains("Human input: [\"later\"]"));
        std::fs::remove_file(&later).unwrap();
        let output = prepare(&definitions, &context).await.unwrap();
        assert!(output.instructions.contains("No definitions listed."));
        assert!(!output.instructions.contains("Explicit Agent mentions"));
        for index in 0..40 {
            context.turn_id = TurnId::new(format!("turn-{index}")).unwrap();
            prepare(&definitions, &context).await.unwrap();
        }
        assert_eq!(definitions.mentions.0.lock().unwrap().len(), 32);
    }
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // One isolated catalog checks listing, typed failures and collision-body exclusion.
    async fn file_prefix_and_inline_roles_coexist_and_hidden_collisions_fail_closed() {
        use rsi_agent_session_protocol::{AgentPresetId, FrozenAgentSettings};
        use rsi_agent_workspace_context::{LocalWorkspaceContext, WorkspaceContextConfig};
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join(".agents/agents");
        std::fs::create_dir_all(&root).unwrap();
        let maximum_description = "\"".repeat(1024);
        let maximum_model = json!({
            "deployment": "\\".repeat(rsi_ai_protocol::MAX_ID_BYTES),
            "model": "\"".repeat(rsi_ai_protocol::MAX_ID_BYTES),
        });
        for index in 0..33 {
            std::fs::write(
                root.join(format!("role-{index:02}.md")),
                format!(
                    "---\n{}\n---\nPersona",
                    json!({
                        "description": maximum_description,
                        "model": maximum_model,
                        "reasoning_effort": "r".repeat(rsi_ai_protocol::MAX_REASONING_EFFORT_BYTES),
                    })
                ),
            )
            .unwrap();
        }
        let header = SessionHeader::new_local(
            SessionId::new("catalog").unwrap(),
            1,
            std::fs::canonicalize(temporary.path())
                .unwrap()
                .to_str()
                .unwrap(),
            AgentPresetId::new("standard").unwrap(),
            FrozenAgentSettings::new(
                "standard",
                "instructions",
                rsi_ai_protocol::ModelRef::new("route", "model").unwrap(),
                serde_json::from_value(json!("workspace-write")).unwrap(),
                false,
            )
            .unwrap(),
        )
        .unwrap();
        let source = Arc::new(CountedSource {
            inner: LocalWorkspaceContext::new(WorkspaceContextConfig::default()).unwrap(),
            calls: std::sync::atomic::AtomicUsize::new(0),
            fault: std::sync::Mutex::new(None),
        });
        let definitions = Definitions {
            mentions: MentionHistory::default(),
            source: source.clone(),
            inline: Arc::new(
                (0..32)
                    .map(|index| {
                        let name = format!("inline-{index}");
                        (
                            name.clone(),
                            DelegationRole {
                                name,
                                persona: None,
                                allow: None,
                                deny: BTreeSet::new(),
                            },
                        )
                    })
                    .collect(),
            ),
        };
        let entries = definitions
            .load(&header, None, None, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(entries.len(), 64);
        assert_eq!(source.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(entries.iter().all(|entry| entry.seed.is_some()));
        let context = ContributionContext {
            execution: None,
            header: Arc::new(header.clone()),
            turn_id: rsi_agent_session_protocol::TurnId::new("turn").unwrap(),
            accepted_fact_seq: 1,
            step_id: rsi_agent_session_protocol::StepId::new("step").unwrap(),
            horizon: rsi_agent_composition_protocol::ContributionHorizon {
                fact_seq: 1,
                control_seq: 0,
            },
            domains: Arc::new([]),
            facts: Arc::new(MentionFacts::default()),
        };
        for fault in [
            rsi_agent_workspace_context::WorkspaceContextError::Capacity,
            rsi_agent_workspace_context::WorkspaceContextError::Closed,
            rsi_agent_workspace_context::WorkspaceContextError::Invalid(
                "invalid observation".into(),
            ),
            rsi_agent_workspace_context::WorkspaceContextError::Failed("source unavailable".into()),
        ] {
            *source.fault.lock().unwrap() = Some(fault.clone());
            let result = prepare(&definitions, &context).await;
            let spawn = definitions
                .resolve(
                    &header,
                    None,
                    &SpawnRoleReference {
                        provider: "rsi.agents".into(),
                        name: "role-00".into(),
                    },
                    CancellationToken::new(),
                )
                .await;
            if fault == WorkspaceContextError::Capacity {
                assert_eq!(result.unwrap_err(), ContributionError::Capacity);
                assert!(matches!(spawn, Err(TurnError::Capacity)));
            } else if fault == WorkspaceContextError::Closed {
                assert_eq!(result.unwrap_err(), ContributionError::Closed);
                assert!(matches!(spawn, Err(TurnError::ShuttingDown)));
            } else {
                let output = result.unwrap();
                assert!(
                    output
                        .instructions
                        .starts_with("Agent catalog unavailable:")
                );
                assert!(!output.instructions.contains("Available subagents"));
                assert!(matches!(spawn, Err(TurnError::Invalid(_))));
            }
        }
        *source.fault.lock().unwrap() = None;
        let output = prepare(&definitions, &context).await.unwrap();
        assert!(output.instructions.contains("Available subagents"));
        assert!(!output.instructions.contains("Agent catalog unavailable:"));
        assert_eq!(
            output.instructions.matches(&maximum_description).count(),
            32
        );
        std::fs::write(root.join("role-32.md"), "not YAML frontmatter").unwrap();
        let collision = Definitions {
            mentions: MentionHistory::default(),
            source: definitions.source,
            inline: Arc::new(
                [(
                    "role-32".into(),
                    DelegationRole {
                        name: "role-32".into(),
                        persona: None,
                        allow: None,
                        deny: BTreeSet::new(),
                    },
                )]
                .into(),
            ),
        };
        let entries = collision
            .load(&header, None, None, CancellationToken::new())
            .await
            .unwrap();
        let entry = entries
            .iter()
            .find(|entry| entry.name == "role-32")
            .unwrap();
        assert!(entry.seed.is_none());
        assert!(entry.description.contains("conflicts"));
        assert!(
            entry.text.is_empty(),
            "the conflicting body must not be read"
        );
        assert!(
            collision
                .resolve(
                    &header,
                    None,
                    &SpawnRoleReference {
                        provider: "rsi.agents".into(),
                        name: "role-32".into()
                    },
                    CancellationToken::new()
                )
                .await
                .is_err()
        );
    }
}
