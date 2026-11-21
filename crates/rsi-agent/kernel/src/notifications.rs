use super::*;

#[derive(Clone, Default)]
pub(super) struct SessionWatchHub(
    Arc<Mutex<BTreeMap<WatchKey, watch::Sender<u64>>>>,
    Arc<std::sync::atomic::AtomicU64>,
);

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
enum WatchKey {
    Directory,
    Session(SessionId),
    TreeMembership(SessionId),
    EditedDomain(String),
}

pub(super) struct SessionWatch {
    hub: SessionWatchHub,
    key: WatchKey,
    receiver: Option<watch::Receiver<u64>>,
}

impl SessionWatchHub {
    pub(super) fn directory(&self) -> SessionWatch {
        self.subscribe(WatchKey::Directory)
    }
    pub(super) fn revision(&self) -> Option<u64> {
        let revision = self.1.load(Ordering::Acquire);
        (revision != u64::MAX).then_some(revision)
    }

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

    pub(super) fn edited_domain(&self, domain: &str) -> SessionWatch {
        self.subscribe(WatchKey::EditedDomain(domain.to_owned()))
    }

    fn domains_changed(&self, domains: BTreeSet<WatchKey>) {
        if domains.is_empty() {
            return;
        }
        let watchers = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for domain in domains {
            if let Some(sender) = watchers.get(&domain) {
                sender.send_modify(|revision| *revision = revision.wrapping_add(1));
            }
        }
    }

    pub(super) fn domains_retired(&self) {
        let watchers = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (key, sender) in watchers.iter() {
            if matches!(key, WatchKey::EditedDomain(_)) {
                sender.send_modify(|revision| *revision = revision.wrapping_add(1));
            }
        }
    }

    fn notify(&self, key: &WatchKey) {
        let _exhausted = self
            .1
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            });

        let watchers = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for key in [key, &WatchKey::Directory] {
            if let Some(sender) = watchers.get(key) {
                sender.send_modify(|revision| *revision = revision.wrapping_add(1));
            }
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
    pub(super) fn emit_activity(&self, event: rsi_agent_turn_protocol::SessionActivityEvent) {
        if let Some(context) = &self.event_context {
            // Retirement can close delivery after the Store has already committed.
            let _ = context.dispatch_local::<rsi_agent_turn_protocol::SessionActivityEvent>(event);
        }
    }

    pub(super) fn notify_committed(
        &self,
        committed: &rsi_agent_store_protocol::AtomicAgentCommitResult,
    ) {
        for watermark in &committed.sessions {
            self.session_changes.committed(&watermark.session_id);
        }
    }

    pub(super) async fn commit_agent(
        &self,
        mut commit: AtomicAgentCommit,
    ) -> rsi_agent_store_protocol::Result<rsi_agent_store_protocol::AtomicAgentCommitResult> {
        let mut completions = Vec::new();
        correlate_terminal_boundaries(&mut commit, |id, fact| {
            if self.event_context.is_some() {
                completions.push(completion_event(id, fact));
            }
        })?;
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
        let revoked_programs = self.program_guard_revocations(&commit);
        // A reader may subscribe while the Store is committing. Keep only the
        // names needed to wake it after publication, never the snapshots.
        let edited_domains = commit
            .sessions
            .iter()
            .flat_map(|append| &append.controls)
            .filter_map(|record| match record.body() {
                AgentControlRecordBody::DomainStateCommitted { commit } => Some(commit.updates()),
                _ => None,
            })
            .flatten()
            .filter(|update| update.revision().get() > 1)
            .map(|update| WatchKey::EditedDomain(update.snapshot().identity().id().to_owned()))
            .collect();
        let retired_messages = commit
            .sessions
            .iter()
            .flat_map(|append| {
                append
                    .controls
                    .iter()
                    .filter_map(|record| match record.body() {
                        AgentControlRecordBody::MessageDiscarded { message_id, .. }
                        | AgentControlRecordBody::MessageClaimed { message_id, .. } => {
                            Some((append.session_id.clone(), message_id.clone()))
                        }
                        AgentControlRecordBody::MessageSuccessor { predecessor_id, .. } => {
                            Some((append.session_id.clone(), predecessor_id.clone()))
                        }
                        _ => None,
                    })
            })
            .collect::<Vec<_>>();
        let needs_proof = commit
            .sessions
            .iter()
            .flat_map(|append| &append.controls)
            .any(|record| {
                matches!(
                    record.body(),
                    AgentControlRecordBody::ProgramRun { .. }
                        | AgentControlRecordBody::InboxPaused
                        | AgentControlRecordBody::MessageDiscarded { .. }
                )
            })
            || commit
                .sessions
                .iter()
                .flat_map(|append| &append.facts)
                .any(|fact| matches!(fact.body(), SessionFactBody::CancelRequested { .. }));
        let proof = needs_proof.then(|| commit.clone());
        let mut result = self.store.commit_agent(commit).await;
        if result.is_ok() || matches!(&result, Err(StoreError::Io(_))) {
            self.session_changes.domains_changed(edited_domains);
        }
        if matches!(&result, Err(StoreError::Io(_)))
            && let Some(proof) = &proof
        {
            match self.reconcile_commit(proof).await {
                Ok(Some(receipt)) => result = Ok(receipt),
                Ok(None) => {}
                Err(error) => self.fail_unverified_cancellations(proof, &error),
            }
        }
        if result.is_ok() || matches!(&result, Err(rsi_agent_store_protocol::StoreError::Io(_))) {
            for cancellation in revoked_programs {
                cancellation.cancel();
            }
        }
        if let Ok(committed) = &result {
            for (session, message) in &retired_messages {
                self.execution_messages.remove(session, message);
            }
            for event in completions {
                self.emit_activity(event);
            }
            self.notify_committed(committed);
        }
        // An I/O error can lose the acknowledgement of a committed creation.
        // Membership watches carry only requery hints, so failed attempts may wake them.
        for root in &created {
            self.session_changes.created_in_tree(root);
        }
        result
    }
}

