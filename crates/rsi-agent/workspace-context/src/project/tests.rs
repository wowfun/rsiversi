use super::*;

fn skill(root: &Path, name: &str, flags: &str, body: &str) {
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join(format!("{name}.md")),
        format!("---\nname: {name}\ndescription: {name} description\n{flags}---\n{body}\n"),
    )
    .unwrap();
}
fn budget(config: &WorkspaceContextConfig, cwd: &Path) -> SnapshotBudget {
    SnapshotBudget::new(config, cwd, CancellationToken::new()).unwrap()
}
fn context(cwd: &Path, request: &Request) -> Context {
    let capture = collect(cwd, request, CancellationToken::new()).unwrap();
    capture.validate(request).unwrap();
    let bytes = encode(&Reply::Captured { capture }, MAXIMUM_RESPONSE_BYTES).unwrap();
    let Reply::Captured { capture } = serde_json::from_slice(&bytes).unwrap() else {
        panic!("capture")
    };
    let Capture::Context(context) = capture else {
        panic!("context")
    };
    context
}

#[test]
fn partitioned_sources_preserve_native_precedence_rendering_and_invocation_order() {
    let project = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    let cwd = project.path().join("nested");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir(project.path().join(".git")).unwrap();
    fs::write(
        project.path().join("AGENTS.md"),
        "broad project instructions",
    )
    .unwrap();
    fs::write(cwd.join("AGENTS.md"), "specific project instructions").unwrap();
    let user_file = user.path().join("private-user.md");
    fs::write(&user_file, "private Service instructions").unwrap();
    skill(
        &project.path().join(".agents/skills"),
        "same",
        "",
        "project wins",
    );
    skill(&cwd.join(".agents/skills"), "same", "", "nearest wins");
    skill(
        &cwd.join(".agents/skills"),
        "model-only",
        "user-invocable: false\n",
        "model body",
    );
    skill(user.path(), "same", "", "losing Service skill");
    skill(user.path(), "global", "", "Service global body");
    let config = WorkspaceContextConfig {
        user_instruction_file: Some(user_file),
        user_skill_roots: vec![user.path().into()],
        user_agent_roots: vec![],
    };
    let names = vec!["global".into(), "same".into(), "model-only".into()];
    let native = snapshot_blocking(&config, &cwd, &names).unwrap();
    let user_sections =
        read_user_sections(&config, &budget(&config, &cwd), &mut Observation::default()).unwrap();
    let request = Request::Snapshot {
        names: names.clone(),
        retained_instruction_bytes: instruction_retained_bytes(&user_sections),
    };
    let wire = String::from_utf8(encode(&request, MAXIMUM_REQUEST_BYTES).unwrap()).unwrap();
    assert!(!wire.contains("private"));
    assert!(!wire.contains(user.path().to_str().unwrap()));
    let target = context(&cwd, &request);
    assert!(!target.skills.contains_key("global"));
    assert!(
        target
            .instructions
            .iter()
            .all(|(_, text)| !text.contains("Service"))
    );
    let combined = add_user_sources(&config, &request, target, budget(&config, &cwd)).unwrap();
    assert_eq!(snapshot(combined, &user_sections, &names), native);
    assert_eq!(
        native
            .invocations
            .iter()
            .map(|value| value.name.as_str())
            .collect::<Vec<_>>(),
        ["global", "same"]
    );
    assert!(native.invocations[1].text.contains("nearest wins"));
}

#[test]
fn combined_skill_entry_budget_withholds_partial_observation() {
    let project = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    skill(
        &project.path().join(".agents/skills"),
        "project",
        "",
        "body",
    );
    for index in 0..MAXIMUM_WORKSPACE_SKILL_ENTRIES {
        skill(user.path(), &format!("user-{index:03}"), "", "body");
    }
    let config = WorkspaceContextConfig {
        user_skill_roots: vec![user.path().into()],
        ..WorkspaceContextConfig::default()
    };
    let request = Request::Skills {
        id: None,
        audience: SkillAudience::Human,
    };
    let target = context(project.path(), &request);
    assert_eq!(target.inspected, 1);
    let combined =
        add_user_sources(&config, &request, target, budget(&config, project.path())).unwrap();
    assert_eq!(combined.inspected, MAXIMUM_WORKSPACE_SKILL_ENTRIES);
    assert!(
        combined
            .diagnostic
            .as_deref()
            .unwrap()
            .contains("scan limit")
    );
    assert!(resource(combined, None, SkillAudience::Human).is_err());
}

