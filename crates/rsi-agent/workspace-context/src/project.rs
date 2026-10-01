//! Bounded project-only wire collection; no Session or Service user configuration crosses it.
use super::*;
use rsi_agent_session_protocol::{SessionResourceDescriptor, SessionResourceValue};

pub(super) const MAXIMUM_REQUEST_BYTES: usize = 512 * 1024;
pub(super) const MAXIMUM_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
pub(super) const MARKER: &str = "--rsi-read-project-context";

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Reply {
    Captured { capture: Capture },
    Failed { capacity: bool },
}

/// Runs the fixed read-only project collector when the sole helper marker is present.
/// The caller must place this entry behind its selected target's `ReadOnly` process plan.
pub fn maybe_run_project_context_helper(arguments: &[std::ffi::OsString]) -> Option<u8> {
    if arguments.first().is_none_or(|value| value != MARKER) {
        return None;
    }
    if arguments.len() != 1 {
        return Some(2);
    }
    Some(run_helper().map_or(2, |()| 0))
}
fn run_helper() -> std::io::Result<()> {
    use std::io::Write as _;
    let result = (|| {
        let mut bytes = Vec::new();
        std::io::stdin()
            .lock()
            .take(MAXIMUM_REQUEST_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid())?;
        if bytes.len() > MAXIMUM_REQUEST_BYTES {
            return Err(WorkspaceContextError::Capacity);
        }
        let request: Request = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        let cwd = std::env::current_dir().map_err(|_| invalid())?;
        let capture = collect(&cwd, &request, CancellationToken::new())?;
        capture.validate(&request)?;
        Ok(capture)
    })();
    let reply = match result {
        Ok(capture) => Reply::Captured { capture },
        Err(error) => Reply::Failed {
            capacity: error == WorkspaceContextError::Capacity,
        },
    };
    let bytes = encode(&reply, MAXIMUM_RESPONSE_BYTES)
        .unwrap_or_else(|_| b"{\"result\":\"failed\",\"capacity\":true}".to_vec());
    std::io::stdout().lock().write_all(&bytes)
}

