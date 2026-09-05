#![allow(clippy::wildcard_imports)]

use super::*;
use std::fmt::Write as _;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
#[cfg(unix)]
use std::os::unix::fs::symlink;

fn patch(root: &Path, body: &str) -> PatchHelperResponse {
    apply_patch(root, &format!("*** Begin Patch\n{body}\n*** End Patch\n"))
}

#[test]
fn applies_add_update_move_delete_after_full_preflight() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("old.txt"), "old\n").unwrap();
    fs::write(root.path().join("delete.txt"), "gone\n").unwrap();
    let result = patch(
        root.path(),
        "*** Add File: nested/add.txt\n+added\n*** Update File: old.txt\n*** Move to: moved/new.txt\n@@\n-old\n+new\n*** Delete File: delete.txt",
    );
    assert_eq!(result.status, PatchStatus::Applied);
    assert!(result.delta_exact);
    assert_eq!(
        fs::read(root.path().join("nested/add.txt")).unwrap(),
        b"added\n"
    );
    assert_eq!(
        fs::read(root.path().join("moved/new.txt")).unwrap(),
        b"new\n"
    );
    assert!(!root.path().join("old.txt").exists());
    assert!(!root.path().join("delete.txt").exists());
}

#[test]
fn preflight_failure_has_no_file_effects() {
    let root = tempfile::tempdir().unwrap();
    let result = patch(
        root.path(),
        "*** Add File: created.txt\n+created\n*** Update File: missing.txt\n@@\n-old\n+new",
    );
    assert_eq!(result.status, PatchStatus::Rejected);
    assert!(result.effects.is_empty());
    assert!(!root.path().join("created.txt").exists());
    assert_eq!(result.failure.unwrap().operation, Some(1));
}

#[test]
fn matcher_uses_global_layer_priority_and_audits_only_fuzzy_matches() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("match.txt"), "target   \nother\ntarget\n").unwrap();
    let result = patch(
        root.path(),
        "*** Update File: match.txt\n@@\n-target\n+changed",
    );
    assert_eq!(result.status, PatchStatus::Applied);
    assert!(result.fuzzy_matches.is_empty());
    assert_eq!(
        fs::read_to_string(root.path().join("match.txt")).unwrap(),
        "target   \nother\nchanged\n"
    );

    fs::write(root.path().join("unicode.txt"), "say “hello”\n").unwrap();
    let fuzzy = patch(
        root.path(),
        "*** Update File: unicode.txt\n@@\n-say \"hello\"\n+done",
    );
    assert_eq!(fuzzy.status, PatchStatus::Applied);
    assert_eq!(fuzzy.fuzzy_matches.len(), 1);
    assert_eq!(fuzzy.fuzzy_matches[0].kind, MatchKind::Unicode);
    assert!(fuzzy.delta_exact);
}

#[test]
fn normalizes_mixed_line_endings_and_terminates_nonempty_output() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("mixed.txt"), b"one\r\ntwo\rthree\nfour").unwrap();
    let result = patch(
        root.path(),
        "*** Update File: mixed.txt\n@@\n one\n two\n-three\n+THREE\n four",
    );
    assert_eq!(result.status, PatchStatus::Applied);
    assert_eq!(
        fs::read(root.path().join("mixed.txt")).unwrap(),
        b"one\r\ntwo\rTHREE\r\nfour\r\n"
    );
}

#[test]
fn rejects_parent_escape_symlinks_special_files_and_controls() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    symlink(outside.path(), root.path().join("link")).unwrap();
    #[cfg(unix)]
    {
        let symlink_result = patch(root.path(), "*** Add File: link/escape.txt\n+bad");
        assert_eq!(symlink_result.status, PatchStatus::Rejected);
        assert!(!outside.path().join("escape.txt").exists());
    }
    let escape = patch(root.path(), "*** Add File: ../escape.txt\n+bad");
    assert_eq!(escape.failure.unwrap().code, "invalid_path");
    let control = apply_patch(root.path(), "*** Begin Patch\n\0*** End Patch\n");
    assert_eq!(control.failure.unwrap().code, "invalid_patch_text");
}

#[test]
fn repeated_operations_use_virtual_preflight_state() {
    let root = tempfile::tempdir().unwrap();
    let result = patch(
        root.path(),
        "*** Add File: same.txt\n+one\n*** Update File: same.txt\n@@\n-one\n+two",
    );
    assert_eq!(result.status, PatchStatus::Applied);
    assert_eq!(fs::read(root.path().join("same.txt")).unwrap(), b"two\n");
}