#[test]
fn restricted_project_skill_cannot_fall_back_to_a_service_definition() {
    let project = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    skill(
        &project.path().join(".agents/skills"),
        "same",
        "disable-model-invocation: true\n",
        "project body",
    );
    skill(user.path(), "same", "", "must not be selected");
    let config = WorkspaceContextConfig {
        user_skill_roots: vec![user.path().into()],
        ..WorkspaceContextConfig::default()
    };
    let request = Request::Skills {
        id: Some("same".into()),
        audience: SkillAudience::Model,
    };
    let target = context(project.path(), &request);
    assert!(target.skills["same"].body.is_none());
    let combined =
        add_user_sources(&config, &request, target, budget(&config, project.path())).unwrap();
    assert!(matches!(
        resource(combined, Some("same"), SkillAudience::Model),
        Err(WorkspaceContextError::Invalid(_))
    ));
}

#[test]
fn agent_partitions_keep_the_shared_listing_prefix_and_reserved_collisions() {
    let project = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    let roots = project.path().join(".agents/agents");
    fs::create_dir_all(&roots).unwrap();
    for index in 0..33 {
        fs::write(
            roots.join(format!("role-{index:02}.md")),
            "---\ndescription: project role\n---\nProject persona",
        )
        .unwrap();
    }
    fs::write(
        user.path().join("role-00.md"),
        "---\ndescription: losing user role\n---\nUser persona",
    )
    .unwrap();
    fs::write(
        user.path().join("user.md"),
        "---\ndescription: user role\n---\nUser persona",
    )
    .unwrap();
    let reserved = BTreeSet::from(["role-32".into()]);
    let request = Request::Agents {
        id: None,
        reserved: reserved.clone(),
    };
    let capture = collect(project.path(), &request, CancellationToken::new()).unwrap();
    capture.validate(&request).unwrap();
    let mut corrupted: Capture =
        serde_json::from_value(serde_json::to_value(&capture).unwrap()).unwrap();
    let Capture::Agents(changed) = &mut corrupted else {
        panic!("agents");
    };
    changed
        .selected
        .values_mut()
        .find(|entry| entry.seed.is_some())
        .unwrap()
        .text
        .push_str("changed");
    assert!(corrupted.validate(&request).is_err());
    let Capture::Agents(collection) = capture else {
        panic!("agents")
    };
    let config = WorkspaceContextConfig {
        user_agent_roots: vec![user.path().into()],
        ..WorkspaceContextConfig::default()
    };
    let combined = agents::read_partition(
        &config,
        project.path(),
        agents::Selection {
            id: None,
            reserved_names: &reserved,
            project: false,
        },
        budget(&config, project.path()),
        collection,
    )
    .unwrap();
    let native = agents::read_agents(
        &config,
        project.path(),
        None,
        &reserved,
        budget(&config, project.path()),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(combined.selected.into_values().collect::<Vec<_>>()).unwrap(),
        serde_json::to_value(native).unwrap()
    );
}

#[test]
fn project_exchange_rejects_unbounded_names_unknown_fields_and_invalid_counts() {
    assert!(
        serde_json::from_str::<Request>(
            r#"{"kind":"skills","id":null,"audience":"human","user_root":"/secret"}"#
        )
        .is_err()
    );
    assert!(
        Request::Snapshot {
            names: vec!["same".into(); 4097],
            retained_instruction_bytes: INSTRUCTIONS_PREAMBLE.len()
        }
        .validate()
        .is_err()
    );
    assert!(
        Request::Skills {
            id: Some("../secret".into()),
            audience: SkillAudience::Human
        }
        .validate()
        .is_err()
    );
    let context = Context {
        inspected: MAXIMUM_WORKSPACE_SKILL_ENTRIES + 1,
        ..Context::default()
    };
    assert!(
        Capture::Context(context)
            .validate(&Request::Skills {
                id: None,
                audience: SkillAudience::Human
            })
            .is_err()
    );
    assert!(encode(&"too long", 3).is_err());
}