pub(super) fn encode(
    value: &impl Serialize,
    maximum: usize,
) -> Result<Vec<u8>, WorkspaceContextError> {
    struct Bounded {
        bytes: Vec<u8>,
        maximum: usize,
    }
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.maximum.saturating_sub(self.bytes.len()) {
                return Err(std::io::ErrorKind::FileTooLarge.into());
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded {
        bytes: Vec::new(),
        maximum,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| WorkspaceContextError::Capacity)?;
    Ok(writer.bytes)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Request {
    Snapshot {
        names: Vec<String>,
        retained_instruction_bytes: usize,
    },
    Skills {
        id: Option<String>,
        audience: SkillAudience,
    },
    Agents {
        id: Option<String>,
        reserved: BTreeSet<String>,
    },
}
impl Request {
    pub(super) fn validate(&self) -> Result<(), WorkspaceContextError> {
        let valid = match self {
            Self::Snapshot {
                names,
                retained_instruction_bytes,
            } => {
                names.len() <= 4096
                    && names.iter().all(|name| valid_skill_name(name))
                    && names.iter().collect::<BTreeSet<_>>().len() == names.len()
                    && (INSTRUCTIONS_PREAMBLE.len()..=MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES)
                        .contains(retained_instruction_bytes)
            }
            Self::Skills { id, .. } => id.as_ref().is_none_or(|id| valid_skill_name(id)),
            Self::Agents { id, reserved } => {
                id.as_ref().is_none_or(|id| valid_skill_name(id))
                    && reserved.len() <= 32
                    && reserved.iter().all(|name| valid_skill_name(name))
            }
        };
        if valid { Ok(()) } else { Err(invalid()) }
    }
    fn wants_body(&self, skill: &SelectedSkill) -> bool {
        match self {
            Self::Snapshot { names, .. } => skill.user_invocable && names.contains(&skill.name),
            Self::Skills {
                id: Some(id),
                audience,
            } if *id == skill.name => match audience {
                SkillAudience::Human => skill.user_invocable,
                SkillAudience::Model => skill.model_invocable,
            },
            _ => false,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Skill {
    pub(super) resource: SessionResourceDescriptor,
    pub(super) user_invocable: bool,
    pub(super) body: Option<String>,
}
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Context {
    pub(super) instructions: Vec<(String, String)>,
    pub(super) skills: BTreeMap<String, Skill>,
    pub(super) inspected: usize,
    pub(super) diagnostic: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Capture {
    Context(Context),
    Agents(agents::Collection),
}
fn invalid() -> WorkspaceContextError {
    WorkspaceContextError::Invalid("invalid project context exchange".into())
}
impl Capture {
    pub(super) fn validate(&self, request: &Request) -> Result<(), WorkspaceContextError> {
        request.validate()?;
        match (self, request) {
            (
                Self::Context(context),
                Request::Snapshot {
                    retained_instruction_bytes,
                    ..
                },
            ) => {
                validate_context(context)?;
                let bytes = context.instructions.iter().try_fold(
                    *retained_instruction_bytes,
                    |sum, (source, text)| {
                        sum.checked_add(instruction_section_bytes(source, text))
                            .filter(|bytes| *bytes <= MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES)
                    },
                );
                if bytes.is_none() {
                    return Err(invalid());
                }
            }
            (Self::Context(context), Request::Skills { .. }) => {
                validate_context(context)?;
                if !context.instructions.is_empty() {
                    return Err(invalid());
                }
            }
            (Self::Agents(collection), Request::Agents { id, reserved }) => {
                validate_agents(collection, id.as_deref(), reserved)?;
            }
            _ => return Err(invalid()),
        }
        if let Self::Context(context) = self {
            for (name, skill) in &context.skills {
                let selected = match request {
                    Request::Snapshot { names, .. } => skill.user_invocable && names.contains(name),
                    Request::Skills {
                        id: Some(id),
                        audience,
                    } if id == name => match audience {
                        SkillAudience::Human => skill.user_invocable,
                        SkillAudience::Model => skill.resource.model_readable,
                    },
                    _ => false,
                };
                if skill.body.is_some() && !selected {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
}
fn validate_context(context: &Context) -> Result<(), WorkspaceContextError> {
    if context.inspected > MAXIMUM_WORKSPACE_SKILL_ENTRIES
        || context.skills.len() > context.inspected
        || context.instructions.len() > MAXIMUM_WORKSPACE_INSTRUCTION_FILES
        || context.diagnostic.as_ref().is_some_and(|text| {
            text.is_empty() || text.len() > 2048 || text.chars().any(char::is_control)
        })
    {
        return Err(invalid());
    }
    for (source, text) in &context.instructions {
        if source.len() > 64 * 1024
            || !session_safe_text(source)
            || text.len() > MAXIMUM_WORKSPACE_CONTEXT_SOURCE_BYTES
            || !session_safe_text(text)
        {
            return Err(invalid());
        }
    }
    for (name, skill) in &context.skills {
        if !valid_skill_name(name)
            || *name != skill.resource.name
            || *name != skill.resource.id
            || skill.resource.media_type != "text/markdown"
            || skill.body.as_ref().is_some_and(|text| {
                text.len() > MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES || !session_safe_text(text)
            })
        {
            return Err(invalid());
        }
        SessionResourceValue::List {
            entries: vec![skill.resource.clone()],
        }
        .validate()
        .map_err(|_| invalid())?;
    }
    Ok(())
}
fn validate_agents(
    collection: &agents::Collection,
    id: Option<&str>,
    reserved: &BTreeSet<String>,
) -> Result<(), WorkspaceContextError> {
    if collection.inspected > MAXIMUM_WORKSPACE_SKILL_ENTRIES
        || collection.listed > 32
        || collection.visited.len() > collection.inspected
        || collection.selected.len() > 32 + reserved.len()
        || collection
            .visited
            .iter()
            .any(|name| !valid_skill_name(name))
    {
        return Err(invalid());
    }
    for (name, entry) in &collection.selected {
        if *name != entry.name
            || !collection.visited.contains(name)
            || id.is_some_and(|id| id != name)
            || entry.description.len() > 1024
            || entry.description.chars().any(char::is_control)
            || entry.source.len() > 64 * 1024
            || !session_safe_text(&entry.source)
            || entry.text.len() > 64 * 1024
            || !session_safe_text(&entry.text)
            || (reserved.contains(name) && (entry.seed.is_some() || !entry.text.is_empty()))
        {
            return Err(invalid());
        }
        if let Some(seed) = &entry.seed {
            seed.validate().map_err(|_| invalid())?;
            if seed.reference.name != *name
                || seed.source != entry.source
                || seed.sha256 != digest(&entry.text)
            {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

pub(super) fn collect(
    cwd: &Path,
    request: &Request,
    cancellation: CancellationToken,
) -> Result<Capture, WorkspaceContextError> {
    request.validate()?;
    let config = WorkspaceContextConfig::default();
    let mut budget = SnapshotBudget::new(&config, cwd, cancellation)?;
    if let Request::Agents { id, reserved } = request {
        return agents::read_partition(
            &config,
            cwd,
            agents::Selection {
                id: id.as_deref(),
                reserved_names: reserved,
                project: true,
            },
            budget,
            agents::Collection::default(),
        )
        .map(Capture::Agents);
    }
    let mut observation = Observation::default();
    let (root, git) = skills::project_boundary(cwd, &mut observation);
    let mut context = Context::default();
    if let Request::Snapshot {
        retained_instruction_bytes,
        ..
    } = request
    {
        let authority = root.as_deref().filter(|_| git).and_then(|root| {
            ProjectAuthority::open(root).map_or_else(
                |error| {
                    observation.io(root, "open instruction root", &error);
                    None
                },
                Some,
            )
        });
        context.instructions = read_project_sections(
            cwd,
            root.as_deref().filter(|_| git),
            authority.as_ref(),
            *retained_instruction_bytes,
            &budget,
            &mut observation,
        )?;
    }
    let mut discovery = SkillDiscovery::default();
    if let Some(root) = root.as_deref() {
        for directory in directories_between(root, cwd)?.into_iter().rev() {
            discovery.scan(
                &directory.join(".agents/skills"),
                Some(root),
                &mut observation,
                &mut budget,
            )?;
        }
    }
    context.inspected = discovery.inspected();
    capture_skills(
        &mut context,
        discovery.into_selected(),
        request,
        &mut observation,
        &mut budget,
    )?;
    context.diagnostic = observation.diagnostic;
    Ok(Capture::Context(context))
}

fn capture_skills(
    context: &mut Context,
    selected: Vec<SelectedSkill>,
    request: &Request,
    observation: &mut Observation,
    budget: &mut SnapshotBudget,
) -> Result<(), WorkspaceContextError> {
    for skill in selected {
        budget.check()?;
        if context.skills.contains_key(&skill.name) {
            continue;
        }
        let resource = skills::descriptor(&skill);
        let body = if request.wants_body(&skill) {
            budget.reserve(MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES)?;
            let invocation = read_skill_invocation(&skill, observation, &budget.cancellation);
            let body = invocation.map(|value| value.text);
            budget.release(
                MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES
                    - body.as_ref().map_or(0, String::capacity),
            );
            body
        } else {
            None
        };
        context.skills.insert(
            skill.name,
            Skill {
                resource,
                user_invocable: skill.user_invocable,
                body,
            },
        );
    }
    Ok(())
}

pub(super) fn add_user_sources(
    config: &WorkspaceContextConfig,
    request: &Request,
    mut context: Context,
    mut budget: SnapshotBudget,
) -> Result<Context, WorkspaceContextError> {
    let mut observation = Observation {
        diagnostic: context.diagnostic.take(),
    };
    let mut discovery = SkillDiscovery::continuing(context.inspected);
    for root in &config.user_skill_roots {
        discovery.scan(root, None, &mut observation, &mut budget)?;
    }
    context.inspected = discovery.inspected();
    capture_skills(
        &mut context,
        discovery.into_selected(),
        request,
        &mut observation,
        &mut budget,
    )?;
    context.diagnostic = observation.diagnostic;
    Ok(context)
}

pub(super) fn snapshot(
    context: Context,
    user: &[(String, String)],
    names: &[String],
) -> WorkspaceContextSnapshot {
    let instructions = render_instructions(user, &context.instructions);
    let skill_catalog = render_skill_catalog_values(
        context
            .skills
            .values()
            .filter(|skill| skill.resource.model_readable)
            .map(|skill| {
                (
                    skill.resource.name.as_str(),
                    skill.resource.description.as_str(),
                )
            }),
    );
    let invocations = names
        .iter()
        .filter_map(|name| {
            let skill = context.skills.get(name)?;
            skill.user_invocable.then_some(())?;
            Some(WorkspaceSkillInvocation {
                name: name.clone(),
                source: skill.resource.source.clone(),
                text: skill.body.clone()?,
            })
        })
        .collect();
    WorkspaceContextSnapshot {
        complete: context.diagnostic.is_none(),
        diagnostic: context.diagnostic,
        instructions_sha256: digest(instructions.as_deref().unwrap_or("")),
        instructions,
        skill_catalog_sha256: digest(skill_catalog.as_deref().unwrap_or("")),
        skill_catalog,
        invocations,
    }
}
pub(super) fn resource(
    mut context: Context,
    id: Option<&str>,
    audience: SkillAudience,
) -> Result<SessionResourceValue, WorkspaceContextError> {
    if let Some(diagnostic) = context.diagnostic {
        return Err(WorkspaceContextError::Failed(diagnostic));
    }
    let eligible = |skill: &Skill| match audience {
        SkillAudience::Human => skill.user_invocable,
        SkillAudience::Model => skill.resource.model_readable,
    };
    let value = if let Some(id) = id {
        let skill = context.skills.remove(id).ok_or_else(|| {
            WorkspaceContextError::Invalid("skill was not found in the current catalog".into())
        })?;
        if !eligible(&skill) {
            return Err(WorkspaceContextError::Invalid(
                "skill does not allow the requested invocation".into(),
            ));
        }
        SessionResourceValue::Read {
            resource: skill.resource,
            text: skill.body.ok_or_else(|| {
                WorkspaceContextError::Failed(
                    "selected skill changed or could not be read; refresh to retry".into(),
                )
            })?,
        }
    } else {
        SessionResourceValue::List {
            entries: context
                .skills
                .into_values()
                .filter(eligible)
                .map(|skill| skill.resource)
                .collect(),
        }
    };
    value
        .validate()
        .map_err(|error| WorkspaceContextError::Invalid(error.to_string()))?;
    Ok(value)
}

#[cfg(test)]
#[path = "project/tests.rs"]
mod tests;