#[test]
fn commit_failure_reports_the_exact_applied_prefix_without_replay_guessing() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("second.txt"), "old\n").unwrap();
    let document = concat!(
        "*** Begin Patch\n",
        "*** Add File: first.txt\n",
        "+first\n",
        "*** Update File: second.txt\n",
        "@@\n",
        "-old\n",
        "+new\n",
        "*** End Patch\n"
    );
    let result = apply_patch_before_commit(root.path(), document, |operation| {
        if operation == 1 {
            fs::write(root.path().join("second.txt"), "raced\n").unwrap();
        }
    });
    assert_eq!(result.status, PatchStatus::Partial);
    assert!(result.delta_exact);
    assert_eq!(result.effects.len(), 1);
    assert_eq!(result.effects[0].kind, PatchEffectKind::Add);
    let failure = result.failure.unwrap();
    assert_eq!(failure.operation, Some(1));
    assert_eq!(failure.code, "changed_since_preflight");
    assert_eq!(fs::read(root.path().join("first.txt")).unwrap(), b"first\n");
    assert_eq!(
        fs::read(root.path().join("second.txt")).unwrap(),
        b"raced\n"
    );
}

#[cfg(unix)]
#[test]
fn new_files_respect_the_helper_process_umask() {
    const CHILD: &str = "RSI_APPLY_PATCH_UMASK_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let root = tempfile::tempdir().unwrap();
        let result = patch(root.path(), "*** Add File: private.txt\n+secret");
        assert_eq!(result.status, PatchStatus::Applied);
        assert_eq!(
            fs::metadata(root.path().join("private.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        return;
    }

    let current = std::env::current_exe().unwrap();
    let output = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "umask 077; exec \"$1\" \"$2\" --exact --nocapture",
            "rsi-apply-patch-umask",
        ])
        .arg(current)
        .arg("patch_engine::tests::new_files_respect_the_helper_process_umask")
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "umask child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn update_preserves_executable_mode() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("script.sh");
    fs::write(&target, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();

    let result = patch(
        root.path(),
        "*** Update File: script.sh\n@@\n-exit 0\n+exit 1",
    );

    assert_eq!(result.status, PatchStatus::Applied);
    assert_eq!(
        fs::metadata(target).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[test]
fn preflight_rejects_a_path_whose_directory_effects_exceed_the_response_bound() {
    let root = tempfile::tempdir().unwrap();
    let path = format!("{}file.txt", "d/".repeat(MAXIMUM_PATCH_OPERATIONS * 3));
    let result = patch(root.path(), &format!("*** Add File: {path}\n+x"));

    assert_eq!(result.status, PatchStatus::Rejected);
    assert!(result.effects.is_empty());
    assert_eq!(result.failure.unwrap().code, "effect_budget");
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn preflight_rejects_effect_metadata_that_cannot_fit_the_helper_capture() {
    let root = tempfile::tempdir().unwrap();
    let component = "d".repeat(230);
    let mut document = String::from("*** Begin Patch\n");
    for operation in 0..11 {
        let mut components = vec![format!("root-{operation}")];
        components.extend((0..67).map(|index| format!("{index:02}-{component}")));
        components.push("file.txt".into());
        write!(document, "*** Add File: {}\n+value\n", components.join("/")).unwrap();
    }
    document.push_str("*** End Patch\n");

    let result = apply_patch(root.path(), &document);

    assert_eq!(result.status, PatchStatus::Rejected);
    assert!(result.effects.is_empty());
    assert_eq!(result.failure.unwrap().code, "response_budget");
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn preflight_rejects_more_fuzzy_audits_than_the_response_contract_can_carry() {
    let root = tempfile::tempdir().unwrap();
    let source = (0..=MAXIMUM_PATCH_FUZZY_MATCHES).fold(String::new(), |mut source, index| {
        writeln!(source, "line-{index}   ").unwrap();
        source
    });
    fs::write(root.path().join("many.txt"), &source).unwrap();
    let mut document = String::from("*** Begin Patch\n*** Update File: many.txt\n");
    for index in 0..=MAXIMUM_PATCH_FUZZY_MATCHES {
        write!(document, "@@\n-line-{index}\n+changed-{index}\n").unwrap();
    }
    document.push_str("*** End Patch\n");

    let result = apply_patch(root.path(), &document);

    assert_eq!(result.status, PatchStatus::Rejected);
    assert!(result.effects.is_empty());
    assert_eq!(result.failure.unwrap().code, "response_budget");
    assert_eq!(
        fs::read_to_string(root.path().join("many.txt")).unwrap(),
        source
    );
}

#[test]
fn commit_rejects_a_replaced_parent_directory_even_with_identical_target_bytes() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("parent")).unwrap();
    fs::write(root.path().join("parent/file.txt"), "old\n").unwrap();
    let document = concat!(
        "*** Begin Patch\n",
        "*** Update File: parent/file.txt\n",
        "@@\n",
        "-old\n",
        "+new\n",
        "*** End Patch\n"
    );

    let result = apply_patch_before_commit(root.path(), document, |_| {
        fs::rename(
            root.path().join("parent"),
            root.path().join("original-parent"),
        )
        .unwrap();
        fs::create_dir(root.path().join("parent")).unwrap();
        fs::write(root.path().join("parent/file.txt"), "old\n").unwrap();
    });

    assert_eq!(result.status, PatchStatus::Rejected);
    assert_eq!(result.failure.unwrap().code, "changed_since_preflight");
    assert_eq!(
        fs::read(root.path().join("parent/file.txt")).unwrap(),
        b"old\n"
    );
    assert_eq!(
        fs::read(root.path().join("original-parent/file.txt")).unwrap(),
        b"old\n"
    );
}

#[test]
fn commit_does_not_recreate_a_parent_that_existed_during_preflight() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("parent")).unwrap();
    let document = concat!(
        "*** Begin Patch\n",
        "*** Add File: parent/file.txt\n",
        "+value\n",
        "*** End Patch\n"
    );

    let result = apply_patch_before_commit(root.path(), document, |_| {
        fs::remove_dir(root.path().join("parent")).unwrap();
    });

    assert_eq!(result.status, PatchStatus::Rejected);
    assert!(result.effects.is_empty());
    assert_eq!(result.failure.unwrap().code, "changed_since_preflight");
    assert!(!root.path().join("parent").exists());
}

#[test]
fn commit_rejects_a_replaced_target_even_with_identical_bytes_and_mode() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.txt"), "old\n").unwrap();
    let document = concat!(
        "*** Begin Patch\n",
        "*** Update File: file.txt\n",
        "@@\n",
        "-old\n",
        "+new\n",
        "*** End Patch\n"
    );

    let result = apply_patch_before_commit(root.path(), document, |_| {
        fs::rename(
            root.path().join("file.txt"),
            root.path().join("original.txt"),
        )
        .unwrap();
        fs::write(root.path().join("file.txt"), "old\n").unwrap();
    });

    assert_eq!(result.status, PatchStatus::Rejected);
    assert_eq!(result.failure.unwrap().code, "changed_since_preflight");
    assert_eq!(fs::read(root.path().join("file.txt")).unwrap(), b"old\n");
    assert_eq!(
        fs::read(root.path().join("original.txt")).unwrap(),
        b"old\n"
    );
}

