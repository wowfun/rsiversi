use rsi_agent_session_protocol::{AgentPresetId, FrozenAgentSettings, SessionHeader, SessionId};
use rsi_agent_workspace_context::{
    LocalWorkspaceContext, SkillAudience, TargetWorkspaceContext, WorkspaceContext,
    WorkspaceContextConfig, WorkspaceContextError, WorkspaceSkillRequests,
};
use rsi_execution::{
    ExecutionAdmission, ExecutionCoordinates, ExecutionLocation, ExecutionOperation,
    ExecutionTargetId, HostEpoch,
};
use rsi_ssh_client::ProcessConnection;
use std::{collections::BTreeSet, fs, path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Default)]
struct Gate(CancellationToken);
impl ExecutionAdmission for Gate {
    fn admit(
        &self,
        _kind: rsi_execution::ExecutionAdmissionKind,
    ) -> rsi_process::Result<ExecutionOperation> {
        if self.0.is_cancelled() {
            return Err(rsi_process::ProcessError::ShuttingDown);
        }
        Ok(ExecutionOperation::new(()))
    }
}
fn header(coordinates: ExecutionCoordinates) -> SessionHeader {
    SessionHeader::new(
        SessionId::new("context-fixture").unwrap(),
        1,
        coordinates,
        AgentPresetId::new("fixture").unwrap(),
        FrozenAgentSettings::new(
            "fixture",
            "system",
            rsi_ai_protocol::ModelRef::new("fixture", "model").unwrap(),
            rsi_sandbox::SandboxMode::ReadOnly,
            false,
        )
        .unwrap(),
    )
    .unwrap()
}
fn skill(root: &Path, body: &str) {
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join("guide.md"),
        format!("---\nname: guide\ndescription: source fixture\n---\n{body}\n"),
    )
    .unwrap();
}

fn sources(
    workspace: &Path,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    WorkspaceContextConfig,
) {
    fs::create_dir(workspace.join(".git")).unwrap();
    let cwd = workspace.join("context-child");
    fs::create_dir(&cwd).unwrap();
    fs::write(workspace.join("AGENTS.md"), "Target ancestor instructions").unwrap();
    fs::write(cwd.join("AGENTS.md"), "Target child instructions").unwrap();
    skill(&cwd.join(".agents/skills"), "Target winning skill");
    fs::create_dir_all(cwd.join(".agents/agents")).unwrap();
    fs::write(
        cwd.join(".agents/agents/review.md"),
        "---\ndescription: target review\n---\nTarget reviewer",
    )
    .unwrap();
    let user = tempfile::tempdir().unwrap();
    fs::write(
        user.path().join("instructions.md"),
        "Service global instructions",
    )
    .unwrap();
    skill(user.path(), "Service losing skill");
    let config = WorkspaceContextConfig {
        user_instruction_file: Some(user.path().join("instructions.md")),
        user_skill_roots: vec![user.path().into()],
        user_agent_roots: vec![],
    };
    (user, cwd, config)
}

pub async fn verify(client: &ProcessConnection, workspace: &Path) {
    let (_user, cwd, config) = sources(workspace);
    let target = ExecutionTargetId::parse("a".repeat(32)).unwrap();
    let provider = rsi_ssh_client::execution_provider(
        HostEpoch::from_bytes([1; 16]),
        client.clone(),
        target.clone(),
        1,
    )
    .unwrap();
    let gate = Arc::new(Gate::default());
    let execution = provider.lease(gate.clone()).unwrap();
    let canonical = client.canonicalize(cwd.to_str().unwrap()).await.unwrap();
    let remote_header = header(
        ExecutionCoordinates::new(ExecutionLocation::Ssh { target }, canonical.clone()).unwrap(),
    );
    let local_header =
        header(ExecutionCoordinates::new(ExecutionLocation::Local, canonical).unwrap());
    let native = LocalWorkspaceContext::new(config.clone()).unwrap();
    let source = TargetWorkspaceContext::new(config).unwrap();
    let mut requests = WorkspaceSkillRequests::default();
    requests.push_text("$guide").unwrap();
    let expected = native
        .snapshot(&local_header, None, &requests)
        .await
        .unwrap();
    let actual = source
        .snapshot(&remote_header, Some(&execution), &requests)
        .await
        .unwrap();
    assert!(actual.complete, "{:?}", actual.diagnostic);
    assert_eq!(actual, expected);
    assert!(
        actual
            .instructions
            .as_ref()
            .unwrap()
            .contains("Service global instructions")
    );
    assert!(actual.invocations[0].text.contains("Target winning skill"));
    let preview = source
        .skills(
            &remote_header,
            Some(&execution),
            Some("guide"),
            SkillAudience::Human,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        preview,
        native
            .skills(
                &local_header,
                None,
                Some("guide"),
                SkillAudience::Human,
                CancellationToken::new()
            )
            .await
            .unwrap()
    );
    let agents = source
        .agents(
            &remote_header,
            Some(&execution),
            Some("review"),
            &BTreeSet::new(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        agents[0].seed.as_ref().unwrap().role.persona.as_deref(),
        Some("Target reviewer")
    );
    fs::write(cwd.join("AGENTS.md"), "Fresh target instructions").unwrap();
    assert!(
        source
            .snapshot(&remote_header, Some(&execution), &requests)
            .await
            .unwrap()
            .instructions
            .unwrap()
            .contains("Fresh target instructions")
    );
    gate.0.cancel();
    assert_eq!(
        source
            .snapshot(&remote_header, Some(&execution), &requests)
            .await
            .unwrap_err(),
        WorkspaceContextError::Closed
    );
}
