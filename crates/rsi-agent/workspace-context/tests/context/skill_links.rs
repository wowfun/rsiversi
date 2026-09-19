use super::*;
use rsi_agent_session_protocol::SessionResourceValue;
use rsi_agent_workspace_context::{SkillAudience, WorkspaceContextError};
use std::os::unix::fs::symlink;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn directory_links_share_catalog_preview_and_invocation_across_all_roots() {
    for scope in ["project", "config/rsi", "home/.agents"] {
        for location in ["root", "parent", "entry"] {
            for relative in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let base = temp.path().canonicalize().unwrap();
                let project = base.join("project");
                fs::create_dir_all(project.join(".git")).unwrap();
                let logical = if scope == "project" {
                    project.join(".agents/skills")
                } else {
                    base.join(scope).join("skills")
                };
                let external = base.join("external");
                write_skill(
                    &external.join("skills"),
                    "guide",
                    "guide",
                    "LINK GUIDE",
                    "LINK BODY",
                );
                let (link, target) = match location {
                    "root" => (logical.clone(), external.join("skills")),
                    "parent" => (logical.parent().unwrap().to_owned(), external.clone()),
                    "entry" => (logical.join("guide"), external.join("skills/guide")),
                    _ => unreachable!(),
                };
                fs::create_dir_all(link.parent().unwrap()).unwrap();
                let target = if relative {
                    let parent_depth = link
                        .parent()
                        .unwrap()
                        .strip_prefix(&base)
                        .unwrap()
                        .components()
                        .count();
                    let mut path = PathBuf::new();
                    for _ in 0..parent_depth {
                        path.push("..");
                    }
                    path.join(target.strip_prefix(&base).unwrap())
                } else {
                    target
                };
                symlink(target, &link).unwrap();
                let source = context(
                    None,
                    if scope == "project" {
                        vec![]
                    } else {
                        vec![logical.clone()]
                    },
                );
                let session = header(&project);
                let expected_source = if scope == "project" {
                    ".agents/skills/guide/SKILL.md".to_owned()
                } else {
                    logical
                        .join("guide/SKILL.md")
                        .to_string_lossy()
                        .into_owned()
                };
                assert_selected_views(&source, &session, &expected_source).await;
            }
        }
    }
}

async fn assert_selected_views(
    source: &LocalWorkspaceContext,
    session: &SessionHeader,
    expected_source: &str,
) {
    let SessionResourceValue::List { entries } = source
        .skills(
            session,
            None,
            SkillAudience::Human,
            CancellationToken::new(),
        )
        .await
        .unwrap()
    else {
        panic!("catalog");
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].source, expected_source);
    for audience in [SkillAudience::Human, SkillAudience::Model] {
        let SessionResourceValue::Read { resource, text } = source
            .skills(session, Some("guide"), audience, CancellationToken::new())
            .await
            .unwrap()
        else {
            panic!("body");
        };
        assert_eq!(resource, entries[0]);
        assert!(text.contains("LINK BODY"));
    }
    for text in ["/guide", "/skill guide", "please use $guide"] {
        let snapshot = source
            .snapshot(
                session,
                &WorkspaceSkillRequests::from_messages(&[&human(text)]).unwrap(),
            )
            .await
            .unwrap();
        assert!(snapshot.complete);
        assert_eq!(snapshot.invocations.len(), 1);
        assert_eq!(snapshot.invocations[0].source, expected_source);
        assert!(snapshot.invocations[0].text.contains("LINK BODY"));
    }
}