#[test]
fn matcher_audits_rstrip_trim_and_eof_priority() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("rstrip.txt"), "alpha   \n").unwrap();
    let rstrip = patch(root.path(), "*** Update File: rstrip.txt\n@@\n-alpha\n+one");
    assert_eq!(rstrip.fuzzy_matches[0].kind, MatchKind::Rstrip);

    fs::write(root.path().join("trim.txt"), "  beta  \n").unwrap();
    let trim = patch(root.path(), "*** Update File: trim.txt\n@@\n-beta\n+two");
    assert_eq!(trim.fuzzy_matches[0].kind, MatchKind::Trim);

    fs::write(root.path().join("eof.txt"), "same\nmiddle\nsame\n").unwrap();
    let eof = patch(
        root.path(),
        "*** Update File: eof.txt\n@@\n-same\n+last\n*** End of File",
    );
    assert_eq!(eof.status, PatchStatus::Applied);
    assert_eq!(
        fs::read_to_string(root.path().join("eof.txt")).unwrap(),
        "same\nmiddle\nlast\n"
    );
}

#[test]
fn eof_marker_rejects_expected_lines_that_do_not_end_the_source() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("eof.txt"), "target\ntrailing\n").unwrap();

    let result = patch(
        root.path(),
        "*** Update File: eof.txt\n@@\n-target\n+changed\n*** End of File",
    );

    assert_eq!(result.status, PatchStatus::Rejected);
    assert_eq!(
        fs::read_to_string(root.path().join("eof.txt")).unwrap(),
        "target\ntrailing\n"
    );
}

#[test]
fn pure_addition_appends_and_deletion_only_can_produce_an_empty_file() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("append.txt"), "one\n").unwrap();
    let appended = patch(
        root.path(),
        "*** Update File: append.txt\n@@\n+two\n*** End of File",
    );
    assert_eq!(appended.status, PatchStatus::Applied);
    assert_eq!(
        fs::read(root.path().join("append.txt")).unwrap(),
        b"one\ntwo\n"
    );

    fs::write(root.path().join("empty.txt"), "remove\n").unwrap();
    let emptied = patch(root.path(), "*** Update File: empty.txt\n@@\n-remove");
    assert_eq!(emptied.status, PatchStatus::Applied);
    assert!(fs::read(root.path().join("empty.txt")).unwrap().is_empty());
}

