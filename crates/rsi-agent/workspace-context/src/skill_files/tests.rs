use super::*;
#[cfg(unix)]
use std::os::unix::fs::symlink as symlink_file;
#[cfg(windows)]
use std::os::windows::fs::symlink_file;

fn write_skill(directory: &Path, description: &str, body: &str) {
    fs::create_dir_all(directory).unwrap();
    fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: guide\ndescription: {description}\n---\n{body}\n"),
    )
    .unwrap();
}

fn select(root: &Path) -> Vec<SelectedSkill> {
    let config = WorkspaceContextConfig {
        user_instruction_file: None,
        user_skill_roots: vec![root.to_owned()],
    };
    let mut budget = SnapshotBudget::new(&config, root, CancellationToken::new()).unwrap();
    let mut observation = Observation::default();
    let selected =
        skills::discover_selected(&config, root, None, &mut observation, &mut budget).unwrap();
    assert!(observation.is_complete());
    selected
}

#[cfg(unix)]
#[test]
fn selected_directory_survives_retarget_and_rename_until_the_next_observation() {
    use std::os::unix::fs::symlink;
    use std::sync::mpsc::sync_channel;
    for root_link in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let external = temp.path().join("external");
        let replacement = temp.path().join("replacement");
        let held = temp.path().join("held");
        write_skill(&external.join("guide"), "GUIDE", "ORIGINAL BODY");
        write_skill(&replacement.join("guide"), "GUIDE", "NEW BODY");
        let root = temp.path().join("skills");
        let link = if root_link {
            symlink(&external, &root).unwrap();
            root.clone()
        } else {
            fs::create_dir_all(&root).unwrap();
            let link = root.join("guide");
            symlink(external.join("guide"), &link).unwrap();
            link
        };
        let (selected_tx, selected_rx) = sync_channel(0);
        let (changed_tx, changed_rx) = sync_channel(0);
        std::thread::scope(|scope| {
            let writer = scope.spawn(move || {
                selected_rx.recv().unwrap();
                fs::rename(&external, &held).unwrap();
                write_skill(&external.join("guide"), "GUIDE", "REPLACED PATH BODY");
                fs::remove_file(&link).unwrap();
                symlink(
                    if root_link {
                        replacement.clone()
                    } else {
                        replacement.join("guide")
                    },
                    &link,
                )
                .unwrap();
                changed_tx.send(()).unwrap();
            });
            let selected = select(&root);
            selected_tx.send(()).unwrap();
            changed_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            let mut observation = Observation::default();
            let invocation =
                read_skill_invocation(&selected[0], &mut observation, &CancellationToken::new())
                    .unwrap();
            assert!(observation.is_complete());
            assert!(invocation.text.contains("ORIGINAL BODY"));
            assert!(!invocation.text.contains("REPLACED PATH BODY"));
            let refreshed = select(&root);
            let invocation =
                read_skill_invocation(&refreshed[0], &mut observation, &CancellationToken::new())
                    .unwrap();
            assert!(observation.is_complete());
            assert!(invocation.text.contains("NEW BODY"));
            writer.join().unwrap();
        });
    }
}

#[test]
fn selected_missing_or_changed_metadata_never_publishes_a_partial_invocation() {
    for change in ["remove", "rename", "description", "model-flag", "user-flag"] {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("guide");
        write_skill(&directory, "GUIDE", "BODY");
        let selected = select(temp.path());
        std::thread::scope(|scope| {
            scope
                .spawn(|| match change {
                    "remove" => fs::remove_file(directory.join("SKILL.md")).unwrap(),
                    "rename" => fs::write(
                        directory.join("SKILL.md"),
                        "---\nname: changed\ndescription: GUIDE\n---\nBODY",
                    )
                    .unwrap(),
                    "description" => write_skill(&directory, "DIFFERENT", "BODY"),
                    "model-flag" => {
                        write_skill(&directory, "GUIDE\ndisable-model-invocation: true", "BODY");
                    }
                    "user-flag" => write_skill(&directory, "GUIDE\nuser-invocable: false", "BODY"),
                    _ => unreachable!(),
                })
                .join()
                .unwrap();
        });
        let mut observation = Observation::default();
        assert!(
            read_skill_invocation(&selected[0], &mut observation, &CancellationToken::new())
                .is_none()
        );
        assert!(!observation.is_complete(), "{change}");
        assert!(
            observation
                .diagnostic
                .as_ref()
                .unwrap()
                .contains("SKILL.md")
        );
        if change == "remove" {
            assert!(
                select(temp.path()).is_empty(),
                "the next discovery is complete"
            );
        }
    }
}

