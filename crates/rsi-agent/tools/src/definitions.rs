//! A single definition source for model discovery, human previews and named spawn.
use super::*;
use rsi_agent_composition_protocol::{
    ContextContributor, ContributionContext, ContributionError, ContributionInput,
    ContributionKind, ContributionOutput, ContributionRegistrarContract, ContributionRegistration,
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
                    ContributionKind::Context(source.clone()),
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
        id: Option<&str>,
        stop: CancellationToken,
    ) -> ContributionResult<Vec<WorkspaceAgentDefinition>> {
        let mut entries = self
            .source
            .agents(header, id, &self.inline.keys().cloned().collect(), stop)
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
                    SessionFactBody::InputMessageEntered {
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
        reference: &SpawnRoleReference,
        cancellation: CancellationToken,
    ) -> rsi_agent_turn_protocol::Result<SpawnRoleSeed> {
        if reference.provider != "rsi.agents" {
            return Err(TurnError::Invalid(
                "unknown Agent definition provider".into(),
            ));
        }
        let entry = self
            .load(header, Some(&reference.name), cancellation)
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
        id: Option<&str>,
        cancellation: CancellationToken,
    ) -> ContributionResult<SessionResourceValue> {
        let entries = self.load(header, id, cancellation).await?;
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
impl ContextContributor for Definitions {
    async fn contribute(
        &self,
        context: &ContributionContext,
        cancellation: CancellationToken,
    ) -> ContributionResult<ContributionOutput> {
        let entries = match self.load(&context.header, None, cancellation.clone()).await {
            Ok(entries) => entries,
            Err(error @ (ContributionError::Capacity | ContributionError::Closed)) => {
                return Err(error);
            }
            Err(error) => {
                return Ok(ContributionOutput {
                    inputs: vec![ContributionInput::context(format!(
                        "Agent catalog unavailable: {error}. Fresh spawn must validate its selected definition."
                    ))],
                    domains: vec![],
                });
            }
        };
        let catalog: Vec<_> = entries.iter().map(|entry| json!({"name":entry.name,"description":entry.description,"available":entry.seed.is_some(),"model":entry.seed.as_ref().and_then(|seed|seed.model.as_ref())})).collect();
        let mut text = format!(
            "Available subagents (bounded listing replaces earlier entries; exact names outside the listing can also resolve):\n{}\nUse spawn_agent(role=exact name) to delegate. The definition is resolved when a new child is admitted. Human @name requests should be delegated by you; orchestrate and summarize the child's result.\n",
            serde_json::to_string(&catalog).map_err(error)?
        );
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
        let sha256 = format!("{:x}", Sha256::digest(text.as_bytes()));
        Ok(ContributionOutput {
            inputs: vec![ContributionInput::sourced(
                InputMessageSource::AgentInstructions {
                    source: "available-agents".into(),
                    sha256,
                    replacement: true,
                    tombstone: entries.is_empty(),
                },
                text,
            )],
            domains: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            id: Option<&str>,
            reserved: &BTreeSet<String>,
            cancellation: CancellationToken,
        ) -> Result<Vec<WorkspaceAgentDefinition>, rsi_agent_workspace_context::WorkspaceContextError>
        {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(error) = self.fault.lock().unwrap().clone() {
                return Err(error);
            }
            self.inner.agents(header, id, reserved, cancellation).await
        }
        async fn skills(
            &self,
            header: &SessionHeader,
            id: Option<&str>,
            audience: rsi_agent_workspace_context::SkillAudience,
            cancellation: CancellationToken,
        ) -> Result<SessionResourceValue, rsi_agent_workspace_context::WorkspaceContextError>
        {
            self.inner.skills(header, id, audience, cancellation).await
        }
        async fn snapshot(
            &self,
            header: &SessionHeader,
            requests: &rsi_agent_workspace_context::WorkspaceSkillRequests,
        ) -> Result<
            rsi_agent_workspace_context::WorkspaceContextSnapshot,
            rsi_agent_workspace_context::WorkspaceContextError,
        > {
            self.inner.snapshot(header, requests).await
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
    async fn mention_history_only_reads_the_suffix_and_resets_on_rewind() {
        use rsi_agent_session_protocol::{AgentPresetId, FrozenAgentSettings, StepId, TurnId};
        let temporary = tempfile::tempdir().unwrap();
        let header = SessionHeader::new(
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
        for horizon in [2, 2, 3, 1] {
            context.horizon.fact_seq = horizon;
            definitions
                .contribute(&context, CancellationToken::new())
                .await
                .unwrap();
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
        for index in 0..40 {
            context.turn_id = TurnId::new(format!("turn-{index}")).unwrap();
            definitions
                .contribute(&context, CancellationToken::new())
                .await
                .unwrap();
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
        for index in 0..33 {
            std::fs::write(
                root.join(format!("role-{index:02}.md")),
                "---\ndescription: review code\n---\nPersona",
            )
            .unwrap();
        }
        let header = SessionHeader::new(
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
            .load(&header, None, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(entries.len(), 64);
        assert_eq!(source.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(entries.iter().all(|entry| entry.seed.is_some()));
        let context = ContributionContext {
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
        ] {
            *source.fault.lock().unwrap() = Some(fault.clone());
            let result = definitions
                .contribute(&context, CancellationToken::new())
                .await;
            let spawn = definitions
                .resolve(
                    &header,
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
            } else {
                assert_eq!(result.unwrap_err(), ContributionError::Closed);
                assert!(matches!(spawn, Err(TurnError::ShuttingDown)));
            }
        }
        *source.fault.lock().unwrap() = None;
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
            .load(&header, None, CancellationToken::new())
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
