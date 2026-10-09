use super::*;
use async_trait::async_trait;
use rsi_agent_session_protocol::SessionId;
use rsi_api_protocol::AuthenticatedDevice;
use rsi_execution::ExecutionLocation;
use rsi_files_protocol::FilesCaller;

#[allow(dead_code)]
#[path = "../../../../fixtures/rsi/execution/metadata.rs"]
mod metadata;

type BackendEntries = (u64, BTreeMap<FileToken, (FilesBinding, OpenedFile)>);
struct Fixture {
    _root: tempfile::TempDir,
    backend: Arc<Backend>,
    owners: Arc<FileOwners>,
    lease: ExecutionLease,
    target: SessionTarget,
    binding: FilesBinding,
}
impl Fixture {
    fn new(name: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(Backend::default());
        let provider = metadata::provider_with_files(ExecutionLocation::Local, backend.clone(), 0);
        let lease = provider.lease(Arc::new(metadata::Gate::default())).unwrap();
        let owners = Arc::new(FileOwners::default());
        *backend.owners.lock().unwrap() = Arc::downgrade(&owners);
        let target = SessionTarget {
            session_id: SessionId::new(name).unwrap(),
            header_key: "a".repeat(64),
        };
        let binding = FilesBinding::new(
            FilesCaller::default(),
            name,
            &target.header_key,
            root.path().canonicalize().unwrap(),
        )
        .unwrap();
        Self {
            _root: root,
            backend,
            owners,
            lease,
            target,
            binding,
        }
    }
    fn view(&self) -> FilesView {
        FilesView::new(
            self.owners.clone(),
            self.lease.clone(),
            self.target.clone(),
            &CallOrigin::Local,
        )
    }
}

