use super::*;
use std::{
    fs,
    io::{Read as _, Write as _},
    os::windows::{
        ffi::{OsStrExt as _, OsStringExt as _},
        fs::OpenOptionsExt as _,
    },
};

#[test]
fn private_archive_does_not_require_content_write_access() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("archive");
    let directory = create_absolute_directory_and_sync_no_follow(&path).unwrap();
    let namespace = directory.pin_namespace().unwrap();
    fs::write(path.join("source"), b"retained content").unwrap();
    // Like an executing image, this reader permits reads and deletion but denies
    // content writes. Changing a TokenUser-owned DACL needs neither write data
    // nor append data access.
    let mut reader = fs::OpenOptions::new()
        .read(true)
        .share_mode(1 | 4) // FILE_SHARE_READ | FILE_SHARE_DELETE
        .open(path.join("source"))
        .unwrap();
    assert!(verify_private_file(&reader).is_err());
    drop(set_relative_private_file_permissions_no_follow(&directory, Path::new("source")).unwrap());
    verify_private_file(&reader).unwrap();
    namespace
        .publish_child(
            Path::new("source"),
            Path::new("saved"),
            EntryKind::File,
            false,
        )
        .unwrap();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"retained content");
    assert!(!path.join("source").exists());
    assert_eq!(fs::read(path.join("saved")).unwrap(), bytes);
}

#[test]
fn concurrent_namespace_acquisition_shares_one_registered_pin() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = std::sync::Arc::new(
        create_absolute_directory_and_sync_no_follow(&temporary.path().join("private")).unwrap(),
    );
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let directory = directory.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let pin = directory.pin_namespace();
                barrier.wait();
                pin
            })
        })
        .collect();
    let pins: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap().unwrap())
        .collect();
    assert!(
        pins.iter()
            .all(|pin| std::sync::Arc::ptr_eq(&pins[0].0, &pin.0))
    );
}