#[cfg(any(unix, windows))]
#[test]
fn selected_file_replaced_with_a_link_is_not_followed() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("guide");
    write_skill(&directory, "GUIDE", "BODY");
    let selected = select(temp.path());
    let target = temp.path().join("external.txt");
    fs::rename(directory.join("SKILL.md"), &target).unwrap();
    symlink_file(target, directory.join("SKILL.md")).unwrap();
    let mut observation = Observation::default();
    assert!(
        read_skill_invocation(&selected[0], &mut observation, &CancellationToken::new()).is_none()
    );
    assert!(!observation.is_complete());
    assert!(
        select(temp.path()).is_empty(),
        "the next discovery omits the file link"
    );
}

#[cfg(any(unix, windows))]
#[test]
fn skill_file_links_are_complete_omissions_during_discovery() {
    let temp = tempfile::tempdir().unwrap();
    write_skill(&temp.path().join("guide"), "GUIDE", "BODY");
    let target = temp.path().join("guide/SKILL.md");
    let linked = temp.path().join("linked");
    fs::create_dir(&linked).unwrap();
    symlink_file(&target, linked.join("SKILL.md")).unwrap();
    symlink_file(&target, temp.path().join("standalone.md")).unwrap();
    let selected = select(temp.path());
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].file.logical_path, target);
}

#[test]
fn duplicate_identity_keeps_first_metadata_even_if_the_target_changes_name() {
    let temp = tempfile::tempdir().unwrap();
    write_skill(&temp.path().join("guide"), "GUIDE", "BODY");
    let mut discovery = SkillDiscovery::default();
    let mut budget = SnapshotBudget::new(
        &WorkspaceContextConfig::default(),
        temp.path(),
        CancellationToken::new(),
    )
    .unwrap();
    let mut observation = Observation::default();
    discovery
        .scan(temp.path(), None, &mut observation, &mut budget)
        .unwrap();
    fs::write(
        temp.path().join("guide/SKILL.md"),
        "---\nname: renamed\ndescription: GUIDE\n---\nBODY",
    )
    .unwrap();
    discovery
        .scan(temp.path(), None, &mut observation, &mut budget)
        .unwrap();
    assert!(observation.is_complete());
    let selected = discovery.into_selected();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].name, "guide");
}

#[test]
fn permission_failures_are_incomplete_and_resolved_storage_is_budgeted() {
    let mut observation = Observation::default();
    assert!(
        optional_directory_result::<()>(
            Err(std::io::ErrorKind::PermissionDenied.into()),
            Path::new("denied"),
            &mut observation
        )
        .is_none()
    );
    assert!(!observation.is_complete());
    let temp = tempfile::tempdir().unwrap();
    write_skill(temp.path(), "GUIDE", "BODY");
    let directory = Arc::new(SkillDirectory::open(temp.path()).unwrap());
    let source = SkillSource::new(PathBuf::from("/logical/SKILL.md"), directory);
    let bytes = source.retained_bytes().unwrap();
    assert!(
        bytes
            > source.logical_path.capacity()
                + source.resolved_path.capacity()
                + source.directory.path.capacity()
    );
    let config = WorkspaceContextConfig::default();
    let mut budget = SnapshotBudget::new(&config, temp.path(), CancellationToken::new()).unwrap();
    // Leave less than one selected source's retained storage available.
    while budget.reserve(bytes).is_ok() {}
    assert_eq!(
        SkillDiscovery::default().select(source, None, &mut Observation::default(), &mut budget),
        Err(WorkspaceContextError::Capacity)
    );
}
