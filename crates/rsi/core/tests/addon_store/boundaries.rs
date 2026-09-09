use super::{NativeAddonStore, fs, source};
use rsi::{
    MAXIMUM_NATIVE_ADDON_MANIFEST_BYTES, MAXIMUM_NATIVE_ADDON_STATE_BYTES, NativeAddonError,
};
use std::os::unix::fs::{PermissionsExt as _, symlink};

#[test]
fn writer_lock_is_shared_by_independent_store_handles_and_releases_on_drop() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let manifest = source(&root.join("source"), b"artifact");
    let first = NativeAddonStore::open(root.join("store")).unwrap();
    let second = NativeAddonStore::open(root.join("store")).unwrap();
    let lock = fs::File::open(root.join("store")).unwrap();
    lock.try_lock().unwrap();
    assert!(matches!(
        first.install(&manifest),
        Err(NativeAddonError::Busy)
    ));
    assert!(matches!(
        second.disable("fixture.addon"),
        Err(NativeAddonError::Busy)
    ));
    assert!(first.snapshot().unwrap().installed.is_empty());
    drop(lock);
    first.install(&manifest).unwrap();
    second.enable("fixture.addon").unwrap();
    assert_eq!(first.snapshot().unwrap(), second.snapshot().unwrap());
}

#[test]
fn abandoned_index_and_object_stages_are_reclaimed_but_unmanaged_files_are_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let manifest = source(&root.join("source"), b"artifact");
    let store = NativeAddonStore::open(root.join("store")).unwrap();
    let name = ".rsi-addon-0123456789abcdef0123456789abcdef.tmp";
    let index_stage = root.join("store").join(name);
    let object_stage = root.join("store/objects").join(name);
    fs::write(&index_stage, b"abandoned").unwrap();
    fs::write(&object_stage, b"abandoned").unwrap();
    store.install(&manifest).unwrap();
    assert!(!index_stage.exists());
    assert!(!object_stage.exists());
    let unmanaged = root.join("store/keep.txt");
    fs::write(&unmanaged, b"keep").unwrap();
    assert!(store.enable("fixture.addon").is_err());
    assert_eq!(fs::read(&unmanaged).unwrap(), b"keep");
    assert!(store.snapshot().unwrap().enabled.is_empty());
}

#[test]
fn oversized_or_linked_abandoned_staging_is_rejected_without_unlinking() {
    for object in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let manifest = source(&root.join("source"), b"artifact");
        let store = NativeAddonStore::open(root.join("store")).unwrap();
        let directory = root.join(if object { "store/objects" } else { "store" });
        let stage = directory.join(".rsi-addon-0123456789abcdef0123456789abcdef.tmp");
        let maximum = if object {
            rsi_meta_native_loader::MAX_ARTIFACT_BYTES
        } else {
            MAXIMUM_NATIVE_ADDON_STATE_BYTES as u64
        };
        fs::File::create(&stage)
            .unwrap()
            .set_len(maximum + 1)
            .unwrap();
        assert!(store.install(&manifest).is_err());
        assert!(stage.exists());
        fs::remove_file(&stage).unwrap();
        symlink(root.join("source/artifact.bin"), &stage).unwrap();
        assert!(store.install(&manifest).is_err());
        assert!(stage.is_symlink());
        assert!(store.snapshot().unwrap().installed.is_empty());
    }
}

#[test]
fn retained_root_and_object_handles_never_redirect_writes_after_replacement() {
    for object in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let manifest = source(&root.join("source"), b"artifact");
        let store = NativeAddonStore::open(root.join("store")).unwrap();
        store.install(&manifest).unwrap();
        let replaced = root.join(if object { "store/objects" } else { "store" });
        fs::rename(&replaced, root.join("retained")).unwrap();
        fs::create_dir(&replaced).unwrap();
        assert!(matches!(store.snapshot(), Err(NativeAddonError::Conflict)));
        assert!(store.install(&manifest).is_err());
        assert!(store.enable("fixture.addon").is_err());
        assert_eq!(fs::read_dir(replaced).unwrap().count(), 0);
    }
}

#[test]
fn source_and_index_reject_links_fifos_directories_and_group_writes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let manifest = source(&root.join("source"), b"artifact");
    let store = NativeAddonStore::open(root.join("store")).unwrap();
    let artifact = root.join("source/artifact.bin");
    fs::rename(&artifact, root.join("source/real.bin")).unwrap();
    for path in [&artifact, &root.join("store/state.json")] {
        symlink(root.join("source/real.bin"), path).unwrap();
        assert!(store.install(&manifest).is_err());
        fs::remove_file(path).unwrap();
        fifo(path);
        assert!(store.install(&manifest).is_err());
        fs::remove_file(path).unwrap();
        fs::create_dir(path).unwrap();
        assert!(store.install(&manifest).is_err());
        fs::remove_dir(path).unwrap();
        if path == &artifact {
            fs::write(&artifact, b"artifact").unwrap();
        }
    }
    store.install(&manifest).unwrap();
    for path in [
        root.join("store"),
        root.join("store/objects"),
        root.join("store/state.json"),
    ] {
        let original = fs::metadata(&path).unwrap().permissions();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o770)).unwrap();
        assert!(store.snapshot().is_err());
        assert!(store.enable("fixture.addon").is_err());
        fs::set_permissions(path, original).unwrap();
    }
    symlink(root.join("source"), root.join("linked-source")).unwrap();
    assert!(
        store
            .install(&root.join("linked-source/addon.toml"))
            .is_err()
    );
}

