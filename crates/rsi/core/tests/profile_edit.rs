#![cfg(unix)]

use rsi::{HostProfileId, ProfileCatalog, ProfileEditError};
use rsi_host::{HostBuilder, HostPaths};
use std::fs;

#[test]
fn edit_is_pure_consumes_one_root_and_rejects_stale_sources() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let catalog = ProfileCatalog::new(
        HostPaths::new(root.join("config"), root.join("state"), root.join("cache")).unwrap(),
    );
    let id = HostProfileId::new("edit").unwrap();
    let path = catalog
        .copy_host(&HostProfileId::new("standard").unwrap(), &id)
        .unwrap();
    let host = HostBuilder::new(catalog.paths().clone()).build().unwrap();
    let old = fs::read(&path).unwrap();
    let proposed = b"format = 1\n# confidential source comment\n";
    let first = catalog.preview_host_edit(&host, &id, proposed).unwrap();
    let stale = catalog
        .preview_host_edit(&host, &id, b"format = 1\n# stale\n")
        .unwrap();
    assert_eq!(first.original_source(), old);
    assert_eq!(first.proposed_source(), proposed);
    assert_eq!(fs::read(&path).unwrap(), old);
    assert!(!format!("{first:?}").contains("confidential"));
    let expected = first.effective().proposed.source_digest().to_owned();
    let receipt = first.commit_once().unwrap();
    assert!(receipt.directory_synced);
    assert_eq!(receipt.source_digest, expected);
    assert_eq!(fs::read(&path).unwrap(), proposed);
    assert!(matches!(
        stale.commit_once(),
        Err(ProfileEditError::Conflict)
    ));
    assert_eq!(fs::read(&path).unwrap(), proposed);
    assert!(
        catalog
            .preview_host_edit(&host, &HostProfileId::new("standard").unwrap(), proposed)
            .is_err()
    );
}

fn fixture() -> (
    tempfile::TempDir,
    ProfileCatalog,
    HostProfileId,
    std::path::PathBuf,
    rsi_host::Host,
) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let catalog = ProfileCatalog::new(
        HostPaths::new(root.join("config"), root.join("state"), root.join("cache")).unwrap(),
    );
    let id = HostProfileId::new("edit").unwrap();
    let path = catalog
        .copy_host(&HostProfileId::new("standard").unwrap(), &id)
        .unwrap();
    let host = HostBuilder::new(catalog.paths().clone()).build().unwrap();
    (temp, catalog, id, path, host)
}

#[test]
fn changed_or_missing_includes_reject_without_writing_or_retaining_staging() {
    let (_temp, catalog, id, path, host) = fixture();
    let child = path.with_file_name("child.toml");
    fs::write(&child, "format = 1\n").unwrap();
    let proposed = b"format = 1\n[[steps]]\nkind = 'include'\npath = 'child.toml'\n";
    let before = fs::read(&path).unwrap();
    let edit = catalog.preview_host_edit(&host, &id, proposed).unwrap();
    fs::write(&child, "format = 1\n# include changed\n").unwrap();
    assert!(matches!(
        edit.commit_once(),
        Err(ProfileEditError::Conflict)
    ));
    assert_eq!(fs::read(&path).unwrap(), before);
    let edit = catalog.preview_host_edit(&host, &id, proposed).unwrap();
    fs::remove_file(&child).unwrap();
    assert!(matches!(
        edit.commit_once(),
        Err(ProfileEditError::Conflict)
    ));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
}

#[test]
fn competing_directory_lock_and_parent_replacement_do_not_redirect_edits() {
    let (_temp, catalog, id, path, host) = fixture();
    let before = fs::read(&path).unwrap();
    let proposed = b"format = 1\n# replacement\n";
    let edit = catalog.preview_host_edit(&host, &id, proposed).unwrap();
    let lock = fs::File::open(path.parent().unwrap()).unwrap();
    lock.try_lock().unwrap();
    assert!(matches!(edit.commit_once(), Err(ProfileEditError::Busy)));
    lock.unlock().unwrap();
    assert_eq!(fs::read(&path).unwrap(), before);
    let edit = catalog.preview_host_edit(&host, &id, proposed).unwrap();
    let original_dir = path.parent().unwrap().with_file_name("moved");
    fs::rename(path.parent().unwrap(), &original_dir).unwrap();
    fs::create_dir(path.parent().unwrap()).unwrap();
    fs::write(&path, &before).unwrap();
    assert!(matches!(
        edit.commit_once(),
        Err(ProfileEditError::Conflict)
    ));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(
        fs::read(original_dir.join(rsi::HOST_PROFILE_FILE)).unwrap(),
        before
    );
    assert_eq!(fs::read_dir(original_dir).unwrap().count(), 1);
}

