use rsi_agent_session_protocol::{
    AgentMessage, AgentMessageContent, AgentMessageSource, AgentPresetId, FrozenAgentSettings,
    MessageId, MessageOptions, SessionHeader, SessionId,
};
use rsi_agent_workspace_context::WorkspaceSkillRequests;
use rsi_agent_workspace_context::{
    LocalWorkspaceContext, MAXIMUM_WORKSPACE_CONTEXT_SOURCE_BYTES,
    MAXIMUM_WORKSPACE_INSTRUCTION_FILES, MAXIMUM_WORKSPACE_SKILL_ENTRIES, WorkspaceContext,
    WorkspaceContextConfig,
};
use rsi_ai_protocol::ModelRef;
use rsi_sandbox::SandboxMode;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn header(cwd: &Path) -> SessionHeader {
    let cwd = fs::canonicalize(cwd).unwrap();
    SessionHeader::new(
        SessionId::new("workspace-context-session").unwrap(),
        1,
        cwd.to_str().unwrap(),
        AgentPresetId::new("test-agent").unwrap(),
        FrozenAgentSettings::new(
            "default",
            "system",
            ModelRef::new("deployment", "model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn stray_agent_filenames_do_not_hide_valid_definitions() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join(".agents/agents");
    fs::create_dir_all(&root).unwrap();
    for name in ["My Agent.md", "review.md"] {
        fs::write(
            root.join(name),
            "---\ndescription: review\n---\nReview code",
        )
        .unwrap();
    }
    let source = context(None, vec![]);
    let entries = source
        .agents(
            &header(temp.path()),
            None,
            &BTreeSet::new(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "review");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn non_utf8_agent_filename_does_not_hide_valid_definitions() {
    use std::os::unix::ffi::OsStringExt as _;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join(".agents/agents");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join(std::ffi::OsString::from_vec(b"\xff.md".to_vec())),
        "bad",
    )
    .unwrap();
    fs::write(
        root.join("review.md"),
        "---\ndescription: review\n---\nReview code",
    )
    .unwrap();
    let entries = context(None, vec![])
        .agents(
            &header(temp.path()),
            None,
            &BTreeSet::new(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "review");
}

fn human(text: &str) -> AgentMessage {
    message(AgentMessageSource::Human, text)
}

fn message(source: AgentMessageSource, text: &str) -> AgentMessage {
    AgentMessage {
        message_id: MessageId::new("message-1").unwrap(),
        source,
        content: vec![AgentMessageContent::Text { text: text.into() }],
        options: MessageOptions::default(),
    }
}

fn write_skill(root: &Path, directory: &str, name: &str, metadata: &str, body: &str) {
    let directory = root.join(directory);
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {metadata}\n---\n{body}\n"),
    )
    .unwrap();
}

fn context(
    user_instruction_file: Option<PathBuf>,
    user_skill_roots: Vec<PathBuf>,
) -> LocalWorkspaceContext {
    LocalWorkspaceContext::new(WorkspaceContextConfig {
        user_agent_roots: Vec::new(),
        user_instruction_file,
        user_skill_roots,
    })
    .unwrap()
}

#[cfg(unix)]
#[tokio::test]
async fn skill_discovery_failure_identifies_the_logical_source() {
    use rsi_agent_workspace_context::SkillAudience;
    use tokio_util::sync::CancellationToken;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("skills");
    fs::create_dir(&root).unwrap();
    let denied = root.join("broken\nentry");
    // An overlong target is an unexpected I/O failure, not an optional missing link.
    std::os::unix::fs::symlink("x".repeat(300), &denied).unwrap();
    let source = context(None, vec![root]);
    let error = source
        .skills(
            &header(temp.path()),
            None,
            SkillAudience::Human,
            CancellationToken::new(),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("broken\\nentry"), "{error}");
    assert!(
        !error.contains('\n'),
        "path control characters must be escaped"
    );
    let snapshot = source
        .snapshot(&header(temp.path()), &WorkspaceSkillRequests::default())
        .await
        .unwrap();
    assert!(!snapshot.complete);
    let diagnostic = snapshot.diagnostic.unwrap();
    assert!(diagnostic.contains("broken\\nentry"), "{diagnostic}");
    assert!(
        diagnostic.contains("resolve skill directory"),
        "{diagnostic}"
    );
    fs::remove_file(denied).unwrap();
    let recovered = source
        .snapshot(&header(temp.path()), &WorkspaceSkillRequests::default())
        .await
        .unwrap();
    assert!(recovered.complete);
    assert!(recovered.diagnostic.is_none());
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One catalog exercises flags, precedence and changing source bytes together"
)]
async fn explicit_reads_preserve_independent_flags_precedence_and_current_bytes() {
    use rsi_agent_session_protocol::SessionResourceValue;
    use rsi_agent_workspace_context::SkillAudience;
    use tokio_util::sync::CancellationToken;
    let temp = tempfile::tempdir().unwrap();
    let user = temp.path().join("user");
    let project = temp.path().join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    write_skill(
        &user,
        "manual",
        "manual",
        "manual\ndisable-model-invocation: true",
        "HUMAN ONLY",
    );
    write_skill(
        &user,
        "automatic",
        "automatic",
        "automatic\nuser-invocable: false",
        "MODEL ONLY",
    );
    write_skill(
        &project.join(".agents/skills"),
        "project",
        "project",
        "project",
        "PROJECT BODY",
    );
    write_skill(
        &project.join(".agents/skills"),
        "manual",
        "manual",
        "shadow\ndisable-model-invocation: true",
        "SHADOW",
    );
    let source = context(None, vec![user.clone()]);
    let session = header(&project);
    let human = source
        .skills(
            &session,
            None,
            SkillAudience::Human,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let SessionResourceValue::List { entries } = human else {
        panic!("catalog")
    };
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["manual", "project"]
    );
    assert!(!entries[0].model_readable);
    for (name, audience, header) in [
        ("manual", SkillAudience::Model, &session),
        ("automatic", SkillAudience::Human, &session),
        ("../manual", SkillAudience::Human, &session),
    ] {
        assert!(
            source
                .skills(header, Some(name), audience, CancellationToken::new())
                .await
                .is_err()
        );
    }
    let before = source
        .skills(
            &session,
            Some("automatic"),
            SkillAudience::Model,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let SessionResourceValue::Read { text, .. } = &before else {
        panic!("body")
    };
    assert!(text.contains("MODEL ONLY"));
    write_skill(
        &user,
        "automatic",
        "automatic",
        "automatic\nuser-invocable: false",
        "UPDATED BODY",
    );
    let after = source
        .skills(
            &session,
            Some("automatic"),
            SkillAudience::Model,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let SessionResourceValue::Read { text: updated, .. } = after else {
        panic!("body")
    };
    assert!(updated.contains("UPDATED BODY"));
    assert!(text.contains("MODEL ONLY"));
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        source
            .skills(&session, None, SkillAudience::Human, cancelled)
            .await
            .is_err()
    );
    let explicit = WorkspaceSkillRequests::from_messages(&[&human_message_for_alias()]).unwrap();
    assert_eq!(explicit.names(), &["manual"]);
}

fn human_message_for_alias() -> AgentMessage {
    human("/skill manual retain these arguments")
}

#[tokio::test]
async fn selected_workspace_loads_project_and_user_sources_by_default() {
    use rsi_agent_session_protocol::{
        AgentPath, ForkOrigin, ForkTurnSelection, ModelSelection, TurnId,
    };
    let temporary = tempfile::tempdir().unwrap();
    let project = temporary.path().join("project");
    let cwd = project.join("nested");
    let user = temporary.path().join("user");
    fs::create_dir_all(project.join(".git")).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&user).unwrap();
    fs::write(project.join("AGENTS.md"), "PROJECT INSTRUCTION").unwrap();
    fs::write(user.join("AGENTS.md"), "USER INSTRUCTION").unwrap();
    write_skill(
        &project.join(".agents/skills"),
        "project-only",
        "project-only",
        "project skill",
        "PROJECT SKILL BODY",
    );
    write_skill(
        &user.join("skills"),
        "user-only",
        "user-only",
        "user skill",
        "USER SKILL BODY",
    );
    let source = context(Some(user.join("AGENTS.md")), vec![user.join("skills")]);

    let created = header(&cwd);
    let restored: SessionHeader =
        serde_json::from_slice(&serde_json::to_vec(&created).unwrap()).unwrap();
    let child = created
        .forked_child(
            SessionId::new("workspace-child").unwrap(),
            2,
            ForkOrigin {
                parent_session_id: created.session_id().clone(),
                root_session_id: created.session_id().clone(),
                path: AgentPath::new(vec![1]).unwrap(),
                task_name: "child".into(),
                parent_header_fingerprint: created.fingerprint().unwrap(),
                invoking_turn_id: TurnId::new("spawn").unwrap(),
                resolved_after_seq: 0,
                resolved_terminal_seq: 0,
                terminal_prefix_sha256: "0".repeat(64),
                resolved_terminal_control_seq: 0,
                terminal_control_prefix_sha256: "0".repeat(64),
                requested_turns: ForkTurnSelection::None,
                effective_turns: 0,
            },
            ModelSelection::baseline(created.settings()),
        )
        .unwrap();
    for session in [created, restored, child] {
        let snapshot = source
            .snapshot(
                &session,
                &WorkspaceSkillRequests::from_messages(&[&human("$project-only")]).unwrap(),
            )
            .await
            .unwrap();
        assert!(snapshot.complete);
        let instructions = snapshot.instructions.unwrap();
        assert!(instructions.contains("USER INSTRUCTION"));
        assert!(instructions.contains("PROJECT INSTRUCTION"));
        let catalog = snapshot.skill_catalog.unwrap();
        assert!(catalog.contains("user-only"));
        assert!(catalog.contains("project-only"));
        assert_eq!(snapshot.invocations.len(), 1);
        assert!(snapshot.invocations[0].text.contains("PROJECT SKILL BODY"));
    }
}

#[tokio::test]
async fn project_instructions_are_root_to_cwd_and_project_skill_wins_name_collision() {
    let temporary = tempfile::tempdir().unwrap();
    let project = temporary.path().join("project");
    let cwd = project.join("a/b");
    let user_skills = temporary.path().join("user-skills");
    fs::create_dir_all(project.join(".git")).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    fs::write(project.join("AGENTS.md"), "ROOT INSTRUCTION").unwrap();
    fs::write(project.join("a/AGENTS.md"), "PARENT INSTRUCTION").unwrap();
    fs::write(cwd.join("AGENTS.md"), "CWD INSTRUCTION").unwrap();
    write_skill(
        &user_skills,
        "shared",
        "shared",
        "user description",
        "USER SELECTED BODY",
    );
    write_skill(
        &project.join(".agents/skills"),
        "shared",
        "shared",
        "project description",
        "PROJECT SHADOW BODY",
    );
    let source = context(None, vec![user_skills]);

    let snapshot = source
        .snapshot(
            &header(&cwd),
            &WorkspaceSkillRequests::from_messages(&[&human("/shared")]).unwrap(),
        )
        .await
        .unwrap();

    let instructions = snapshot.instructions.unwrap();
    let root = instructions.find("ROOT INSTRUCTION").unwrap();
    let parent = instructions.find("PARENT INSTRUCTION").unwrap();
    let cwd = instructions.find("CWD INSTRUCTION").unwrap();
    assert!(root < parent && parent < cwd);
    let catalog = snapshot.skill_catalog.unwrap();
    assert!(!catalog.contains("user description"));
    assert!(catalog.contains("project description"));
    assert_eq!(snapshot.invocations.len(), 1);
    assert!(!snapshot.invocations[0].text.contains("USER SELECTED BODY"));
    assert!(snapshot.invocations[0].text.contains("PROJECT SHADOW BODY"));
}

#[tokio::test]
async fn only_direct_human_input_invokes_a_user_invocable_hidden_skill() {
    let temporary = tempfile::tempdir().unwrap();
    let skills = temporary.path().join("skills");
    write_skill(
        &skills,
        "manual",
        "manual",
        "manual skill\ndisable-model-invocation: true\nuser-invocable: true",
        "MANUAL BODY",
    );
    let source = context(None, vec![skills]);
    let session = SessionId::new("source-agent").unwrap();

    let agent_snapshot = source
        .snapshot(
            &header(temporary.path()),
            &WorkspaceSkillRequests::from_messages(&[&message(
                AgentMessageSource::Agent {
                    source_session_id: session,
                },
                "/manual",
            )])
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(agent_snapshot.skill_catalog.is_none());
    assert!(agent_snapshot.invocations.is_empty());

    let human_snapshot = source
        .snapshot(
            &header(temporary.path()),
            &WorkspaceSkillRequests::from_messages(&[&human("\n /manual argument")]).unwrap(),
        )
        .await
        .unwrap();
    assert!(human_snapshot.skill_catalog.is_none());
    assert_eq!(human_snapshot.invocations.len(), 1);
    assert!(human_snapshot.invocations[0].text.contains("MANUAL BODY"));
}

#[tokio::test]
async fn catalog_discovers_a_large_skill_from_metadata_and_loads_its_body_only_when_invoked() {
    let temporary = tempfile::tempdir().unwrap();
    let skills = temporary.path().join("skills");
    let body = format!("{}TAIL AFTER METADATA PREFIX", "x".repeat(32 * 1024));
    write_skill(&skills, "large", "large", "large body skill", &body);
    let source = context(None, vec![skills]);
    let session = header(temporary.path());

    let catalog = source
        .snapshot(
            &session,
            &WorkspaceSkillRequests::from_messages(&[]).unwrap(),
        )
        .await
        .unwrap();
    assert!(catalog.skill_catalog.unwrap().contains("large body skill"));
    assert!(catalog.invocations.is_empty());

    let invoked = source
        .snapshot(
            &session,
            &WorkspaceSkillRequests::from_messages(&[&human("/large")]).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invoked.invocations.len(), 1);
    assert!(
        invoked.invocations[0]
            .text
            .contains("TAIL AFTER METADATA PREFIX")
    );
}

#[tokio::test]
async fn crlf_skill_frontmatter_is_discovered_and_invoked() {
    let temporary = tempfile::tempdir().unwrap();
    let skills = temporary.path().join("skills");
    let skill = skills.join("crlf/SKILL.md");
    fs::create_dir_all(skill.parent().unwrap()).unwrap();
    fs::write(
        &skill,
        "---\r\nname: crlf\r\ndescription: windows lines\r\n---\r\nCRLF BODY\r\n",
    )
    .unwrap();
    let source = context(None, vec![skills]);

    let snapshot = source
        .snapshot(
            &header(temporary.path()),
            &WorkspaceSkillRequests::from_messages(&[&human("/crlf")]).unwrap(),
        )
        .await
        .unwrap();

    assert!(snapshot.skill_catalog.unwrap().contains("windows lines"));
    assert_eq!(snapshot.invocations.len(), 1);
    assert!(snapshot.invocations[0].text.contains("CRLF BODY"));
}

#[tokio::test]
async fn oversized_optional_sources_are_omitted_from_a_complete_empty_snapshot() {
    let temporary = tempfile::tempdir().unwrap();
    let instruction = temporary.path().join("AGENTS.md");
    let skills = temporary.path().join("skills");
    fs::create_dir_all(&skills).unwrap();
    fs::write(
        &instruction,
        vec![b'x'; MAXIMUM_WORKSPACE_CONTEXT_SOURCE_BYTES + 1],
    )
    .unwrap();
    fs::write(skills.join("broken.md"), "not frontmatter").unwrap();
    let source = context(Some(instruction), vec![skills]);

    let first = source
        .snapshot(
            &header(temporary.path()),
            &WorkspaceSkillRequests::from_messages(&[]).unwrap(),
        )
        .await
        .unwrap();
    let second = source
        .snapshot(
            &header(temporary.path()),
            &WorkspaceSkillRequests::from_messages(&[]).unwrap(),
        )
        .await
        .unwrap();

    assert!(first.complete);
    assert!(first.instructions.is_none());
    assert!(first.skill_catalog.is_none());
    assert_eq!(first.instructions_sha256, second.instructions_sha256);
    assert_eq!(first.skill_catalog_sha256, second.skill_catalog_sha256);
}

#[tokio::test]
async fn session_unsafe_instruction_and_skill_sources_are_omitted() {
    let temporary = tempfile::tempdir().unwrap();
    let instruction = temporary.path().join("AGENTS.md");
    let skills = temporary.path().join("skills");
    fs::write(&instruction, b"unsafe\x7f instruction").unwrap();
    write_skill(
        &skills,
        "unsafe",
        "unsafe",
        "unsafe\0 description",
        "unsafe\0 body",
    );
    let source = context(Some(instruction), vec![skills]);

    let snapshot = source
        .snapshot(
            &header(temporary.path()),
            &WorkspaceSkillRequests::from_messages(&[&human("/unsafe")]).unwrap(),
        )
        .await
        .unwrap();

    assert!(snapshot.complete);
    assert!(snapshot.instructions.is_none());
    assert!(snapshot.skill_catalog.is_none());
    assert!(snapshot.invocations.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn project_skill_links_follow_targets_while_instructions_stay_contained() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let project = temporary.path().join("project");
    let outside = temporary.path().join("outside");
    fs::create_dir_all(project.join(".git")).unwrap();
    fs::create_dir_all(project.join(".agents/skills")).unwrap();
    fs::create_dir_all(outside.join("skill")).unwrap();
    fs::write(outside.join("AGENTS.md"), "OUTSIDE INSTRUCTION").unwrap();
    write_skill(
        &outside,
        "skill",
        "outside",
        "outside description",
        "OUTSIDE SKILL BODY",
    );
    symlink(outside.join("AGENTS.md"), project.join("AGENTS.md")).unwrap();
    symlink(
        outside.join("skill"),
        project.join(".agents/skills/outside"),
    )
    .unwrap();

    let snapshot = context(None, Vec::new())
        .snapshot(
            &header(&project),
            &WorkspaceSkillRequests::from_messages(&[]).unwrap(),
        )
        .await
        .unwrap();

    assert!(snapshot.complete);
    assert!(snapshot.instructions.is_none());
    assert!(snapshot.skill_catalog.unwrap().contains("outside"));
}

#[cfg(unix)]
#[tokio::test]
async fn symlinked_project_skill_root_loads_external_skills() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let project = temporary.path().join("project");
    let outside = temporary.path().join("outside-skills");
    fs::create_dir_all(project.join(".git")).unwrap();
    fs::create_dir_all(project.join(".agents")).unwrap();
    write_skill(&outside, "outside", "outside", "outside", "OUTSIDE");
    symlink(&outside, project.join(".agents/skills")).unwrap();

    let snapshot = context(None, Vec::new())
        .snapshot(
            &header(&project),
            &WorkspaceSkillRequests::from_messages(&[]).unwrap(),
        )
        .await
        .unwrap();

    assert!(snapshot.complete);
    assert!(snapshot.skill_catalog.unwrap().contains("outside"));
}

#[tokio::test]
async fn skill_entry_scan_stops_at_the_declared_bound() {
    let temporary = tempfile::tempdir().unwrap();
    let skills = temporary.path().join("skills");
    for index in 0..=MAXIMUM_WORKSPACE_SKILL_ENTRIES {
        write_skill(
            &skills,
            &format!("skill-{index:03}"),
            &format!("skill-{index:03}"),
            "bounded",
            "BODY",
        );
    }

    let snapshot = context(None, vec![skills])
        .snapshot(
            &header(temporary.path()),
            &WorkspaceSkillRequests::from_messages(&[]).unwrap(),
        )
        .await
        .unwrap();

    assert!(!snapshot.complete);
}

#[tokio::test]
async fn later_skill_root_overflow_is_not_mistaken_for_a_complete_catalog() {
    let temporary = tempfile::tempdir().unwrap();
    let first = temporary.path().join("first-skills");
    let second = temporary.path().join("second-skills");
    for index in 0..MAXIMUM_WORKSPACE_SKILL_ENTRIES {
        write_skill(
            &first,
            &format!("skill-{index:03}"),
            &format!("skill-{index:03}"),
            "bounded",
            "BODY",
        );
    }
    write_skill(&second, "extra", "extra", "extra", "BODY");

    let snapshot = context(None, vec![first, second])
        .snapshot(
            &header(temporary.path()),
            &WorkspaceSkillRequests::from_messages(&[]).unwrap(),
        )
        .await
        .unwrap();

    assert!(!snapshot.complete);
}

#[tokio::test]
async fn project_skill_invocation_exposes_only_a_project_relative_source() {
    let temporary = tempfile::tempdir().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    write_skill(
        &project.join(".agents/skills"),
        "relative",
        "relative",
        "relative source",
        "BODY",
    );

    let snapshot = context(None, Vec::new())
        .snapshot(
            &header(&project),
            &WorkspaceSkillRequests::from_messages(&[&human("/relative")]).unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        snapshot.invocations[0].source,
        ".agents/skills/relative/SKILL.md"
    );
    assert!(
        !snapshot.invocations[0]
            .text
            .contains(temporary.path().to_str().unwrap())
    );
}

#[tokio::test]
async fn instruction_bounds_retain_the_most_specific_project_policy() {
    let temporary = tempfile::tempdir().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    fs::write(
        project.join("AGENTS.md"),
        "x".repeat(MAXIMUM_WORKSPACE_CONTEXT_SOURCE_BYTES),
    )
    .unwrap();
    let mut cwd = project.clone();
    for index in 0..=MAXIMUM_WORKSPACE_INSTRUCTION_FILES {
        cwd.push(format!("d{index}"));
        fs::create_dir_all(&cwd).unwrap();
    }
    fs::write(cwd.join("AGENTS.md"), "MOST SPECIFIC POLICY").unwrap();

    let snapshot = context(None, Vec::new())
        .snapshot(
            &header(&cwd),
            &WorkspaceSkillRequests::from_messages(&[]).unwrap(),
        )
        .await
        .unwrap();

    assert!(
        snapshot
            .instructions
            .as_deref()
            .is_some_and(|text| text.contains("MOST SPECIFIC POLICY"))
    );
}

#[tokio::test]
async fn skills_above_the_source_limit_are_omitted_from_the_catalog() {
    let temporary = tempfile::tempdir().unwrap();
    let skills = temporary.path().join("skills");
    write_skill(
        &skills,
        "oversized",
        "oversized",
        "unloadable skill",
        &"x".repeat(rsi_agent_workspace_context::MAXIMUM_WORKSPACE_CONTEXT_SOURCE_BYTES),
    );
    let source = context(None, vec![skills]);
    let session = header(temporary.path());
    let snapshot = source
        .snapshot(
            &session,
            &WorkspaceSkillRequests::from_messages(&[&human("/oversized")]).unwrap(),
        )
        .await
        .unwrap();
    assert!(snapshot.complete);
    assert!(snapshot.skill_catalog.is_none());
    assert!(snapshot.invocations.is_empty());
}

#[tokio::test]
async fn nested_roots_precede_rsi_and_personal_roots_and_stop_at_git() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("repo");
    let cwd = project.join("nested");
    fs::create_dir_all(project.join(".git")).unwrap();
    let rsi = temp.path().join("config/skills");
    let personal = temp.path().join("home/.agents/skills");
    for (root, body) in [
        (&personal, "PERSONAL"),
        (&rsi, "RSI"),
        (&project.join(".agents/skills"), "ROOT"),
        (&cwd.join(".agents/skills"), "NEAREST"),
    ] {
        write_skill(root, "review", "review", body, body);
    }
    write_skill(
        &temp.path().join(".agents/skills"),
        "outside",
        "outside",
        "outside",
        "OUTSIDE",
    );
    let source = context(None, vec![rsi.clone(), personal]);
    let requests =
        WorkspaceSkillRequests::from_messages(&[&human("请用 $review $review")]).unwrap();
    for (remove, expected) in [
        (None, "NEAREST"),
        (Some(cwd.join(".agents/skills")), "ROOT"),
        (Some(project.join(".agents/skills")), "RSI"),
        (Some(rsi), "PERSONAL"),
    ] {
        if let Some(path) = remove {
            fs::remove_dir_all(path).unwrap();
        }
        let snapshot = source.snapshot(&header(&cwd), &requests).await.unwrap();
        assert!(snapshot.complete);
        assert_eq!(snapshot.invocations.len(), 1);
        assert!(snapshot.invocations[0].text.contains(expected));
        assert!(!snapshot.skill_catalog.unwrap().contains("outside"));
    }
    write_skill(
        &cwd.join(".agents/skills"),
        "review",
        "review",
        "nearest",
        "NEAREST",
    );
    fs::remove_dir_all(project.join(".git")).unwrap();
    let snapshot = source.snapshot(&header(&cwd), &requests).await.unwrap();
    assert!(snapshot.invocations[0].text.contains("NEAREST"));
    assert!(!snapshot.skill_catalog.unwrap().contains("outside"));
}

#[test]
fn dollar_requests_are_ordered_bounded_and_human_only() {
    let human = human("/first then $second $first `$code` \\$escaped\n$third");
    let agent = message(
        AgentMessageSource::Agent {
            source_session_id: SessionId::new("agent").unwrap(),
        },
        "$agent",
    );
    let requests = WorkspaceSkillRequests::from_messages(&[&human, &agent]).unwrap();
    assert_eq!(requests.names(), ["first", "second", "third"]);
    assert!(
        WorkspaceSkillRequests::default()
            .push_text(&(0..4097).fold(String::new(), |mut text, index| {
                use std::fmt::Write as _;
                write!(text, "$name-{index} ").unwrap();
                text
            }))
            .is_err()
    );
}

#[cfg(unix)]
#[path = "context/skill_links.rs"]
mod skill_links;

#[test]
fn repeated_dollar_prose_only_counts_distinct_candidates() {
    let mut requests = WorkspaceSkillRequests::default();
    requests
        .push_text(&"$100 $200 $guide ".repeat(4096))
        .unwrap();
    assert_eq!(requests.names(), ["100", "200", "guide"]);
    let mut requests = WorkspaceSkillRequests::default();
    for index in 0..4096 {
        requests.push_text(&format!("$skill-{index}")).unwrap();
    }
    requests.push_text("$skill-0").unwrap();
    assert_eq!(
        requests.push_text("$overflow"),
        Err(rsi_agent_workspace_context::WorkspaceContextError::Capacity)
    );
}

#[tokio::test]
async fn invalid_optional_body_does_not_discard_other_workspace_context() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join(".git")).unwrap();
    fs::write(temp.path().join("AGENTS.md"), "Valid project instructions").unwrap();
    let skills = temp.path().join(".agents/skills");
    write_skill(
        &skills,
        "bad",
        "bad",
        "Late invalid body",
        &format!("{}\0", "a".repeat(20_000)),
    );
    write_skill(&skills, "good", "good", "Valid body", "Valid skill body");
    let source = context(None, vec![]);
    let requests = WorkspaceSkillRequests::from_messages(&[&human("$bad $good")]).unwrap();
    let snapshot = source
        .snapshot(&header(temp.path()), &requests)
        .await
        .unwrap();
    assert!(snapshot.complete);
    assert!(
        snapshot
            .instructions
            .unwrap()
            .contains("Valid project instructions")
    );
    assert!(snapshot.skill_catalog.unwrap().contains("good"));
    assert_eq!(snapshot.invocations.len(), 1);
    assert_eq!(snapshot.invocations[0].name, "good");
}

#[tokio::test]
async fn instruction_byte_limit_prefers_deepest_without_reaching_the_file_count_limit() {
    let temporary = tempfile::tempdir().unwrap();
    let project = temporary.path().join("project");
    let cwd = project.join("near/deep");
    fs::create_dir_all(project.join(".git")).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    for (directory, marker) in [
        (&project, "ROOT OMITTED"),
        (&project.join("near"), "NEAR RETAINED"),
    ] {
        fs::write(
            directory.join("AGENTS.md"),
            format!(
                "{marker}{}",
                "x".repeat(MAXIMUM_WORKSPACE_CONTEXT_SOURCE_BYTES - marker.len())
            ),
        )
        .unwrap();
    }
    fs::write(cwd.join("AGENTS.md"), "DEEPEST RETAINED").unwrap();
    let snapshot = context(None, Vec::new())
        .snapshot(&header(&cwd), &WorkspaceSkillRequests::default())
        .await
        .unwrap();
    let text = snapshot.instructions.unwrap();
    assert!(snapshot.complete);
    assert!(text.contains("DEEPEST RETAINED") && text.contains("NEAR RETAINED"));
    assert!(!text.contains("ROOT OMITTED"));
    assert!(text.len() <= rsi_agent_workspace_context::MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES);
}

#[tokio::test]
async fn skill_catalog_retains_a_complete_lexical_prefix_within_its_byte_limit() {
    let temporary = tempfile::tempdir().unwrap();
    let skills = temporary.path().join("skills");
    let name = |i| format!("{i:03}{}", "x".repeat(61));
    for index in 0..MAXIMUM_WORKSPACE_SKILL_ENTRIES {
        write_skill(
            &skills,
            &name(index),
            &name(index),
            &"🦀".repeat(500),
            "bounded body",
        );
    }
    let snapshot = context(None, vec![skills])
        .snapshot(
            &header(temporary.path()),
            &WorkspaceSkillRequests::default(),
        )
        .await
        .unwrap();
    assert!(snapshot.complete);
    let catalog = snapshot.skill_catalog.unwrap();
    assert!(catalog.len() <= rsi_agent_workspace_context::MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES);
    assert!(catalog.contains(&name(0)));
    assert!(!catalog.contains(&name(MAXIMUM_WORKSPACE_SKILL_ENTRIES - 1)));
    assert!(catalog.ends_with("</available_skills>"));
}

#[tokio::test]
async fn unavailable_agent_roots_do_not_hide_valid_user_definitions() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir_all(project.join(".agents")).unwrap();
    fs::create_dir_all(project.join(".git")).unwrap();
    let user = temp.path().join("personal");
    fs::create_dir_all(&user).unwrap();
    fs::write(
        user.join("review.md"),
        "---\ndescription: review\n---\nPersona",
    )
    .unwrap();
    let source = LocalWorkspaceContext::new(WorkspaceContextConfig {
        user_agent_roots: vec![user],
        ..Default::default()
    })
    .unwrap();
    let root = project.join(".agents/agents");
    fs::write(&root, "not a directory").unwrap();
    let header = header(&project);
    let reserved = BTreeSet::new();
    let read = || {
        source.agents(
            &header,
            None,
            &reserved,
            tokio_util::sync::CancellationToken::new(),
        )
    };
    assert_eq!(read().await.unwrap()[0].name, "review");
    #[cfg(unix)]
    {
        fs::remove_file(&root).unwrap();
        std::os::unix::fs::symlink("agents", &root).unwrap();
        assert_eq!(read().await.unwrap()[0].name, "review");
    }
}

