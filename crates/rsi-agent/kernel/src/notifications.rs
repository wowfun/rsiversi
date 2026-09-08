use super::*;

#[derive(Clone, Default)]
pub(super) struct SessionWatchHub(Arc<Mutex<BTreeMap<WatchKey, watch::Sender<u64>>>>);

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
enum WatchKey {
    Session(SessionId),
    TreeMembership(SessionId),
}

pub(super) struct SessionWatch {
    hub: SessionWatchHub,
    key: WatchKey,
    receiver: Option<watch::Receiver<u64>>,
}

impl SessionWatchHub {
    fn subscribe(&self, key: WatchKey) -> SessionWatch {
        let receiver = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(key.clone())
            .or_insert_with(|| watch::channel(0).0)
            .subscribe();
        SessionWatch {
            hub: self.clone(),
            key,
            receiver: Some(receiver),
        }
    }

    pub(super) fn session(&self, id: &SessionId) -> SessionWatch {
        self.subscribe(WatchKey::Session(id.clone()))
    }

    pub(super) fn tree(&self, id: &SessionId) -> SessionWatch {
        self.subscribe(WatchKey::TreeMembership(id.clone()))
    }

    fn notify(&self, key: &WatchKey) {
        if let Some(sender) = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
        {
            sender.send_modify(|revision| *revision = revision.wrapping_add(1));
        }
    }

    pub(super) fn committed(&self, id: &SessionId) {
        self.notify(&WatchKey::Session(id.clone()));
    }

    pub(super) fn created_in_tree(&self, root: &SessionId) {
        self.notify(&WatchKey::TreeMembership(root.clone()));
    }
}

impl SessionWatch {
    pub(super) fn has_changed(&self) -> bool {
        self.receiver
            .as_ref()
            .expect("active watch")
            .has_changed()
            .unwrap_or(false)
    }

    pub(super) fn mark_seen(&mut self) {
        self.receiver
            .as_mut()
            .expect("active watch")
            .borrow_and_update();
    }

    pub(super) async fn changed(&mut self) {
        let _ = self
            .receiver
            .as_mut()
            .expect("active watch")
            .changed()
            .await;
    }
}

impl Drop for SessionWatch {
    fn drop(&mut self) {
        let mut hub = self
            .hub
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Drop the receiver under the same lock as the zero-subscriber check.
        // Concurrent final drops must not both observe each other's receiver.
        drop(self.receiver.take());
        if hub
            .get(&self.key)
            .is_some_and(|sender| sender.receiver_count() == 0)
        {
            hub.remove(&self.key);
        }
    }
}

impl KernelInner {
    pub(super) async fn commit_agent(
        &self,
        mut commit: AtomicAgentCommit,
    ) -> rsi_agent_store_protocol::Result<rsi_agent_store_protocol::AtomicAgentCommitResult> {
        correlate_terminal_boundaries(&mut commit)?;
        let created = commit
            .sessions
            .iter()
            .filter_map(|append| append.header.as_ref())
            .map(|header| {
                header
                    .fork_origin()
                    .map_or(header.session_id(), |origin| &origin.root_session_id)
                    .clone()
            })
            .collect::<Vec<_>>();
        let result = self.store.commit_agent(commit).await;
        if let Ok(committed) = &result {
            for watermark in &committed.sessions {
                self.session_changes.committed(&watermark.session_id);
            }
        }
        // An I/O error can lose the acknowledgement of a committed creation.
        // Membership watches carry only requery hints, so failed attempts may wake them.
        for root in &created {
            self.session_changes.created_in_tree(root);
        }
        result
    }
}

/// The only Kernel producer of terminal correlation records, including startup recovery.
pub(super) fn correlate_terminal_boundaries(
    commit: &mut AtomicAgentCommit,
) -> rsi_agent_store_protocol::Result<()> {
    for append in &mut commit.sessions {
        if let Some(terminal) = append.facts.iter().find(|fact| is_terminal_fact(fact)) {
            let seq = append
                .controls
                .last()
                .map_or(append.expected_control_seq, AgentControlRecord::seq)
                .checked_add(1)
                .ok_or_else(|| StoreError::Invalid("terminal control sequence exhausted".into()))?;
            append.controls.push(
                terminal_boundary_record(seq, terminal)
                    .map_err(|error| StoreError::Invalid(error.to_string()))?,
            );
        }
    }
    commit.validate()
}

pub(super) fn terminal_boundary_record(
    seq: u64,
    terminal: &SessionFact,
) -> rsi_agent_session_protocol::Result<AgentControlRecord> {
    AgentControlRecord::new(
        seq,
        terminal.timestamp_ms(),
        AgentControlRecordBody::TurnBoundaryRecorded {
            turn_id: terminal.body().turn_id().clone(),
            terminal_fact_seq: terminal.seq(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_last_watch_drops_leave_no_inactive_registry_entry() {
        let hub = SessionWatchHub::default();
        let session = SessionId::new("concurrent-drops").unwrap();
        for _ in 0..64 {
            let barrier = Arc::new(tokio::sync::Barrier::new(8));
            let mut tasks = Vec::new();
            for _ in 0..8 {
                let watch = hub.session(&session);
                let barrier = barrier.clone();
                tasks.push(tokio::spawn(async move {
                    barrier.wait().await;
                    drop(watch);
                }));
            }
            for task in tasks {
                task.await.unwrap();
            }
            assert!(hub.0.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn scoped_changes_coalesce_and_only_active_watch_entries_are_retained() {
        let hub = SessionWatchHub::default();
        let first = SessionId::new("first").unwrap();
        let other = SessionId::new("other").unwrap();
        let mut session = hub.session(&first);
        let mut tree = hub.tree(&first);
        let duplicate = hub.session(&first);
        assert_eq!(hub.0.lock().unwrap().len(), 2);
        hub.committed(&other);
        assert!(!session.receiver.as_ref().unwrap().has_changed().unwrap());
        hub.committed(&first);
        hub.committed(&first);
        assert!(session.receiver.as_ref().unwrap().has_changed().unwrap());
        assert!(!tree.receiver.as_ref().unwrap().has_changed().unwrap());
        session.changed().await;
        session.mark_seen();
        assert!(!session.receiver.as_ref().unwrap().has_changed().unwrap());
        hub.created_in_tree(&first);
        tree.changed().await;
        assert!(!session.receiver.as_ref().unwrap().has_changed().unwrap());
        drop(session);
        assert_eq!(hub.0.lock().unwrap().len(), 2);
        drop(duplicate);
        assert_eq!(hub.0.lock().unwrap().len(), 1);
        drop(tree);
        assert!(hub.0.lock().unwrap().is_empty());
    }
}
