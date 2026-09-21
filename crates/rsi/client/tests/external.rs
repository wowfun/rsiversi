use async_trait::async_trait;
use rsi_acp_protocol::{
    observation::{Capabilities, ConversationId, Page, Record, RecordKind, Snapshot, Status},
    service::{
        Endpoint, Error, ExternalConversations, Permission, PermissionOption, Resident, Result,
        Setup, View,
    },
};
use rsi_client::{ExternalCommand, ExternalController};
use rsi_meta_execution::Execution;
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug)]
struct Host {
    view: Mutex<View>,
    sends: AtomicUsize,
    reads: AtomicUsize,
    closes: AtomicUsize,
    answers: AtomicUsize,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
impl Host {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            view: Mutex::new(View {
                snapshot: Snapshot {
                    id: ConversationId::new("external").unwrap(),
                    endpoint: "fixture".into(),
                    cwd: "/fixture".into(),
                    remote: Some("remote".into()),
                    generation: 1,
                    epoch: 1,
                    status: Status::Ready,
                    completion: None,
                    capabilities: Capabilities::default(),
                },
                connected: true,
                permissions: vec![Permission {
                    id: "exact".into(),
                    generation: "1".into(),
                    title: "Approve?".into(),
                    source_sequence: "1".into(),
                    options: vec![
                        PermissionOption {
                            id: "once".into(),
                            name: "Once".into(),
                            kind: "allow_once".into(),
                        },
                        PermissionOption {
                            id: "always".into(),
                            name: "Always".into(),
                            kind: "allow_always".into(),
                        },
                    ],
                }],
            }),
            sends: AtomicUsize::new(0),
            reads: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
            answers: AtomicUsize::new(0),
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        })
    }
}
#[async_trait]
impl ExternalConversations for Host {
    async fn endpoints(&self) -> Result<Vec<Endpoint>> {
        Ok(vec![])
    }
    async fn residents(&self) -> Result<Vec<Resident>> {
        Ok(vec![])
    }
    async fn list(&self, _: Option<ConversationId>) -> Result<Vec<Snapshot>> {
        Ok(vec![])
    }
    async fn view(&self, _: &ConversationId) -> Result<View> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(self.view.lock().unwrap().clone())
    }
    async fn start(&self, _: ConversationId, _: &str) -> Result<Snapshot> {
        Err(Error::Unsupported)
    }
    async fn reconnect(&self, _: &ConversationId, _: Setup) -> Result<Snapshot> {
        Err(Error::Unsupported)
    }
    async fn submit(&self, _: &ConversationId, _: &str) -> Result<Snapshot> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        self.release.notified().await;
        Err(Error::Unknown)
    }
    async fn cancel(&self, _: &ConversationId) -> Result<Snapshot> {
        Err(Error::Unsupported)
    }
    async fn close(&self, _: &ConversationId) -> Result<Snapshot> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        Ok(self.view.lock().unwrap().snapshot.clone())
    }
    async fn answer(&self, _: &ConversationId, _: u64, _: &str, _: &str) -> Result<()> {
        self.answers.fetch_add(1, Ordering::SeqCst);
        self.view.lock().unwrap().permissions.clear();
        Ok(())
    }
    async fn page(&self, _: &ConversationId, epoch: u64, after: u64) -> Result<Page> {
        if epoch != self.view.lock().unwrap().snapshot.epoch {
            return Err(Error::NotFound);
        }
        let end = (after + 64).min(192);
        Ok(Page{records:(after+1..=end).map(|sequence|Record{sequence,epoch,kind:RecordKind::Update,bytes:100,value:Some(json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":format!("epoch {epoch} 界🦀 record {sequence}")}}))}).collect(),has_more:end<192})
    }
    async fn window(&self, _: &ConversationId, _: u64, _: u64, _: usize) -> Result<Vec<u8>> {
        Ok(b"exact raw record".to_vec())
    }
}
async fn attach(host: Arc<Host>) -> Arc<ExternalController> {
    ExternalController::attach(
        host,
        Execution::native(tokio::runtime::Handle::current()),
        ConversationId::new("external").unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test(start_paused = true)]
async fn detach_does_not_close_peer_and_lost_submit_waiter_never_resends_unknown() {
    let host = Host::new();
    let controller = attach(host.clone()).await;
    let waiter = controller.command(ExternalCommand::Submit {
        text: "once".into(),
    });
    host.entered.notified().await;
    drop(waiter);
    assert!(controller.view().busy);
    assert_eq!(
        controller
            .command(ExternalCommand::Submit {
                text: "second".into()
            })
            .await
            .unwrap_err(),
        Error::Busy
    );
    host.release.notify_one();
    let mut changed = controller.changes();
    while controller.view().busy {
        changed.changed().await.unwrap();
    }
    controller.command(ExternalCommand::Refresh).await.unwrap();
    assert_eq!(host.sends.load(Ordering::SeqCst), 1);
    assert_eq!(
        controller.view().diagnostic,
        Some(Error::Unknown.to_string())
    );
    controller.retire().await;
    assert_eq!(host.closes.load(Ordering::SeqCst), 0);
    assert_eq!(
        controller
            .command(ExternalCommand::Close)
            .await
            .unwrap_err(),
        Error::Stale
    );
}

#[tokio::test(start_paused = true)]
async fn bounded_history_replay_source_and_exact_permission_capabilities_stay_separate() {
    let host = Host::new();
    let controller = attach(host.clone()).await;
    assert!(!controller.view().capabilities.goal);
    assert!(!controller.view().capabilities.preset);
    assert!(!controller.view().capabilities.load);
    assert!(!controller.view().capabilities.resume);
    controller.command(ExternalCommand::Next).await.unwrap();
    controller.command(ExternalCommand::Next).await.unwrap();
    let view = controller.view();
    assert_eq!(view.blocks.len(), 128);
    assert!(view.blocks.front().unwrap().text.ends_with("65"));
    let source = view.blocks.front().unwrap().source.clone();
    assert_eq!(
        controller.source(&source, 0).await.unwrap(),
        b"exact raw record"
    );
    let wrong = rsi_conversation::ExternalSource::new(
        source.conversation().clone(),
        source.epoch(),
        source.sequence(),
        RecordKind::User,
    )
    .unwrap();
    assert_eq!(
        controller.source(&wrong, 0).await.unwrap_err(),
        Error::NotFound
    );
    assert_eq!(
        controller
            .command(ExternalCommand::Answer {
                generation: "2".into(),
                permission: "exact".into(),
                option: "always".into()
            })
            .await
            .unwrap_err(),
        Error::Stale
    );
    controller
        .command(ExternalCommand::Answer {
            generation: "1".into(),
            permission: "exact".into(),
            option: "always".into(),
        })
        .await
        .unwrap();
    assert_eq!(host.answers.load(Ordering::SeqCst), 1);
    assert_eq!(
        controller
            .command(ExternalCommand::Answer {
                generation: "1".into(),
                permission: "exact".into(),
                option: "always".into()
            })
            .await
            .unwrap_err(),
        Error::Stale
    );
    host.view.lock().unwrap().snapshot.epoch = 2;
    controller.command(ExternalCommand::Refresh).await.unwrap();
    assert!(
        controller
            .view()
            .blocks
            .iter()
            .all(|block| block.source.epoch() == 2)
    );
    assert_eq!(
        controller.source(&source, 0).await.unwrap_err(),
        Error::NotFound
    );
    controller.retire().await;
}

#[tokio::test(start_paused = true)]
async fn idle_polling_preserves_revision_and_changed_permissions_publish() {
    let host = Host::new();
    let controller = attach(host.clone()).await;
    // Drain the bounded fixture history first.
    controller.command(ExternalCommand::Next).await.unwrap();
    controller.command(ExternalCommand::Next).await.unwrap();
    let revision = controller.revision();
    let mut changes = controller.changes();
    for _ in 0..4 {
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert!(Arc::ptr_eq(&revision, &controller.revision()));
        assert!(!changes.has_changed().unwrap());
    }
    let tail = controller.view_tail(100);
    assert!(!tail.blocks.is_empty());
    assert!(
        tail.blocks
            .iter()
            .map(|block| block.role.len() + block.key.len() + " · \n\n".len() + block.text.len())
            .sum::<usize>()
            <= 100
    );
    host.view.lock().unwrap().permissions.clear();
    changes.changed().await.unwrap();
    assert!(!Arc::ptr_eq(&revision, &controller.revision()));
    assert!(controller.view().observed.permissions.is_empty());
    controller.retire().await;
}

#[tokio::test(start_paused = true)]
async fn idle_reads_back_off_and_detach_does_not_wait_for_an_unsettled_control() {
    let host = Host::new();
    host.view.lock().unwrap().permissions.clear();
    let controller = attach(host.clone()).await;
    controller.command(ExternalCommand::Next).await.unwrap();
    controller.command(ExternalCommand::Next).await.unwrap();
    let before = host.reads.load(Ordering::Relaxed);
    for _ in 0..60 {
        tokio::time::advance(std::time::Duration::from_millis(500)).await;
        tokio::task::yield_now().await;
    }
    let reads = host.reads.load(Ordering::Relaxed) - before;
    assert!(reads <= 9, "idle requests must back off, got {reads}");
    let wait = controller.command(ExternalCommand::Submit {
        text: "once".into(),
    });
    host.entered.notified().await;
    tokio::time::timeout(std::time::Duration::from_millis(100), controller.retire())
        .await
        .expect("detach must not wait for the 45-second control timeout");
    assert_eq!(wait.await, Err(Error::Unknown));
    assert_eq!(host.sends.load(Ordering::SeqCst), 1);
    assert_eq!(host.closes.load(Ordering::SeqCst), 0);
}
