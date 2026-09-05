use rsi_agent_session_protocol::{
    AgentMessage, AgentMessageContent, AgentMessageSource, AgentPresetId, FrozenAgentSettings,
    MessageId, MessageOptions, SessionHeader, SessionId, WorkspaceTrust,
};
use rsi_agent_workspace_context::{
    LocalWorkspaceContext, MAXIMUM_WORKSPACE_CONTEXT_SOURCE_BYTES,
    MAXIMUM_WORKSPACE_INSTRUCTION_FILES, MAXIMUM_WORKSPACE_SKILL_ENTRIES, WorkspaceContext,
    WorkspaceContextConfig,
};
use rsi_ai_protocol::ModelRef;
use rsi_sandbox::SandboxMode;
use std::fs;
use std::path::{Path, PathBuf};

fn header(cwd: &Path, trust: WorkspaceTrust) -> SessionHeader {
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
    .with_workspace_trust(trust)
    .unwrap()
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
        user_instruction_file,
        user_skill_roots,
    })
    .unwrap()
}

#[tokio::test]
async fn untrusted_workspace_omits_every_project_controlled_source() {
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

    let snapshot = source
        .snapshot(&header(&cwd, WorkspaceTrust::Untrusted), &[])
        .await
        .unwrap();

    assert!(snapshot.complete);
    let instructions = snapshot.instructions.unwrap();
    assert!(instructions.contains("USER INSTRUCTION"));
    assert!(!instructions.contains("PROJECT INSTRUCTION"));
    let catalog = snapshot.skill_catalog.unwrap();
    assert!(catalog.contains("user-only"));
    assert!(!catalog.contains("project-only"));
}

#[tokio::test]
async fn trusted_project_instructions_are_root_to_cwd_and_user_skill_wins_name_collision() {
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
        .snapshot(&header(&cwd, WorkspaceTrust::Trusted), &[&human("/shared")])
        .await
        .unwrap();

    let instructions = snapshot.instructions.unwrap();
    let root = instructions.find("ROOT INSTRUCTION").unwrap();
    let parent = instructions.find("PARENT INSTRUCTION").unwrap();
    let cwd = instructions.find("CWD INSTRUCTION").unwrap();
    assert!(root < parent && parent < cwd);
    let catalog = snapshot.skill_catalog.unwrap();
    assert!(catalog.contains("user description"));
    assert!(!catalog.contains("project description"));
    assert_eq!(snapshot.invocations.len(), 1);
    assert!(snapshot.invocations[0].text.contains("USER SELECTED BODY"));
    assert!(!snapshot.invocations[0].text.contains("PROJECT SHADOW BODY"));
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
            &header(temporary.path(), WorkspaceTrust::Untrusted),
            &[&message(
                AgentMessageSource::Agent {
                    source_session_id: session,
                },
                "/manual",
            )],
        )
        .await
        .unwrap();
    assert!(agent_snapshot.skill_catalog.is_none());
    assert!(agent_snapshot.invocations.is_empty());

    let human_snapshot = source
        .snapshot(
            &header(temporary.path(), WorkspaceTrust::Untrusted),
            &[&human("\n /manual argument")],
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
    let session = header(temporary.path(), WorkspaceTrust::Untrusted);

    let catalog = source.snapshot(&session, &[]).await.unwrap();
    assert!(catalog.skill_catalog.unwrap().contains("large body skill"));
    assert!(catalog.invocations.is_empty());

    let invoked = source
        .snapshot(&session, &[&human("/large")])
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
            &header(temporary.path(), WorkspaceTrust::Untrusted),
            &[&human("/crlf")],
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
        .snapshot(&header(temporary.path(), WorkspaceTrust::Untrusted), &[])
        .await
        .unwrap();
    let second = source
        .snapshot(&header(temporary.path(), WorkspaceTrust::Untrusted), &[])
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
            &header(temporary.path(), WorkspaceTrust::Untrusted),
            &[&human("/unsafe")],
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
async fn trusted_project_sources_never_follow_links_outside_the_project() {
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
        .snapshot(&header(&project, WorkspaceTrust::Trusted), &[])
        .await
        .unwrap();

    assert!(snapshot.complete);
    assert!(snapshot.instructions.is_none());
    assert!(snapshot.skill_catalog.is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn symlinked_project_skill_root_is_a_complete_omission() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let project = temporary.path().join("project");
    let outside = temporary.path().join("outside-skills");
    fs::create_dir_all(project.join(".git")).unwrap();
    fs::create_dir_all(project.join(".agents")).unwrap();
    write_skill(&outside, "outside", "outside", "outside", "OUTSIDE");
    symlink(&outside, project.join(".agents/skills")).unwrap();

    let snapshot = context(None, Vec::new())
        .snapshot(&header(&project, WorkspaceTrust::Trusted), &[])
        .await
        .unwrap();

    assert!(snapshot.complete);
    assert!(snapshot.skill_catalog.is_none());
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
        .snapshot(&header(temporary.path(), WorkspaceTrust::Untrusted), &[])
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
        .snapshot(&header(temporary.path(), WorkspaceTrust::Untrusted), &[])
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
            &header(&project, WorkspaceTrust::Trusted),
            &[&human("/relative")],
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
        .snapshot(&header(&cwd, WorkspaceTrust::Trusted), &[])
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
    let session = header(temporary.path(), WorkspaceTrust::Untrusted);
    let snapshot = source
        .snapshot(&session, &[&human("/oversized")])
        .await
        .unwrap();
    assert!(snapshot.complete);
    assert!(snapshot.skill_catalog.is_none());
    assert!(snapshot.invocations.is_empty());
}
