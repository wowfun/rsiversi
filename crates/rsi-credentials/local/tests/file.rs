#![cfg(unix)]

use rsi_credentials_local::{FileSecretStore, SecretStore};
use rsi_credentials_protocol::{
    CredentialRef, CredentialStoreFailure as Failure, CredentialsError, SecretValue,
};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::Path,
};

fn temp_root() -> tempfile::TempDir {
    tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap()
}

#[expect(unsafe_code, reason = "isolated Unix FIFO fixture without forking")]
fn fifo(path: &Path) {
    use std::os::unix::ffi::OsStrExt as _;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the live CString is NUL-terminated, the mode is valid, and mkfifo
    // retains no pointer. Every caller owns an isolated temporary directory.
    let result = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
    assert_eq!(result, 0, "mkfifo: {}", std::io::Error::last_os_error());
}

fn reference(slot: &str) -> CredentialRef {
    CredentialRef::new("fixture.provider", slot).unwrap()
}
fn key(value: &str) -> SecretValue {
    SecretValue::new(value).unwrap()
}
fn private_file(path: &Path, contents: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::set_permissions(path.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn saved_credentials_survive_reopen_and_isolate_full_references() {
    let root = temp_root();
    let path = root.path().join("credentials/credentials.json");
    let store = FileSecretStore::new(&path);
    assert!(store.get(&reference("primary")).unwrap().is_none());
    assert!(
        !path.parent().unwrap().exists(),
        "read must not create state"
    );
    store.set(&reference("primary"), &key("first")).unwrap();
    let other = CredentialRef::new("another.provider", "primary").unwrap();
    store.set(&other, &key("other")).unwrap();
    let reopened = FileSecretStore::new(&path);
    assert_eq!(
        reopened
            .get(&reference("primary"))
            .unwrap()
            .unwrap()
            .expose_secret(),
        "first"
    );
    reopened
        .set(&reference("primary"), &key("replacement"))
        .unwrap();
    assert_eq!(
        store
            .get(&reference("primary"))
            .unwrap()
            .unwrap()
            .expose_secret(),
        "replacement"
    );
    assert!(store.unset(&reference("primary")).unwrap());
    assert!(!store.unset(&reference("primary")).unwrap());
    assert_eq!(
        reopened.get(&other).unwrap().unwrap().expose_secret(),
        "other"
    );
    assert_eq!(
        fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o7777,
        0o700
    );
    for entry in fs::read_dir(path.parent().unwrap()).unwrap() {
        let metadata = entry.unwrap().metadata().unwrap();
        assert_eq!(metadata.mode() & 0o7777, 0o600);
        assert_eq!(metadata.nlink(), 1);
    }
}

#[test]
fn invalid_documents_are_never_overwritten_or_echoed() {
    let root = temp_root();
    let path = root.path().join("credentials/credentials.json");
    let store = FileSecretStore::new(&path);
    for contents in [
        r#"{"secret-marker":"unterminated"#,
        r#"{"version":2,"entries":[]}"#,
        r#"{"version":1,"version":1,"entries":[]}"#,
        r#"{"version":1,"entries":[],"unexpected":"secret-marker"}"#,
        r#"{"version":1,"entries":[{"reference":{"owner":"fixture.provider","slot":"a"},"secret":"secret-marker"},{"reference":{"owner":"fixture.provider","slot":"a"},"secret":"different"}]}"#,
        r#"{"version":1,"entries":[{"reference":{"owner":"invalid/owner","slot":"a"},"secret":"secret-marker"}]}"#,
        r#"{"version":1,"entries":[{"reference":{"owner":"fixture.provider","slot":"a"},"secret":""}]}"#,
        r#"{"version":1,"entries":[{"reference":{"owner":"fixture.provider","slot":"a"},"secret":"secret-marker","secret":"another"}]}"#,
    ] {
        private_file(&path, contents.as_bytes());
        let error = store.get(&reference("a")).unwrap_err();
        assert_eq!(error, CredentialsError::Store(Failure::Corrupt));
        assert!(!format!("{error:?} {error}").contains("secret-marker"));
        assert_eq!(
            store.set(&reference("a"), &key("replacement")),
            Err(CredentialsError::Store(Failure::Corrupt))
        );
        assert_eq!(fs::read(&path).unwrap(), contents.as_bytes());
    }
}

#[test]
fn oversized_documents_and_record_counts_fail_explicitly() {
    let root = temp_root();
    let path = root.path().join("credentials/credentials.json");
    let store = FileSecretStore::new(&path);
    private_file(&path, &vec![b' '; 4 * 1024 * 1024 + 1]);
    assert_eq!(
        store.get(&reference("a")).unwrap_err(),
        CredentialsError::Store(Failure::TooLarge)
    );
    let entries: Vec<_> = (0..4097).map(|n| serde_json::json!({"reference":{"owner":"fixture.provider","slot":n.to_string()},"secret":"fixture"})).collect();
    private_file(
        &path,
        &serde_json::to_vec(&serde_json::json!({"version":1,"entries":entries})).unwrap(),
    );
    assert_eq!(
        store.get(&reference("a")).unwrap_err(),
        CredentialsError::Store(Failure::TooLarge)
    );
}

#[test]
fn writes_exceeding_document_or_record_limits_preserve_the_previous_file() {
    let root = temp_root();
    let path = root.path().join("credentials/credentials.json");
    let store = FileSecretStore::new(&path);
    for (count, value) in [(4096, "fixture".into()), (63, "s".repeat(64 * 1024))] {
        let entries: Vec<_> = (0..count).map(|n| serde_json::json!({"reference":{"owner":"fixture.provider","slot":n.to_string()},"secret":value})).collect();
        let original =
            serde_json::to_vec(&serde_json::json!({"version":1,"entries":entries})).unwrap();
        private_file(&path, &original);
        assert_eq!(
            store.set(&reference("new"), &key(&value)),
            Err(CredentialsError::Store(Failure::TooLarge))
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(
            store.get(&reference("0")).unwrap().unwrap().expose_secret(),
            value
        );
    }
}

#[test]
fn permissive_files_and_directories_are_rejected_without_repair() {
    let root = temp_root();
    let path = root.path().join("credentials/credentials.json");
    let store = FileSecretStore::new(&path);
    store.set(&reference("a"), &key("fixture")).unwrap();
    for target in [&path, path.parent().unwrap()] {
        let original = fs::metadata(target).unwrap().permissions();
        fs::set_permissions(
            target,
            fs::Permissions::from_mode(if target == path { 0o644 } else { 0o755 }),
        )
        .unwrap();
        assert_eq!(
            store.get(&reference("a")).unwrap_err(),
            CredentialsError::Store(Failure::Permissions)
        );
        assert_eq!(
            store.set(&reference("a"), &key("replacement")),
            Err(CredentialsError::Store(Failure::Permissions))
        );
        assert_ne!(fs::metadata(target).unwrap().permissions(), original);
        fs::set_permissions(target, original).unwrap();
    }
    assert_eq!(
        store.get(&reference("a")).unwrap().unwrap().expose_secret(),
        "fixture"
    );
}

#[test]
fn links_and_special_files_cannot_redirect_reads_or_writes() {
    let root = temp_root();
    let path = root.path().join("credentials/credentials.json");
    let outside = root.path().join("outside");
    private_file(&path, br#"{"version":1,"entries":[]}"#);
    fs::write(&outside, "untouched").unwrap();
    fs::remove_file(&path).unwrap();
    symlink(&outside, &path).unwrap();
    let store = FileSecretStore::new(&path);
    assert_eq!(
        store.get(&reference("a")).unwrap_err(),
        CredentialsError::Store(Failure::UnsafePath)
    );
    assert!(store.set(&reference("a"), &key("fixture")).is_err());
    assert_eq!(fs::read_to_string(&outside).unwrap(), "untouched");
    fs::remove_file(&path).unwrap();
    fs::hard_link(&outside, &path).unwrap();
    assert_eq!(
        store.get(&reference("a")).unwrap_err(),
        CredentialsError::Store(Failure::UnsafePath)
    );
    fs::remove_file(&path).unwrap();
    fifo(&path);
    assert_eq!(
        store.get(&reference("a")).unwrap_err(),
        CredentialsError::Store(Failure::UnsafePath)
    );
    fs::remove_file(&path).unwrap();
    let alias = root.path().join("alias");
    symlink(path.parent().unwrap(), &alias).unwrap();
    assert!(
        FileSecretStore::new(alias.join("credentials.json"))
            .set(&reference("a"), &key("fixture"))
            .is_err()
    );
    let lock = path.parent().unwrap().join(".credentials.json.lock");
    fs::remove_file(&lock).unwrap();
    symlink(&outside, lock).unwrap();
    assert_eq!(
        store.set(&reference("a"), &key("fixture")),
        Err(CredentialsError::Store(Failure::UnsafePath))
    );
}

#[test]
fn held_writer_lock_times_out_without_changing_the_document() {
    let root = temp_root();
    let path = root.path().join("credentials/credentials.json");
    let store = FileSecretStore::new(&path);
    store.set(&reference("a"), &key("fixture")).unwrap();
    let original = fs::read(&path).unwrap();
    let lock = fs::File::open(path.parent().unwrap().join(".credentials.json.lock")).unwrap();
    lock.lock().unwrap();
    assert_eq!(
        store.set(&reference("a"), &key("replacement")),
        Err(CredentialsError::Store(Failure::LockTimeout))
    );
    assert_eq!(fs::read(&path).unwrap(), original);
    lock.unlock().unwrap();
    store.set(&reference("a"), &key("replacement")).unwrap();
}

#[test]
fn trusted_root_alias_keeps_nested_links_rejected() {
    let root = temp_root();
    let target = root.path().join("target");
    fs::create_dir(&target).unwrap();
    let alias = root.path().join("alias");
    symlink(&target, &alias).unwrap();
    let store = FileSecretStore::with_trusted_root_alias(alias.join("credentials.json"));
    assert!(matches!(
        store.get(&reference("a")),
        Err(CredentialsError::Store(Failure::UnsafePath))
    ));
    assert!(store.set(&reference("a"), &key("fixture")).is_err());
    assert!(!target.join("credentials.json").exists());
}

#[cfg(target_os = "macos")]
#[test]
fn trusted_macos_root_alias_preserves_strict_default() {
    let root = tempfile::tempdir_in("/var/tmp").unwrap();
    let path = root.path().join("credentials/credentials.json");
    let strict = FileSecretStore::new(&path);
    assert!(strict.set(&reference("a"), &key("fixture")).is_err());
    let selected = FileSecretStore::with_trusted_root_alias(&path);
    selected.set(&reference("a"), &key("fixture")).unwrap();
    assert_eq!(
        selected
            .get(&reference("a"))
            .unwrap()
            .unwrap()
            .expose_secret(),
        "fixture"
    );
    assert!(strict.get(&reference("a")).is_err());
}