#[tokio::test]
async fn independent_providers_with_equal_tokens_open_and_continue_without_collisions() {
    let first = Fixture::new("collision-first");
    let mut second = Fixture::new("collision-second");
    second.owners = first.owners.clone();
    *second.backend.owners.lock().unwrap() = Arc::downgrade(&second.owners);
    let first_view = first.view();
    let second_view = second.view();
    let a = first_view
        .open(
            first.binding.clone(),
            RelativePath::new(b"first").unwrap(),
            FileKind::File,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let b = second_view
        .open(
            second.binding.clone(),
            RelativePath::new(b"second").unwrap(),
            FileKind::File,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        first.backend.entries.lock().unwrap().1.keys().next(),
        second.backend.entries.lock().unwrap().1.keys().next()
    );
    assert_ne!(a.token, b.token);
    assert_eq!(first_view.describe(&first.binding, &a.token).unwrap(), a);
    assert_eq!(second_view.describe(&second.binding, &b.token).unwrap(), b);
    assert_eq!(
        first_view.describe(&first.binding, &b.token),
        Err(FilesError::Binding)
    );
    for (view, fixture, token) in [
        (&first_view, &first, &a.token),
        (&second_view, &second, &b.token),
    ] {
        assert_eq!(
            view.read(
                fixture.binding.clone(),
                token.clone(),
                0,
                1,
                CancellationToken::new()
            )
            .await,
            Err(FilesError::Unavailable)
        );
        assert_eq!(
            view.list(
                fixture.binding.clone(),
                token.clone(),
                0,
                1,
                CancellationToken::new()
            )
            .await,
            Err(FilesError::Unavailable)
        );
        assert!(view.describe(&fixture.binding, token).is_ok());
        let provider_token = fixture
            .backend
            .entries
            .lock()
            .unwrap()
            .1
            .keys()
            .next()
            .unwrap()
            .clone();
        assert_eq!(
            *fixture.backend.continued.lock().unwrap(),
            vec![provider_token.clone(), provider_token]
        );
    }
    first
        .owners
        .release(&CallOrigin::Local, &first.target, &a.token)
        .unwrap();
    assert!(first.backend.entries.lock().unwrap().1.is_empty());
    assert_eq!(second_view.describe(&second.binding, &b.token).unwrap(), b);
    second
        .owners
        .release(&CallOrigin::Local, &second.target, &b.token)
        .unwrap();
    assert!(second.backend.entries.lock().unwrap().1.is_empty());
}

#[tokio::test]
async fn unavailable_io_preserves_tokens_until_description_confirms_loss() {
    for listing in [false, true] {
        let fixture = Fixture::new("unavailable");
        let view = fixture.view();
        let opened = view
            .open(
                fixture.binding.clone(),
                RelativePath::new(b"object").unwrap(),
                if listing {
                    FileKind::Directory
                } else {
                    FileKind::File
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        for lost in [false, true] {
            if lost {
                fixture.backend.entries.lock().unwrap().1.clear();
            }
            if listing {
                assert_eq!(
                    view.list(
                        fixture.binding.clone(),
                        opened.token.clone(),
                        0,
                        1,
                        CancellationToken::new()
                    )
                    .await,
                    Err(FilesError::Unavailable)
                );
            } else {
                assert_eq!(
                    view.read(
                        fixture.binding.clone(),
                        opened.token.clone(),
                        0,
                        1,
                        CancellationToken::new()
                    )
                    .await,
                    Err(FilesError::Unavailable)
                );
            }
            assert_eq!(
                fixture.owners.capacity.available_permits(),
                MAXIMUM_FILE_TOKENS - usize::from(!lost)
            );
            assert_eq!(
                fixture
                    .owners
                    .entries
                    .lock()
                    .unwrap()
                    .contains_key(&opened.token),
                !lost
            );
        }
    }
}

#[tokio::test]
async fn a_late_token_loss_cannot_evict_a_republished_owner() {
    let fixture = Fixture::new("identity");
    let view = fixture.view();
    let first = view
        .open(
            fixture.binding.clone(),
            RelativePath::new(b"old").unwrap(),
            FileKind::File,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let old = {
        let mut entries = fixture.owners.entries.lock().unwrap();
        let entry = entries.get_mut(&first.token).unwrap();
        Arc::get_mut(entry).unwrap().expires = Instant::now();
        entry.clone()
    };
    fixture.backend.entries.lock().unwrap().0 = 0;
    let fresh = view
        .open(
            fixture.binding.clone(),
            RelativePath::new(b"new").unwrap(),
            FileKind::File,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_ne!(first.token, fresh.token);
    fixture.owners.unavailable(&fresh.token, &old);
    assert_eq!(
        view.describe(&fixture.binding, &fresh.token).unwrap(),
        fresh
    );
    assert_eq!(
        fixture.owners.capacity.available_permits(),
        MAXIMUM_FILE_TOKENS - 2
    );
    drop(old);
    assert_eq!(
        fixture.owners.capacity.available_permits(),
        MAXIMUM_FILE_TOKENS - 1
    );
    fixture
        .owners
        .release(&CallOrigin::Local, &fixture.target, &fresh.token)
        .unwrap();
    assert_eq!(
        fixture.owners.capacity.available_permits(),
        MAXIMUM_FILE_TOKENS
    );
}

#[derive(Debug, Default)]
struct Backend {
    entries: Mutex<BackendEntries>,
    continued: Mutex<Vec<FileToken>>,
    owners: Mutex<std::sync::Weak<FileOwners>>,
    pause: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
}
#[async_trait]
impl Files for Backend {
    fn release_caller(&self, caller: &FilesCaller) {
        if let Some(owners) = self.owners.lock().unwrap().upgrade() {
            assert!(
                owners.entries.try_lock().is_ok(),
                "resource dropped inside owner lock"
            );
        }
        self.entries
            .lock()
            .unwrap()
            .1
            .retain(|_, (binding, _)| binding.caller() != caller);
    }
    fn describe(&self, binding: &FilesBinding, token: &FileToken) -> Result<OpenedFile> {
        self.entries
            .lock()
            .unwrap()
            .1
            .get(token)
            .filter(|(b, _)| b == binding)
            .map(|(_, opened)| opened.clone())
            .ok_or(FilesError::Unavailable)
    }
    async fn open(
        &self,
        binding: FilesBinding,
        path: RelativePath,
        kind: FileKind,
        _: CancellationToken,
    ) -> Result<OpenedFile> {
        let pause = self.pause.lock().unwrap().take();
        if let Some((entered, finish)) = pause {
            let _ = entered.send(());
            let _ = finish.await;
        }
        let mut entries = self.entries.lock().unwrap();
        entries.0 += 1;
        let opened = OpenedFile {
            token: FileToken::try_from(format!("{:032x}", entries.0)).unwrap(),
            path,
            kind,
            length: 1,
            executable: false,
        };
        entries
            .1
            .insert(opened.token.clone(), (binding, opened.clone()));
        Ok(opened)
    }
    async fn read(
        &self,
        _: FilesBinding,
        token: FileToken,
        _: u64,
        _: usize,
        _: CancellationToken,
    ) -> Result<FilePage> {
        self.continued.lock().unwrap().push(token);
        Err(FilesError::Unavailable)
    }
    async fn list(
        &self,
        _: FilesBinding,
        token: FileToken,
        _: usize,
        _: usize,
        _: CancellationToken,
    ) -> Result<DirectoryPage> {
        self.continued.lock().unwrap().push(token);
        Err(FilesError::Unavailable)
    }
    fn release(&self, _: &FilesBinding, token: &FileToken) -> Result<()> {
        self.entries.lock().unwrap().1.remove(token);
        Ok(())
    }
}

#[tokio::test]
async fn expired_owners_recover_capacity_after_lost_release() {
    let root = tempfile::tempdir().unwrap();
    let backend = Arc::new(Backend::default());
    let provider = metadata::provider_with_files(ExecutionLocation::Local, backend.clone(), 0);
    let lease = provider.lease(Arc::new(metadata::Gate::default())).unwrap();
    let owners = Arc::new(FileOwners::default());
    *backend.owners.lock().unwrap() = Arc::downgrade(&owners);
    let origin = CallOrigin::Device(AuthenticatedDevice {
        id: DeviceId::from_bytes([1; 16]),
        revoked: CancellationToken::new(),
    });
    let target = SessionTarget {
        session_id: SessionId::new("expiry").unwrap(),
        header_key: "a".repeat(64),
    };
    let binding = FilesBinding::new(
        FilesCaller::default(),
        "expiry",
        &target.header_key,
        root.path().canonicalize().unwrap(),
    )
    .unwrap();
    let view = FilesView::new(owners.clone(), lease.clone(), target.clone(), &origin);
    for _ in 0..3 {
        for _ in 0..MAXIMUM_FILE_TOKENS {
            view.open(
                binding.clone(),
                RelativePath::new(b"file").unwrap(),
                FileKind::File,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        }
        assert_eq!(owners.capacity.available_permits(), 0);
        let held = {
            let mut entries = owners.entries.lock().unwrap();
            for entry in entries.values_mut() {
                Arc::get_mut(entry).unwrap().expires = Instant::now();
            }
            let (_, entry) = entries.first_key_value().unwrap();
            entry.clone()
        };
        let expired = backend
            .entries
            .lock()
            .unwrap()
            .1
            .get(&held.provider_token)
            .unwrap()
            .1
            .clone();
        assert_eq!(
            owners.publish(expired, held.clone()),
            Err(FilesError::Cancelled)
        );
        backend.entries.lock().unwrap().1.clear();
        let fresh = view
            .open(
                binding.clone(),
                RelativePath::new(b"fresh").unwrap(),
                FileKind::File,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(owners.capacity.available_permits(), MAXIMUM_FILE_TOKENS - 2);
        owners.release(&origin, &target, &fresh.token).unwrap();
        assert_eq!(owners.capacity.available_permits(), MAXIMUM_FILE_TOKENS - 1);
        drop(held);
        assert_eq!(owners.capacity.available_permits(), MAXIMUM_FILE_TOKENS);
    }
    drop(view);
    let next = FilesView::new(owners.clone(), lease, target, &CallOrigin::Local);
    let opened = next
        .open(
            binding.clone(),
            RelativePath::new(b"file").unwrap(),
            FileKind::File,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    backend.entries.lock().unwrap().1.clear();
    assert_eq!(
        next.describe(&binding, &opened.token),
        Err(FilesError::Unavailable)
    );
    assert_eq!(owners.capacity.available_permits(), MAXIMUM_FILE_TOKENS);
    owners.clear();
    assert!(matches!(
        next.open(
            binding,
            RelativePath::new(b"file").unwrap(),
            FileKind::File,
            CancellationToken::new()
        )
        .await,
        Err(FilesError::Cancelled)
    ));
}

#[tokio::test]
async fn the_last_resource_slot_includes_an_unpublished_open_and_retires_without_dispatch() {
    let fixture = Fixture::new("burst");
    let view = fixture.view();
    let mut tokens = Vec::new();
    for _ in 1..MAXIMUM_FILE_TOKENS {
        tokens.push(
            view.open(
                fixture.binding.clone(),
                RelativePath::new(b"file").unwrap(),
                FileKind::File,
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .token,
        );
    }
    let (entered, entering) = oneshot::channel();
    let (finish, finishing) = oneshot::channel();
    *fixture.backend.pause.lock().unwrap() = Some((entered, finishing));
    let last_view = fixture.view();
    let binding = fixture.binding.clone();
    let last = tokio::spawn(async move {
        last_view
            .open(
                binding,
                RelativePath::new(b"file").unwrap(),
                FileKind::File,
                CancellationToken::new(),
            )
            .await
    });
    entering.await.unwrap();
    assert_eq!(fixture.owners.capacity.available_permits(), 0);
    assert_eq!(
        view.open(
            fixture.binding.clone(),
            RelativePath::new(b"overflow").unwrap(),
            FileKind::File,
            CancellationToken::new()
        )
        .await,
        Err(FilesError::Capacity)
    );
    assert_eq!(
        fixture.backend.entries.lock().unwrap().0,
        (MAXIMUM_FILE_TOKENS - 1) as u64
    );
    fixture
        .owners
        .release(&CallOrigin::Local, &fixture.target, &tokens[0])
        .unwrap();
    let replacement = view
        .open(
            fixture.binding.clone(),
            RelativePath::new(b"replacement").unwrap(),
            FileKind::File,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(view.describe(&fixture.binding, &replacement.token).is_ok());
    finish.send(()).unwrap();
    assert!(last.await.unwrap().is_ok());
    assert_eq!(fixture.owners.capacity.available_permits(), 0);
    fixture.owners.clear();
    assert_eq!(
        view.open(
            fixture.binding.clone(),
            RelativePath::new(b"retired").unwrap(),
            FileKind::File,
            CancellationToken::new()
        )
        .await,
        Err(FilesError::Cancelled)
    );
    assert_eq!(
        fixture.backend.entries.lock().unwrap().0,
        (MAXIMUM_FILE_TOKENS + 1) as u64
    );
}

#[tokio::test]
async fn sixty_four_in_flight_opens_exhaust_admission_before_the_sixty_fifth_dispatch() {
    let fixture = Fixture::new("concurrent-burst");
    let mut requests = Vec::new();
    let mut releases = Vec::new();
    for _ in 0..MAXIMUM_FILE_TOKENS {
        let (entered, entering) = oneshot::channel();
        let (finish, finishing) = oneshot::channel();
        *fixture.backend.pause.lock().unwrap() = Some((entered, finishing));
        let view = fixture.view();
        let binding = fixture.binding.clone();
        requests.push(tokio::spawn(async move {
            view.open(
                binding,
                RelativePath::new(b"file").unwrap(),
                FileKind::File,
                CancellationToken::new(),
            )
            .await
        }));
        entering.await.unwrap();
        releases.push(finish);
    }
    assert_eq!(fixture.owners.capacity.available_permits(), 0);
    assert_eq!(
        fixture
            .view()
            .open(
                fixture.binding.clone(),
                RelativePath::new(b"overflow").unwrap(),
                FileKind::File,
                CancellationToken::new()
            )
            .await,
        Err(FilesError::Capacity)
    );
    assert_eq!(fixture.backend.entries.lock().unwrap().0, 0);
    for release in releases {
        release.send(()).unwrap();
    }
    for request in requests {
        request.await.unwrap().unwrap();
    }
    assert_eq!(
        fixture.owners.entries.lock().unwrap().len(),
        MAXIMUM_FILE_TOKENS
    );
    fixture.owners.clear();
    assert_eq!(
        fixture.owners.capacity.available_permits(),
        MAXIMUM_FILE_TOKENS
    );
}

#[tokio::test]
async fn a_foreign_principal_cannot_describe_or_release_an_owned_token() {
    let fixture = Fixture::new("principal");
    let view = fixture.view();
    let opened = view
        .open(
            fixture.binding.clone(),
            RelativePath::new(b"file").unwrap(),
            FileKind::File,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let foreign = CallOrigin::Device(AuthenticatedDevice {
        id: DeviceId::from_bytes([7; 16]),
        revoked: CancellationToken::new(),
    });
    let foreign_view = FilesView::new(
        fixture.owners.clone(),
        fixture.lease.clone(),
        fixture.target.clone(),
        &foreign,
    );
    assert_eq!(
        foreign_view.describe(&fixture.binding, &opened.token),
        Err(FilesError::Binding)
    );
    assert_eq!(
        fixture
            .owners
            .release(&foreign, &fixture.target, &opened.token),
        Err(FilesError::Binding)
    );
    assert_eq!(
        view.describe(&fixture.binding, &opened.token).unwrap(),
        opened
    );
}

#[tokio::test]
async fn admitted_open_keeps_capacity_after_waiter_loss_and_cannot_publish_after_clear() {
    let root = tempfile::tempdir().unwrap();
    for abandon in [false, true] {
        let backend = Arc::new(Backend::default());
        let provider = metadata::provider_with_files(ExecutionLocation::Local, backend.clone(), 0);
        let lease = provider.lease(Arc::new(metadata::Gate::default())).unwrap();
        let owners = Arc::new(FileOwners::default());
        *backend.owners.lock().unwrap() = Arc::downgrade(&owners);
        let target = SessionTarget {
            session_id: SessionId::new("publication").unwrap(),
            header_key: "a".repeat(64),
        };
        let binding = FilesBinding::new(
            FilesCaller::default(),
            "publication",
            &target.header_key,
            root.path().canonicalize().unwrap(),
        )
        .unwrap();
        let view = FilesView::new(owners.clone(), lease, target, &CallOrigin::Local);
        let (entered, entering) = oneshot::channel();
        let (finish, finishing) = oneshot::channel();
        *backend.pause.lock().unwrap() = Some((entered, finishing));
        let request = tokio::spawn(async move {
            view.open(
                binding,
                RelativePath::new(b"file").unwrap(),
                FileKind::File,
                CancellationToken::new(),
            )
            .await
        });
        entering.await.unwrap();
        assert_eq!(owners.capacity.available_permits(), MAXIMUM_FILE_TOKENS - 1);
        if abandon {
            request.abort();
        } else {
            owners.clear();
        }
        assert_eq!(owners.capacity.available_permits(), MAXIMUM_FILE_TOKENS - 1);
        finish.send(()).unwrap();
        if abandon {
            assert!(request.await.unwrap_err().is_cancelled());
        } else {
            assert_eq!(request.await.unwrap(), Err(FilesError::Cancelled));
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while owners.capacity.available_permits() != MAXIMUM_FILE_TOKENS {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(owners.entries.lock().unwrap().is_empty());
    }
}