#[test]
fn source_links_special_files_and_oversize_are_rejected_without_blocking() {
    use std::os::unix::fs::symlink;
    let (_temp, catalog, id, path, host) = fixture();
    let proposed = b"format = 1\n";
    assert!(matches!(
        catalog.preview_host_edit(&host, &id, &vec![b' '; 1024 * 1024 + 1]),
        Err(ProfileEditError::TooLarge)
    ));
    let target = path.with_file_name("target");
    fs::rename(&path, &target).unwrap();
    symlink(&target, &path).unwrap();
    assert!(catalog.preview_host_edit(&host, &id, proposed).is_err());
    fs::remove_file(&path).unwrap();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &path,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        0,
    )
    .unwrap();
    assert!(matches!(
        catalog.preview_host_edit(&host, &id, proposed),
        Err(ProfileEditError::InvalidSource)
    ));
    fs::remove_file(&path).unwrap();
    fs::rename(&target, &path).unwrap();
    let old = path.parent().unwrap().with_file_name("real");
    fs::rename(path.parent().unwrap(), &old).unwrap();
    symlink(&old, path.parent().unwrap()).unwrap();
    assert!(catalog.preview_host_edit(&host, &id, proposed).is_err());
}

#[test]
fn invalid_application_can_be_repaired_without_preparing_and_drop_keeps_source() {
    use rsi::ApplicationProfileId;
    use std::os::unix::fs::PermissionsExt as _;
    let (_temp, catalog, _, _, host) = fixture();
    let id = ApplicationProfileId::new("repair").unwrap();
    let path = catalog.application_path(&id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "secret = 'unfinished").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    let edit = catalog
        .preview_application_edit(&host, &id, b"format = 1\n")
        .unwrap();
    assert!(edit.effective().previous.is_none());
    assert!(edit.effective().changes.is_none());
    drop(edit);
    assert_eq!(fs::read(&path).unwrap(), b"secret = 'unfinished");
    let edit = catalog
        .preview_application_edit(&host, &id, b"format = 1\n")
        .unwrap();
    edit.commit_once().unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"format = 1\n");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    assert!(
        catalog
            .preview_application_edit(
                &host,
                &ApplicationProfileId::new("cli").unwrap(),
                b"format = 1\n"
            )
            .is_err()
    );
}

#[derive(Debug, Default)]
struct FailsActivation {
    prepared: std::sync::atomic::AtomicUsize,
    activated: std::sync::atomic::AtomicUsize,
}
#[async_trait::async_trait]
impl rsi_meta::PluginFactory for FailsActivation {
    fn prepare(
        &self,
        desired: &rsi_meta::ConfigValue,
    ) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        self.prepared
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(rsi_meta::PreparedActivation::new(desired.clone()))
    }
    async fn activate(&self, _: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        self.activated
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(rsi_meta::MetaError::Service(
            "expected activation refusal".into(),
        ))
    }
}

#[tokio::test]
async fn publication_is_independent_of_later_activation_failure_and_catalog_revision() {
    use std::sync::atomic::Ordering;
    let (_temp, catalog, id, path, _) = fixture();
    let factory = std::sync::Arc::new(FailsActivation::default());
    let build = |revision| {
        let mut builder = HostBuilder::new(catalog.paths().clone());
        builder
            .register_linked(
                "test.fail",
                revision,
                rsi_meta::UpdateMode::Replayable,
                factory.clone(),
            )
            .unwrap();
        builder.build().unwrap()
    };
    let first = build("first");
    let second = build("second");
    let proposed = b"format = 1\n[[steps]]\nkind = 'plugin'\nid = 'fail'\nplugin = 'test.fail'\n";
    let edit = catalog.preview_host_edit(&first, &id, proposed).unwrap();
    let other = catalog.preview_host_edit(&second, &id, proposed).unwrap();
    assert_ne!(edit.review_digest(), other.review_digest());
    drop(other);
    assert_eq!(factory.prepared.load(Ordering::SeqCst), 0);
    edit.commit_once().unwrap();
    assert_eq!(factory.prepared.load(Ordering::SeqCst), 0);
    assert_eq!(factory.activated.load(Ordering::SeqCst), 0);
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), first.start_file(&path))
        .await
        .unwrap();
    assert!(result.is_err());
    assert!(factory.prepared.load(Ordering::SeqCst) > 0);
    assert_eq!(factory.activated.load(Ordering::SeqCst), 1);
    assert_eq!(fs::read(&path).unwrap(), proposed);
}