#[tokio::test]
async fn aliases_keep_first_logical_source_and_existing_name_precedence() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let root = project.join(".agents/skills");
    fs::create_dir_all(project.join(".git")).unwrap();
    fs::create_dir_all(&root).unwrap();
    let external = temp.path().join("external");
    write_skill(&external, "guide", "guide", "EXTERNAL", "EXTERNAL BODY");
    symlink(external.join("guide"), root.join("a-first")).unwrap();
    symlink(external.join("guide"), root.join("z-alias")).unwrap();
    let user = temp.path().join("user");
    write_skill(&user, "guide", "guide", "SHADOWED", "USER BODY");
    symlink(external.join("guide"), user.join("alias")).unwrap();
    let source = context(None, vec![user]);
    let session = header(&project);
    let SessionResourceValue::List { entries } = source
        .skills(
            &session,
            None,
            SkillAudience::Human,
            CancellationToken::new(),
        )
        .await
        .unwrap()
    else {
        panic!("catalog");
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].source, ".agents/skills/a-first/SKILL.md");
    let SessionResourceValue::Read { text, .. } = source
        .skills(
            &session,
            Some("guide"),
            SkillAudience::Model,
            CancellationToken::new(),
        )
        .await
        .unwrap()
    else {
        panic!("body");
    };
    assert!(text.contains("EXTERNAL BODY"));
    fs::write(
        external.join("guide/SKILL.md"),
        "---\nname: guide\ndescription: UPDATED\n---\nUPDATED BODY\n",
    )
    .unwrap();
    let SessionResourceValue::Read { resource, text } = source
        .skills(
            &session,
            Some("guide"),
            SkillAudience::Model,
            CancellationToken::new(),
        )
        .await
        .unwrap()
    else {
        panic!("body");
    };
    assert_eq!(resource.description, "UPDATED");
    assert!(text.contains("UPDATED BODY"));
}

#[tokio::test]
async fn file_links_dangling_links_and_cycles_do_not_hide_valid_skills() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("skills");
    write_skill(&root, "valid", "valid", "VALID", "VALID BODY");
    let external = temp.path().join("external");
    write_skill(&external, "linked", "linked", "FILE LINK", "FILE BODY");
    fs::create_dir_all(root.join("file-link")).unwrap();
    symlink(
        external.join("linked/SKILL.md"),
        root.join("file-link/SKILL.md"),
    )
    .unwrap();
    symlink(external.join("linked/SKILL.md"), root.join("standalone.md")).unwrap();
    symlink("missing-directory", root.join("dangling")).unwrap();
    symlink("cycle-b", root.join("cycle-a")).unwrap();
    symlink("cycle-a", root.join("cycle-b")).unwrap();
    symlink("loop-root", temp.path().join("loop-root")).unwrap();
    let source = context(None, vec![temp.path().join("loop-root"), root]);
    let snapshot = source
        .snapshot(
            &header(temp.path()),
            &WorkspaceSkillRequests::from_messages(&[&human("$valid $linked")]).unwrap(),
        )
        .await
        .unwrap();
    assert!(snapshot.complete);
    assert_eq!(snapshot.invocations.len(), 1);
    assert_eq!(snapshot.invocations[0].name, "valid");
    assert!(!snapshot.skill_catalog.unwrap().contains("FILE LINK"));
}

#[tokio::test]
async fn link_entries_count_before_alias_deduplication() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("skills");
    let external = temp.path().join("external");
    write_skill(&external, "guide", "guide", "GUIDE", "BODY");
    fs::create_dir_all(&root).unwrap();
    for index in 0..=MAXIMUM_WORKSPACE_SKILL_ENTRIES {
        symlink(
            external.join("guide"),
            root.join(format!("alias-{index:03}")),
        )
        .unwrap();
    }
    let source = context(None, vec![root]);
    let snapshot = source
        .snapshot(&header(temp.path()), &WorkspaceSkillRequests::default())
        .await
        .unwrap();
    assert!(!snapshot.complete);
}

#[tokio::test]
async fn exact_linked_reads_distinguish_absence_and_each_invocation_restriction() {
    let temp = tempfile::tempdir().unwrap();
    let external = temp.path().join("external");
    write_skill(
        &external,
        "manual",
        "manual",
        "MANUAL\ndisable-model-invocation: true",
        "MANUAL BODY",
    );
    write_skill(
        &external,
        "automatic",
        "automatic",
        "AUTO\nuser-invocable: false",
        "AUTO BODY",
    );
    symlink(&external, temp.path().join("skills")).unwrap();
    let source = context(None, vec![temp.path().join("skills")]);
    let header = header(temp.path());
    for (name, audience, message) in [
        (
            "absent",
            SkillAudience::Model,
            "skill was not found in the current catalog",
        ),
        (
            "manual",
            SkillAudience::Model,
            "skill does not allow model invocation",
        ),
        (
            "automatic",
            SkillAudience::Human,
            "skill does not allow user invocation",
        ),
    ] {
        assert_eq!(
            source
                .skills(&header, Some(name), audience, CancellationToken::new())
                .await,
            Err(WorkspaceContextError::Invalid(message.into()))
        );
    }
}
