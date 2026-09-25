use crate::{Error, Result, State};
use rsi_acp_journal::{Capabilities, Completion, Snapshot, Status};
use std::sync::{Arc, atomic::Ordering};

pub(super) enum Transition {
    Status(Status),
    Disconnected,
    Complete(Completion),
    BeginReplay,
    FinishReplay(bool),
    Bind(String, Capabilities),
}

fn unfinished(status: Status) -> bool {
    matches!(
        status,
        Status::Starting | Status::Ready | Status::Loading | Status::Running
    )
}

#[cfg(test)]
pub(super) struct PublicationHook {
    pub committed: tokio::sync::oneshot::Sender<()>,
    pub publish: tokio::sync::oneshot::Receiver<()>,
}

impl State {
    pub(super) async fn transition(self: &Arc<Self>, change: Transition) -> Result<Snapshot> {
        let owner = self.clone();
        // Journal's admitted blocking worker survives waiter cancellation. This
        // owner must therefore retain the gate through both commit and publication.
        self.tasks
            .spawn(async move {
                let _gate = owner.transition.lock().await;
                owner.publish(change).await
            })
            .await
            .map_err(|_| Error::Journal)?
    }

    async fn publish(&self, change: Transition) -> Result<Snapshot> {
        let setup = matches!(change, Transition::Bind(..) | Transition::BeginReplay);
        if setup || matches!(change, Transition::Status(Status::Running)) {
            self.accepting()?;
        }
        let current = self.snapshot();
        let journal = &self.journal;
        let id = &self.id;
        let generation = self.generation;
        let mut snapshot = match change {
            Transition::Status(Status::Unknown) | Transition::Disconnected
                if !unfinished(current.status) =>
            {
                return Ok(current);
            }
            Transition::Status(Status::Unknown) | Transition::Disconnected
                if current.status == Status::Loading =>
            {
                journal.finish_replay(id, generation, false).await
            }
            Transition::Status(status) => journal.settle(id, generation, status).await,
            Transition::Disconnected => journal.settle(id, generation, Status::Unknown).await,
            Transition::Complete(completion) => journal.complete(id, generation, completion).await,
            Transition::BeginReplay => journal.begin_replay(id, generation).await,
            Transition::FinishReplay(success) => {
                journal.finish_replay(id, generation, success).await
            }
            Transition::Bind(target, capabilities) => {
                journal.bind(id, generation, target, capabilities).await
            }
        }
        .map_err(|_| Error::Journal)?;
        #[cfg(test)]
        {
            let hook = self.publication_hook.lock().unwrap().take();
            if let Some(hook) = hook {
                let _ = hook.committed.send(());
                let _ = hook.publish.await;
            }
        }
        let lost_setup = setup && self.accepting().is_err();
        if lost_setup {
            snapshot = if snapshot.status == Status::Loading {
                journal.finish_replay(id, generation, false).await
            } else {
                journal.settle(id, generation, Status::Unknown).await
            }
            .map_err(|_| Error::Journal)?;
        }
        *self.snapshot.lock().expect("ACP client snapshot") = snapshot.clone();
        if setup && snapshot.status == Status::Ready && !lost_setup {
            self.initialized.store(true, Ordering::Release);
        }
        self.changed();
        if lost_setup {
            Err(Error::Unknown)
        } else {
            Ok(snapshot)
        }
    }
}