#[test]
fn manifest_is_fully_validated_before_artifact_access_or_build_execution() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let manifest = source(&root.join("source"), b"artifact");
    let valid = fs::read_to_string(&manifest).unwrap();
    let store = NativeAddonStore::open(root.join("store")).unwrap();
    fs::remove_file(root.join("source/artifact.bin")).unwrap();
    let invalid = [
        valid.replace("format = 1", "format = 2"),
        valid.replace("artifact.bin", "../artifact.bin"),
        valid.replace("fixture.addon", &"a".repeat(65)),
        valid.replace("['fixture.native.tools']", "['duplicate', 'duplicate']"),
        format!("{valid}unknown = true\n"),
        format!("{valid}[build]\ncommand = []\n"),
        format!("{valid}[build]\ncommand = ['true']\ntimeout_seconds = 601\n"),
        format!("{valid}[build]\ncommand = ['true']\nwatch = ['../escape']\n"),
    ];
    for text in invalid {
        fs::write(&manifest, text).unwrap();
        assert!(matches!(
            store.install(&manifest),
            Err(NativeAddonError::Invalid(_))
        ));
    }
    fs::write(&manifest, [0xff]).unwrap();
    assert!(matches!(
        store.install(&manifest),
        Err(NativeAddonError::Invalid(_))
    ));
    fs::write(
        &manifest,
        vec![b' '; MAXIMUM_NATIVE_ADDON_MANIFEST_BYTES + 1],
    )
    .unwrap();
    assert!(matches!(
        store.install(&manifest),
        Err(NativeAddonError::Capacity(_))
    ));
    fs::write(root.join("source/artifact.bin"), b"artifact").unwrap();
    let marker = root.join("BUILD-MUST-NOT-RUN");
    fs::write(
        &manifest,
        format!(
            "{valid}[build]\ncommand = ['touch', '{}']\n",
            marker.display()
        ),
    )
    .unwrap();
    store.install(&manifest).unwrap();
    assert!(!marker.exists());
    let inspection = serde_json::to_string(&store.snapshot().unwrap()).unwrap();
    assert!(!inspection.contains("BUILD-MUST-NOT-RUN"));
    assert!(!inspection.contains(root.to_str().unwrap()));
}

#[test]
fn manifest_and_service_exact_bounds_accept_then_reject_one_more() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let manifest = source(&root.join("source"), b"artifact");
    let valid = fs::read_to_string(&manifest).unwrap();
    let store = NativeAddonStore::open(root.join("store")).unwrap();
    let keys: Vec<_> = (0..64).map(|i| format!("'fixture.{i}'")).collect();
    let text = valid.replace("'fixture.native.tools'", &keys.join(","));
    let exact = format!(
        "{text}#{}",
        " ".repeat(MAXIMUM_NATIVE_ADDON_MANIFEST_BYTES - text.len() - 1)
    );
    fs::write(&manifest, &exact).unwrap();
    assert_eq!(
        store
            .install(&manifest)
            .unwrap()
            .record
            .unwrap()
            .portable_services()
            .len(),
        64
    );
    fs::write(&manifest, format!("{exact} ")).unwrap();
    assert!(matches!(
        store.install(&manifest),
        Err(NativeAddonError::Capacity(_))
    ));
    fs::write(
        &manifest,
        text.replace("'fixture.63'", "'fixture.63','extra'"),
    )
    .unwrap();
    assert!(matches!(
        store.install(&manifest),
        Err(NativeAddonError::Capacity(_))
    ));
    fs::write(&manifest, valid).unwrap();
    fs::File::create(root.join("source/artifact.bin"))
        .unwrap()
        .set_len(rsi_meta_native_loader::MAX_ARTIFACT_BYTES + 1)
        .unwrap();
    assert!(matches!(
        store.install(&manifest),
        Err(NativeAddonError::Capacity(_))
    ));
}

// Starting an external mkfifo can inherit another test's flock between fork and
// exec, delaying release after that test drops its last parent descriptor.
#[expect(
    unsafe_code,
    reason = "Unix FIFO fixture creation without forking the test process"
)]
fn fifo(path: &std::path::Path) {
    use std::os::unix::ffi::OsStrExt as _;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the CString supplies a live, NUL-terminated path throughout this
    // synchronous call. mkfifo retains no pointer; mode is a valid permission
    // bitmask. Callers use paths inside their own isolated temporary directories.
    let result = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
    assert_eq!(result, 0, "mkfifo: {}", std::io::Error::last_os_error());
}
