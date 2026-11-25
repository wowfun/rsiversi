use super::*;
use rsi_agent_session_protocol::{
    AgentControlRecordBody, ApprovalPromptPolicy, CommandArguments, ContributionId,
    DomainRequestId, SessionCommandInvocation, SessionPermission,
};

#[tokio::test]
async fn selected_permission_is_committed_once_and_restored_from_the_session_store() {
    let fixture = fixture("http://127.0.0.1:1");
    let alternate = rsi_agent_session_protocol::AgentPresetId::new("context-only").unwrap();
    let directory = fixture.paths.config().join("agent-presets/context-only");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("agent.profile.toml"),
        "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"context\"\nplugin = \"rsi.agent.context.default\"\n").unwrap();
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let workspace = running
        .workspace_registry()
        .unwrap()
        .get_or_create(&fixture.workspace)
        .await
        .unwrap();
    let session_id = SessionId::new("permission-selected").unwrap();
    let handle = running
        .session_service()
        .unwrap()
        .create(CreateSession {
            session_id: session_id.clone(),
            workspace_id: workspace.id.clone(),
            agent_preset_id: None,
            fork: None,
        })
        .await
        .unwrap();
    let command = ContributionId::new(SessionPermission::COMMAND).unwrap();
    assert!(
        handle
            .commands()
            .await
            .unwrap()
            .commands()
            .iter()
            .any(|entry| entry.id() == &command)
    );
    let selected = SessionPermission {
        preset: "danger-full-access".into(),
        sandbox: rsi_sandbox::SandboxMode::DangerFullAccess,
        approval: ApprovalPromptPolicy::Never,
    };
    let settings = running.settings_access().unwrap();
    let before = settings.read("rsi.agent").await.unwrap();
    let applied = settings
        .set_fields(
            "rsi.agent",
            &before.version(),
            [("permission_default".into(), serde_json::json!(selected))]
                .into_iter()
                .collect(),
        )
        .await
        .unwrap();
    let inherited_id = SessionId::new("permission-inherited").unwrap();
    let inherited = running
        .session_service()
        .unwrap()
        .create(CreateSession {
            session_id: inherited_id.clone(),
            workspace_id: workspace.id,
            agent_preset_id: None,
            fork: None,
        })
        .await
        .unwrap();
    assert_eq!(
        inherited.header().await.unwrap().settings().permission(),
        Some(&selected)
    );
    assert!(
        handle
            .header()
            .await
            .unwrap()
            .settings()
            .permission()
            .is_none()
    );
    settings
        .replace("rsi.agent", &applied.version(), before.value)
        .await
        .unwrap();
    assert_eq!(
        inherited.header().await.unwrap().settings().permission(),
        Some(&selected)
    );
    inherited.prepare_application_commands(None).await.unwrap();
    drop(inherited);
    let request_id = DomainRequestId::new("choose-full-access").unwrap();
    let revision = handle.prepare_application_commands(None).await.unwrap();
    let invocation = SessionCommandInvocation {
        command,
        request_id: request_id.clone(),
        expected_revision: revision,
        arguments: CommandArguments::new(serde_json::json!("danger-full-access")).unwrap(),
    };
    let receipt = handle.execute_command(invocation.clone()).await.unwrap();
    assert_eq!(handle.execute_command(invocation).await.unwrap(), receipt);
    let original = handle.header().await.unwrap().agent_preset_id().clone();
    let (revision, changed) = handle.select_agent_preset(alternate.clone()).await.unwrap();
    assert!(changed);
    assert!(matches!(
        revision,
        rsi_agent_session_protocol::CommandRevision::Durable { .. }
    ));
    assert_eq!(
        handle.select_agent_preset(alternate.clone()).await.unwrap(),
        (revision, false)
    );
    assert!(
        !handle
            .commands()
            .await
            .unwrap()
            .commands()
            .iter()
            .any(|entry| entry.id().as_str() == SessionPermission::COMMAND)
    );
    drop(handle);
    assert!(running.shutdown().await.is_clean());
    let restarted = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let inherited = restarted
        .session_service()
        .unwrap()
        .attach(&inherited_id)
        .await
        .unwrap();
    let inherited_header = inherited.header().await.unwrap();
    assert_eq!(
        inherited_header.settings().sandbox(),
        rsi_sandbox::SandboxMode::DangerFullAccess
    );
    assert_eq!(
        SessionPermission::initial_approval(&inherited_header),
        ApprovalPromptPolicy::Never
    );
    assert_eq!(inherited_header.settings().permission(), Some(&selected));
    drop(inherited);
    let handle = restarted
        .session_service()
        .unwrap()
        .attach(&session_id)
        .await
        .unwrap();
    assert_eq!(handle.header().await.unwrap().agent_preset_id(), &original);
    assert_eq!(handle.preset_view().await.unwrap().preset_id, alternate);
    assert!(handle.select_agent_preset(original).await.unwrap().1);
    assert_eq!(
        handle.command_status(&request_id).await.unwrap().unwrap(),
        receipt
    );
    let mut projections = handle.observe_projections().await.unwrap();
    let snapshot = projections.next().await.unwrap().unwrap();
    let view = snapshot
        .snapshot()
        .entries()
        .iter()
        .find(|entry| entry.producer().as_str() == SessionPermission::VIEW)
        .unwrap();
    assert_eq!(view.view().unwrap().value(), &serde_json::json!(selected));
    drop(projections);
    let mut updates = handle.observe(ObservationCursor::default()).await.unwrap();
    let saved = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(update) = updates.next().await {
            if let SessionObservation::Control { record, .. } = update.unwrap()
                && let AgentControlRecordBody::DomainStateCommitted { commit } = record.body()
                && commit.request_id() == Some(&request_id)
            {
                return SessionPermission::from_snapshot(commit.updates()[0].snapshot()).unwrap();
            }
        }
        None
    })
    .await
    .unwrap();
    assert_eq!(saved, Some(selected));
    drop(updates);
    assert!(restarted.shutdown().await.is_clean());
}