#[tokio::test]
async fn agent_files_refresh_precedence_and_invalid_winners_are_shared() {
    use tokio_util::sync::CancellationToken;
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let nested = project.join("nested");
    let personal = temp.path().join("personal");
    for dir in [
        project.join(".git"),
        project.join(".agents/agents"),
        nested.join(".agents/agents"),
        personal.clone(),
    ] {
        fs::create_dir_all(dir).unwrap();
    }
    let write = |path: &Path, body: &str| {
        fs::write(
            path,
            format!("---\r\ndescription: review code\r\nallow: []\r\n---\r\n{body}\r\n"),
        )
        .unwrap();
    };
    write(&personal.join("review.md"), "personal");
    write(&project.join(".agents/agents/review.md"), "project");
    let selected = nested.join(".agents/agents/review.md");
    write(&selected, "nearest");
    let source = LocalWorkspaceContext::new(WorkspaceContextConfig {
        user_agent_roots: vec![personal],
        ..Default::default()
    })
    .unwrap();
    let header = header(&nested);
    let reserved = BTreeSet::new();
    let read = || source.agents(&header, Some("review"), &reserved, CancellationToken::new());
    let entries = read().await.unwrap();
    let role = &entries[0].seed.as_ref().unwrap().role;
    assert_eq!(role.persona.as_deref(), Some("nearest"));
    write(&selected, "changed");
    let entries = read().await.unwrap();
    let role = &entries[0].seed.as_ref().unwrap().role;
    assert_eq!(role.persona.as_deref(), Some("changed"));
    fs::write(&selected, "---\ndescription: [invalid\n---\nbody").unwrap();
    let invalid = read().await.unwrap();
    assert!(invalid[0].seed.is_none());
    assert!(
        invalid[0]
            .source
            .ends_with("nested/.agents/agents/review.md")
    );
    fs::remove_file(&selected).unwrap();
    let entries = read().await.unwrap();
    let role = &entries[0].seed.as_ref().unwrap().role;
    assert_eq!(role.persona.as_deref(), Some("project"));
    assert!(
        source
            .agents(
                &header,
                Some("../escape"),
                &BTreeSet::new(),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            project.join(".agents/agents/review.md"),
            nested.join(".agents/agents/linked.md"),
        )
        .unwrap();
        assert!(
            source
                .agents(
                    &header,
                    Some("linked"),
                    &BTreeSet::new(),
                    CancellationToken::new()
                )
                .await
                .unwrap()
                .is_empty()
        );
    }
    fs::write(&selected, "x".repeat(64 * 1024 + 1)).unwrap();
    assert!(read().await.unwrap()[0].description.contains("64 KiB"));
}

#[tokio::test]
async fn agent_catalog_overflow_keeps_a_bounded_prefix_and_exact_lookup() {
    use tokio_util::sync::CancellationToken;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join(".agents/agents");
    fs::create_dir_all(&root).unwrap();
    for n in 0..33 {
        fs::write(
            root.join(format!("role-{n:02}.md")),
            "---\ndescription: bounded role\n---\nPersona",
        )
        .unwrap();
    }
    let source = LocalWorkspaceContext::new(WorkspaceContextConfig::default()).unwrap();
    let header = header(temp.path());
    let entries = source
        .agents(&header, None, &BTreeSet::new(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(entries.len(), 32);
    assert_eq!(entries[0].name, "role-00");
    assert_eq!(entries[31].name, "role-31");
    let exact = source
        .agents(
            &header,
            Some("role-32"),
            &BTreeSet::new(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(exact[0].name, "role-32");
    assert!(exact[0].seed.is_some());
}