pub(super) fn completion_event(
    session: &SessionId,
    terminal: &SessionFact,
) -> rsi_agent_turn_protocol::SessionActivityEvent {
    let SessionFactBody::TurnTerminal { outcome, .. } = terminal.body() else {
        unreachable!("terminal publication")
    };
    rsi_agent_turn_protocol::SessionActivityEvent::TurnEnded {
        session: session.clone(),
        sequence: terminal.seq(),
        failed: matches!(
            outcome,
            TurnOutcome::Failed { .. } | TurnOutcome::PartialFailed { .. }
        ),
    }
}

/// The only Kernel producer of terminal correlation records, including startup recovery.
pub(super) fn correlate_terminal_boundaries(
    commit: &mut AtomicAgentCommit,
    mut on_terminal: impl FnMut(&SessionId, &SessionFact),
) -> rsi_agent_store_protocol::Result<()> {
    for append in &mut commit.sessions {
        if let Some(terminal) = append.facts.iter().find(|fact| is_terminal_fact(fact)) {
            let seq = append
                .controls
                .last()
                .map_or(append.expected_control_seq, AgentControlRecord::seq)
                .checked_add(1)
                .ok_or_else(|| StoreError::Invalid("terminal control sequence exhausted".into()))?;
            on_terminal(&append.session_id, terminal);
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
            turn_id: terminal
                .body()
                .turn_id()
                .ok_or_else(|| {
                    rsi_agent_session_protocol::SessionError::Invalid(
                        "turn boundary requires a turn-owned fact".into(),
                    )
                })?
                .clone(),
            terminal_fact_seq: terminal.seq(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_revision_needs_no_observers_and_exhaustion_disables_caching() {
        let hub = SessionWatchHub::default();
        assert_eq!(hub.revision(), Some(0));
        hub.committed(&SessionId::new("activity").unwrap());
        assert_eq!(hub.revision(), Some(1));
        assert!(hub.0.lock().unwrap().is_empty());
        hub.1.store(u64::MAX - 1, Ordering::Release);
        hub.committed(&SessionId::new("activity").unwrap());
        assert_eq!(hub.revision(), None);
        hub.committed(&SessionId::new("activity").unwrap());
        assert_eq!(hub.revision(), None);
    }
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

    #[test]
    fn domain_observations_ignore_unrelated_commits_and_wake_on_retirement() {
        let hub = SessionWatchHub::default();
        let mut tasks = hub.edited_domain("tasks");
        let notes = hub.edited_domain("notes");
        hub.committed(&SessionId::new("chat").unwrap());
        assert!(!tasks.has_changed());
        hub.domains_changed(BTreeSet::from([WatchKey::EditedDomain("tasks".into())]));
        assert!(tasks.has_changed());
        assert!(!notes.has_changed());
        tasks.mark_seen();
        hub.domains_retired();
        assert!(tasks.has_changed() && notes.has_changed());
        drop(tasks);
        drop(notes);
        assert!(hub.0.lock().unwrap().is_empty());
    }
}