#[test]
fn private_staging_links_and_cleanup_use_retained_namespaces() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("cas");
    let cas = create_absolute_directory_and_sync_no_follow(&path).unwrap();
    let stage_path = path.join("staging");
    let staging = create_absolute_directory_and_sync_no_follow(&stage_path).unwrap();
    let destination = cas.pin_namespace().unwrap();
    let source = staging.pin_namespace().unwrap();
    let mut file = create_relative_private_file_no_follow(&staging, Path::new("temp")).unwrap();
    file.write_all(b"body").unwrap();
    file.sync_all().unwrap();
    drop(file);
    assert!(fs::rename(&path, temporary.path().join("moved")).is_err());
    destination
        .link_child(&staging, Path::new("temp"), Path::new("digest"))
        .unwrap();
    assert_eq!(
        destination
            .link_child(&staging, Path::new("temp"), Path::new("digest"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    source.remove_child(Path::new("temp")).unwrap();
    assert!(!stage_path.join("temp").exists());
    assert_eq!(fs::read(path.join("digest")).unwrap(), b"body");
    assert!(source.remove_child(Path::new("../digest")).is_err());
    source.sync_all().unwrap();
    destination.sync_all().unwrap();
}

#[test]
fn private_file_creation_and_directory_sync_use_retained_authority() {
    let temporary = tempfile::tempdir().unwrap();
    let private_path = temporary.path().join("PrivateOutput");
    let directory = create_absolute_directory_no_follow(&private_path).unwrap();
    let mut file =
        create_relative_private_file_no_follow(&directory, Path::new("body.bin")).unwrap();
    verify_private_file(&file).unwrap();
    let before = file_version(&file).unwrap();
    file.write_all(b"first body").unwrap();
    file.sync_all().unwrap();
    let after = file_version(&file).unwrap();
    assert_eq!(before.identity, after.identity);
    assert_eq!(after.length, 10);
    assert_ne!(before, after);
    drop(file);
    directory.sync_all().unwrap();
    let mut reopened =
        open_relative_private_file_no_follow(&directory, Path::new("body.bin")).unwrap();
    let mut body = String::new();
    reopened.read_to_string(&mut body).unwrap();
    assert_eq!(body, "first body");
    assert_eq!(
        create_relative_private_file_no_follow(&directory, Path::new("body.bin"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
}

#[test]
fn namespace_pin_guards_root_reparse_and_rename_while_allowing_child_publication() {
    let temporary = tempfile::tempdir().unwrap();
    let selected = temporary.path().join("PinnedNamespace");
    let directory = create_absolute_directory_and_sync_no_follow(&selected).unwrap();
    let lease = directory.pin_namespace().unwrap();
    let second = open_absolute_directory_no_follow(&selected)
        .unwrap()
        .pin_namespace()
        .unwrap();
    lease.sync_all().unwrap();
    assert!(fs::rename(&selected, temporary.path().join("MovedNamespace")).is_err());
    let parent = open_absolute_directory_no_follow(temporary.path()).unwrap();
    let writable = native::writable_relative(
        &parent.0,
        &"PinnedNamespace".encode_utf16().collect::<Vec<_>>(),
        true,
    )
    .unwrap();
    junction(&temporary.path().join("control"), temporary.path());
    let target: Vec<_> = temporary.path().as_os_str().encode_wide().collect();
    assert!(native::set_junction(&writable, &target).is_err());
    assert!(fs::remove_file(selected.join(".rsi-namespace-pin")).is_err());
    drop(writable);
    // Child content remains usable; the lease owns only the directory namespace.
    let mut file =
        create_relative_private_file_no_follow(&directory, Path::new("body.bin")).unwrap();
    file.write_all(b"body").unwrap();
    file.sync_all().unwrap();
    lease.sync_all().unwrap();
    drop(file);
    drop(lease);
    assert!(fs::rename(&selected, temporary.path().join("MovedNamespace")).is_err());
    drop(second);
    fs::rename(&selected, temporary.path().join("MovedNamespace")).unwrap();
}

#[test]
fn retained_namespace_publishes_files_and_directories_with_explicit_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let root_path = temp.path().join("private");
    let root = create_absolute_directory_and_sync_no_follow(&root_path).unwrap();
    let namespace = root.pin_namespace().unwrap();
    for (name, bytes) in [("one", b"first".as_slice()), ("two", b"second".as_slice())] {
        let mut file = create_relative_private_file_no_follow(&root, Path::new(name)).unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
    }
    namespace
        .publish_child(Path::new("one"), Path::new("final"), EntryKind::File, false)
        .unwrap();
    assert!(
        namespace
            .publish_child(Path::new("two"), Path::new("final"), EntryKind::File, false)
            .is_err()
    );
    assert_eq!(fs::read(root_path.join("final")).unwrap(), b"first");
    let guarded = open_relative_private_file_no_follow(&root, Path::new("final")).unwrap();
    assert!(
        namespace
            .publish_child(Path::new("two"), Path::new("final"), EntryKind::File, true)
            .is_err()
    );
    drop(guarded);
    let mut reader = open_relative_file_no_follow(&root, Path::new("final")).unwrap();
    namespace
        .publish_child(Path::new("two"), Path::new("final"), EntryKind::File, true)
        .unwrap();
    let mut original = Vec::new();
    reader.read_to_end(&mut original).unwrap();
    assert_eq!(original, b"first");
    assert_eq!(fs::read(root_path.join("final")).unwrap(), b"second");
    let stage = create_absolute_directory_and_sync_no_follow(&root_path.join("stage")).unwrap();
    let mut file = create_relative_private_file_no_follow(&stage, Path::new("body")).unwrap();
    file.write_all(b"directory body").unwrap();
    file.sync_all().unwrap();
    drop(file);
    namespace
        .publish_child(
            Path::new("stage"),
            Path::new("published"),
            EntryKind::Directory,
            false,
        )
        .unwrap();
    namespace.sync_all().unwrap();
    assert_eq!(
        fs::read(root_path.join("published/body")).unwrap(),
        b"directory body"
    );
    assert!(
        namespace
            .publish_child(
                Path::new("../final"),
                Path::new("escape"),
                EntryKind::File,
                true
            )
            .is_err()
    );
}

#[test]
fn privatization_changes_only_the_selected_owned_directory() {
    let temporary = tempfile::tempdir().unwrap();
    let selected = temporary.path().join("ExistingOwnedDirectory");
    // Elevated runners may default to an Administrators owner. This fixture
    // explicitly owns the selected directory as TokenUser.
    drop(create_absolute_directory_no_follow(&selected).unwrap());
    let directory = set_private_directory_permissions_no_follow(&selected).unwrap();
    let policy = native::private_descriptor(&directory.0).unwrap();
    assert!(policy.contains("D:P"));
    assert_eq!(
        native::private_descriptor(
            &set_private_directory_permissions_no_follow(&selected)
                .unwrap()
                .0
        )
        .unwrap(),
        policy
    );
    let retained = open_absolute_directory_no_follow(temporary.path()).unwrap();
    let child = set_relative_private_directory_permissions_no_follow(
        &retained,
        Path::new("ExistingOwnedDirectory"),
    )
    .unwrap();
    assert_eq!(native::private_descriptor(&child.0).unwrap(), policy);
    assert!(set_private_directory_permissions_no_follow(Path::new(r"C:\")).is_err());
    fs::write(temporary.path().join("unprotected.txt"), "body").unwrap();
    let parent = open_absolute_directory_no_follow(temporary.path()).unwrap();
    assert!(open_relative_private_file_no_follow(&parent, Path::new("unprotected.txt")).is_err());
}

#[test]
fn exact_names_and_native_utf16_survive_relative_reads_and_enumeration() {
    let temporary = tempfile::tempdir().unwrap();
    let base = temporary.path();
    fs::write(base.join("plain.txt"), "plain file").unwrap();
    fs::create_dir(base.join("SubdirectoryLongName")).unwrap();
    let chinese = "Case中文😀.txt";
    fs::write(base.join("SubdirectoryLongName").join(chinese), "original").unwrap();
    let surrogate = OsString::from_wide(&[0xd800, 46, 116, 120, 116]);
    fs::write(base.join(&surrogate), "surrogate").unwrap();
    let root = open_absolute_directory_no_follow(base).unwrap();
    let mut file =
        open_relative_file_no_follow(&root, Path::new(&format!("SubdirectoryLongName/{chinese}")))
            .unwrap();
    let mut text = String::new();
    file.read_to_string(&mut text).unwrap();
    assert_eq!(text, "original");
    assert!(file.write_all(b"changed").is_err());
    let error =
        open_relative_file_no_follow(&root, Path::new("SubdirectoryLongName/case中文😀.txt"))
            .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    let entries: Vec<_> = root
        .entries()
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name(), entry.file_type().unwrap())
        })
        .collect();
    assert!(entries.contains(&(surrogate.clone(), EntryKind::File)));
    assert!(entries.contains(&(OsString::from("plain.txt"), EntryKind::File)));
    assert!(entries.contains(&(OsString::from("SubdirectoryLongName"), EntryKind::Directory)));
    text.clear();
    open_relative_file_no_follow(&root, Path::new(&surrogate))
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    assert_eq!(text, "surrogate");
}

#[test]
fn enumeration_reuses_its_buffer_without_retaining_previous_record_bytes() {
    let temporary = tempfile::tempdir().unwrap();
    let mut expected = Vec::new();
    for index in 0..128 {
        let name = format!("{index:03}-{}", "中".repeat(index % 80));
        fs::write(temporary.path().join(&name), []).unwrap();
        expected.push(OsString::from(name));
    }
    let root = open_absolute_directory_no_follow(temporary.path()).unwrap();
    let mut observed = root
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    expected.sort();
    observed.sort();
    assert_eq!(observed, expected);
}

#[test]
fn large_directory_enumeration_preserves_names_with_bounded_native_queries() {
    let temporary = tempfile::tempdir().unwrap();
    let mut expected = Vec::new();
    for index in 0..1536 {
        let name = format!("{index:04}-{}", "中".repeat(index % 180));
        fs::write(temporary.path().join(&name), []).unwrap();
        expected.push(OsString::from(name));
    }
    let root = open_absolute_directory_no_follow(temporary.path()).unwrap();
    native::ENUMERATION_QUERIES.with(|count| count.set(0));
    let mut observed = root
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    let queries = native::ENUMERATION_QUERIES.with(std::cell::Cell::get);
    expected.sort();
    observed.sort();
    assert_eq!(observed, expected);
    println!("1536 entries, {queries} native enumeration queries");
    assert!(
        queries <= 16,
        "bounded batches should not query once per entry: {queries}"
    );
}

#[test]
fn long_paths_use_native_component_opens_without_absolute_reopening() {
    let temporary = tempfile::tempdir().unwrap();
    let mut relative = PathBuf::new();
    for index in 0..8 {
        relative.push(format!("component-{index}-{}", "x".repeat(40)));
    }
    fs::create_dir_all(temporary.path().join(&relative)).unwrap();
    relative.push("file中文😀.txt");
    fs::write(temporary.path().join(&relative), "long path").unwrap();
    assert!(
        temporary
            .path()
            .join(&relative)
            .as_os_str()
            .encode_wide()
            .count()
            > 260
    );
    let root = open_absolute_directory_no_follow(temporary.path()).unwrap();
    let mut text = String::new();
    open_relative_file_no_follow(&root, &relative)
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    assert_eq!(text, "long path");
}

#[test]
fn enumeration_retains_original_identity_and_independent_cursors_after_root_replacement() {
    let temporary = tempfile::tempdir().unwrap();
    let base = temporary.path();
    fs::create_dir(base.join("root")).unwrap();
    for name in ["first", "second", "third"] {
        fs::write(base.join("root").join(name), name).unwrap();
    }
    let root = open_absolute_directory_no_follow(&base.join("root")).unwrap();
    let mut first = root.entries().unwrap();
    fs::rename(base.join("root"), base.join("moved")).unwrap();
    fs::create_dir(base.join("root")).unwrap();
    fs::write(base.join("root/replacement"), "outside").unwrap();
    let mut second = root.entries().unwrap();
    let a = first.next().unwrap().unwrap().file_name();
    let b = first.next().unwrap().unwrap().file_name();
    assert_eq!(second.next().unwrap().unwrap().file_name(), a);
    drop(root);
    let mut names = vec![a, b];
    names.extend(first.map(|entry| entry.unwrap().file_name()));
    names.sort();
    assert_eq!(names, ["first", "second", "third"].map(OsString::from));
}

#[test]
fn file_handle_retains_original_content_after_rename_and_same_name_replacement() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("file"), "original").unwrap();
    let root = open_absolute_directory_no_follow(temporary.path()).unwrap();
    let mut file = open_relative_file_no_follow(&root, Path::new("file")).unwrap();
    fs::rename(
        temporary.path().join("file"),
        temporary.path().join("moved"),
    )
    .unwrap();
    fs::write(temporary.path().join("file"), "replacement").unwrap();
    let mut text = String::new();
    file.read_to_string(&mut text).unwrap();
    assert_eq!(text, "original");
}

#[test]
fn junctions_are_classified_and_refused_at_every_open_boundary() {
    let temporary = tempfile::tempdir().unwrap();
    let base = temporary.path();
    fs::create_dir(base.join("outside")).unwrap();
    fs::write(base.join("outside/private"), "outside").unwrap();
    junction(&base.join("link"), &base.join("outside"));
    let root = open_absolute_directory_no_follow(base).unwrap();
    let error = open_relative_directory_no_follow(&root, Path::new("link")).unwrap_err();
    assert!(is_link_rejection(&error), "{error}");
    assert!(open_relative_file_no_follow(&root, Path::new("link/private")).is_err());
    assert!(open_absolute_directory_no_follow(&base.join("link")).is_err());
    assert!(root.entries().unwrap().any(|entry| {
        let entry = entry.unwrap();
        entry.file_name() == "link" && entry.file_type().unwrap().is_symlink()
    }));
    assert_eq!(
        fs::read_to_string(base.join("outside/private")).unwrap(),
        "outside"
    );
}

fn junction(path: &Path, target: &Path) {
    fs::create_dir(path).unwrap();
    let fixture = fs::OpenOptions::new()
        .access_mode(0x4000_0000)
        .custom_flags(0x0220_0000)
        .open(path)
        .unwrap();
    let target: Vec<_> = target.as_os_str().encode_wide().collect();
    native::set_junction(&fixture, &target).unwrap();
}

#[test]
fn creation_collision_acquires_only_an_existing_no_follow_directory() {
    let temporary = tempfile::tempdir().unwrap();
    let base = temporary.path();
    let root = open_absolute_directory_no_follow(base).unwrap();
    let name = "existing".encode_utf16().collect::<Vec<_>>();
    assert_eq!(
        native::relative(&root.0, &name, true).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    fs::create_dir(base.join("existing")).unwrap();
    let existing = open_relative_directory_no_follow(&root, Path::new("existing")).unwrap();
    assert!(matches!(native::create_directory(&root.0, &name),
        Err(DirectoryCreationError::NotCreated(error)) if error.kind() == io::ErrorKind::AlreadyExists));
    let (acquired, created) = create_or_open_directory(&root, &name).unwrap();
    assert!(
        !created,
        "a creation collision has no new namespace receipt"
    );
    assert_eq!(
        native::identity(&acquired).unwrap(),
        native::identity(&existing.0).unwrap()
    );

    fs::create_dir(base.join("outside")).unwrap();
    junction(&base.join("link"), &base.join("outside"));
    let error =
        create_or_open_directory(&root, &"link".encode_utf16().collect::<Vec<_>>()).unwrap_err();
    assert!(is_link_rejection(&error), "{error}");
    assert!(create_absolute_directory_no_follow(&base.join("link/child")).is_err());
    assert_eq!(fs::read_dir(base.join("outside")).unwrap().count(), 0);

    fs::write(base.join("file"), "retained").unwrap();
    assert!(create_or_open_directory(&root, &"file".encode_utf16().collect::<Vec<_>>()).is_err());
    assert_eq!(fs::read_to_string(base.join("file")).unwrap(), "retained");
}

#[test]
fn link_rejection_does_not_misclassify_other_io_errors() {
    for code in [
        native::ERROR_REPARSE_POINT_ENCOUNTERED,
        native::ERROR_DIRECTORY,
    ] {
        assert!(is_link_rejection(&io::Error::from_raw_os_error(code)));
    }
    for code in [1, 2, 3, 5, 32, 80, 183] {
        assert!(
            !is_link_rejection(&io::Error::from_raw_os_error(code)),
            "{code}"
        );
    }
    assert!(!is_link_rejection(&io::Error::new(
        io::ErrorKind::InvalidInput,
        "not native"
    )));
}

#[test]
fn unicode_spaces_remain_distinct_native_names_instead_of_dos_devices() {
    let temporary = tempfile::tempdir().unwrap();
    let root = open_absolute_directory_no_follow(temporary.path()).unwrap();
    for name in ["CON\u{00a0}", "NUL\u{3000}"] {
        let directory = create_directory_no_follow(&root, name.as_ref()).unwrap();
        assert!(directory.dir_metadata().unwrap().is_dir());
        let path = temporary.path().join(name);
        assert!(fs::metadata(&path).unwrap().is_dir());
        fs::write(path.join("payload"), name).unwrap();
        let mut text = String::new();
        open_relative_file_no_follow(&root, &Path::new(name).join("payload"))
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, name);
    }
    assert_eq!(root.entries().unwrap().count(), 2);
}

#[test]
fn invalid_paths_fail_before_directory_creation() {
    let temporary = tempfile::tempdir().unwrap();
    let root = open_absolute_directory_no_follow(temporary.path()).unwrap();
    for path in [
        "..",
        "sub/../file",
        "/absolute",
        "C:/absolute",
        "file:stream",
        "NUL.txt",
        "CON .txt",
        "trailing.",
        "space ",
        "nul\0suffix",
        "a//b",
    ] {
        assert_eq!(
            open_relative_file_no_follow(&root, Path::new(path))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput,
            "{path:?}"
        );
    }
    for path in [
        "c:/",
        "C:relative",
        "//server/share",
        "//?/C:/",
        "C:/new/../escape",
    ] {
        assert_eq!(
            open_absolute_directory_no_follow(Path::new(path))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput,
            "{path}"
        );
    }
    for name in ["", "a/b", "a\\b", "..", "nul\0suffix", "NUL.txt"] {
        assert!(matches!(create_directory_no_follow(&root, name.as_ref()),
            Err(DirectoryCreationError::NotCreated(error)) if error.kind() == io::ErrorKind::InvalidInput));
    }
    assert_eq!(
        create_absolute_directory_no_follow(&temporary.path().join("new/../escape"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 0);
}

#[test]
fn creation_sets_private_owner_and_protected_dacl_without_repairing_existing_objects() {
    let temporary = tempfile::tempdir().unwrap();
    let root = open_absolute_directory_no_follow(temporary.path()).unwrap();
    let created = create_directory_no_follow(&root, "private中文😀".as_ref()).unwrap();
    let before = native::private_descriptor(&created.0).unwrap();
    let (owner, rules) = before.split_once("D:P").unwrap();
    let user = owner.strip_prefix("O:").unwrap();
    assert!(rules.contains("(A;OICI;FA;;;SY)"));
    assert!(rules.contains("(A;OICI;FA;;;BA)"));
    assert!(rules.contains(&format!("(A;OICI;FA;;;{user})")));
    assert_eq!(rules.matches("(A;").count(), 3);
    assert!(
        matches!(create_directory_no_follow(&root, "private中文😀".as_ref()),
        Err(DirectoryCreationError::NotCreated(error)) if error.kind() == io::ErrorKind::AlreadyExists)
    );
    let path = temporary.path().join("private中文😀/nested");
    let nested = create_absolute_directory_no_follow(&path).unwrap();
    // The parent grants inheritable ACEs, but the new protected child receives
    // only its own three explicit ACEs, with the same exact owner/DACL policy.
    assert_eq!(native::private_descriptor(&nested.0).unwrap(), before);
    assert_eq!(native::private_descriptor(&created.0).unwrap(), before);
    assert!(
        created
            .entries()
            .unwrap()
            .any(|entry| entry.unwrap().file_name() == "nested")
    );
}

#[test]
fn directory_creation_bounds_utf16_without_creating_a_truncated_name() {
    let temporary = tempfile::tempdir().unwrap();
    let root = open_absolute_directory_no_follow(temporary.path()).unwrap();
    let maximum = "a".repeat(255);
    create_directory_no_follow(&root, maximum.as_ref()).unwrap();
    for name in [
        "a".repeat(256),
        format!("{maximum}suffix"),
        "😀".repeat(128),
    ] {
        assert!(matches!(create_directory_no_follow(&root, name.as_ref()),
            Err(DirectoryCreationError::NotCreated(error)) if error.kind() == io::ErrorKind::InvalidInput));
    }
    let entries = root
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(entries, [std::ffi::OsString::from(maximum)]);
}