#[test]
fn pure_addition_with_context_inserts_after_the_anchor() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("anchored.txt"), "one\nanchor\nthree\n").unwrap();

    let result = patch(
        root.path(),
        "*** Update File: anchored.txt\n@@ anchor\n+inserted",
    );

    assert_eq!(result.status, PatchStatus::Applied);
    assert_eq!(
        fs::read(root.path().join("anchored.txt")).unwrap(),
        b"one\nanchor\ninserted\nthree\n"
    );
}

#[test]
fn parser_preserves_a_lone_carriage_return_inside_added_content() {
    let root = tempfile::tempdir().unwrap();
    let result = apply_patch(
        root.path(),
        "*** Begin Patch\n*** Add File: carriage.txt\n+left\rright\n*** End Patch\n",
    );

    assert_eq!(result.status, PatchStatus::Applied);
    assert_eq!(
        fs::read(root.path().join("carriage.txt")).unwrap(),
        b"left\rright\n"
    );
}

#[test]
fn move_write_failure_identifies_the_destination_path() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("source.txt"), "old\n").unwrap();
    fs::create_dir(root.path().join("destination")).unwrap();
    let result = apply_patch_before_commit(
        root.path(),
        "*** Begin Patch\n*** Update File: source.txt\n*** Move to: destination/moved.txt\n@@\n-old\n+new\n*** End Patch\n",
        |_| {
            fs::rename(
                root.path().join("destination"),
                root.path().join("original-destination"),
            )
            .unwrap();
            fs::create_dir(root.path().join("destination")).unwrap();
        },
    );

    assert_eq!(result.status, PatchStatus::Rejected);
    assert_eq!(
        result.failure.unwrap().path.as_deref(),
        Some("destination/moved.txt")
    );
    assert_eq!(fs::read(root.path().join("source.txt")).unwrap(), b"old\n");
}

#[test]
fn overlapping_hunks_existing_add_non_utf8_and_special_targets_are_rejected_without_effects() {
    #[cfg(unix)]
    use std::os::unix::net::UnixListener;

    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("overlap.txt"), "one\n").unwrap();
    let overlap = patch(
        root.path(),
        "*** Update File: overlap.txt\n@@\n-one\n+first\n@@\n-one\n+second\n*** End of File",
    );
    assert_eq!(overlap.status, PatchStatus::Rejected);
    assert!(overlap.effects.is_empty());
    assert_eq!(fs::read(root.path().join("overlap.txt")).unwrap(), b"one\n");

    let existing = patch(root.path(), "*** Add File: overlap.txt\n+replacement");
    assert_eq!(existing.failure.unwrap().code, "already_exists");
    assert_eq!(fs::read(root.path().join("overlap.txt")).unwrap(), b"one\n");

    fs::write(root.path().join("bytes.bin"), [0xff, b'\n']).unwrap();
    let non_utf8 = patch(root.path(), "*** Update File: bytes.bin\n@@\n-old\n+new");
    assert_eq!(non_utf8.failure.unwrap().code, "non_utf8_file");

    #[cfg(unix)]
    {
        let _listener = UnixListener::bind(root.path().join("socket")).unwrap();
        let special = patch(root.path(), "*** Delete File: socket");
        assert_eq!(special.status, PatchStatus::Rejected);
        assert!(special.effects.is_empty());
    }
}

#[test]
fn operation_path_and_file_budgets_reject_before_mutation() {
    let root = tempfile::tempdir().unwrap();
    let mut operations = String::from("*** Begin Patch\n");
    for index in 0..=MAXIMUM_PATCH_OPERATIONS {
        writeln!(&mut operations, "*** Add File: file-{index}\n+x").unwrap();
    }
    operations.push_str("*** End Patch\n");
    let too_many = apply_patch(root.path(), &operations);
    assert_eq!(too_many.failure.unwrap().code, "too_many_operations");
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);

    let long_path = "x".repeat(MAXIMUM_PATCH_PATH_BYTES + 1);
    let path_result = patch(root.path(), &format!("*** Add File: {long_path}\n+x"));
    assert_eq!(path_result.failure.unwrap().code, "invalid_path");

    fs::write(
        root.path().join("large.txt"),
        vec![b'x'; MAXIMUM_PATCH_FILE_BYTES + 1],
    )
    .unwrap();
    let large = patch(root.path(), "*** Update File: large.txt\n@@\n-x\n+y");
    assert_eq!(large.failure.unwrap().code, "file_too_large");
    assert!(large.effects.is_empty());
}
