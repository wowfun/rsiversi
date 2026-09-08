use futures_util::StreamExt as _;
use rsi_user_questions_protocol::PendingChanges;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::watch;

const MAXIMUM_SUBSCRIBERS: usize = 1024;
const MAXIMUM_SESSIONS: usize = 256;
type Subscribers = BTreeMap<String, Vec<Weak<watch::Sender<u64>>>>;

#[derive(Clone, Debug, Default)]
pub(super) struct Changes(Arc<Mutex<Registry>>);
#[derive(Debug, Default)]
struct Registry {
    sessions: Subscribers,
    subscribers: usize,
}

struct Registration {
    hub: Changes,
    sessions: BTreeSet<String>,
    sender: Arc<watch::Sender<u64>>,
}
impl Drop for Registration {
    fn drop(&mut self) {
        let mut registry = self
            .hub
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let identity = Arc::downgrade(&self.sender);
        for session in &self.sessions {
            if let Some(subscribers) = registry.sessions.get_mut(session) {
                subscribers.retain(|subscriber| !subscriber.ptr_eq(&identity));
                if subscribers.is_empty() {
                    registry.sessions.remove(session);
                }
            }
        }
        registry.subscribers -= 1;
    }
}

impl Changes {
    pub(super) fn subscribe(&self, sessions: &[String]) -> Option<PendingChanges> {
        if sessions.len() > MAXIMUM_SESSIONS {
            return None;
        }
        let sessions = sessions.iter().cloned().collect::<BTreeSet<_>>();
        let (sender, receiver) = watch::channel(0_u64);
        let sender = Arc::new(sender);
        let mut registry = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if registry.subscribers == MAXIMUM_SUBSCRIBERS {
            return None;
        }
        for session in &sessions {
            registry
                .sessions
                .entry(session.clone())
                .or_default()
                .push(Arc::downgrade(&sender));
        }
        registry.subscribers += 1;
        drop(registry);
        let registration = Registration {
            hub: self.clone(),
            sessions,
            sender,
        };
        Some(
            futures_util::stream::unfold(
                (receiver, registration),
                |(mut receiver, registration)| async move {
                    receiver.changed().await.ok()?;
                    Some(((), (receiver, registration)))
                },
            )
            .boxed(),
        )
    }
    pub(super) fn notify(&self, session: &str) {
        let registry = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(subscribers) = registry.sessions.get(session) {
            for sender in subscribers.iter().filter_map(Weak::upgrade) {
                sender.send_modify(|revision| *revision = revision.wrapping_add(1));
            }
        }
    }
    pub(super) fn notify_all(&self) {
        let registry = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for subscribers in registry.sessions.values() {
            for sender in subscribers.iter().filter_map(Weak::upgrade) {
                sender.send_modify(|revision| *revision = revision.wrapping_add(1));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Changes, MAXIMUM_SUBSCRIBERS};
    use futures_util::{FutureExt as _, StreamExt as _};

    #[tokio::test]
    async fn scoped_changes_coalesce_and_registration_drop_reclaims_keys() {
        let hub = Changes::default();
        let mut first = hub.subscribe(&["first".into()]).unwrap();
        let mut both = hub.subscribe(&["first".into(), "second".into()]).unwrap();
        hub.notify("unrelated");
        assert!(first.next().now_or_never().is_none());
        hub.notify("second");
        hub.notify("second");
        assert_eq!(both.next().now_or_never(), Some(Some(())));
        assert!(both.next().now_or_never().is_none());
        assert!(first.next().now_or_never().is_none());
        drop(both);
        assert_eq!(hub.0.lock().unwrap().sessions.len(), 1);
        drop(first);
        let registry = hub.0.lock().unwrap();
        assert_eq!(registry.subscribers, 0);
        assert!(registry.sessions.is_empty());
    }

    #[test]
    fn subscriptions_have_atomic_capacity_and_reuse_it_after_drop() {
        let hub = Changes::default();
        let mut subscriptions = (0..MAXIMUM_SUBSCRIBERS)
            .map(|_| hub.subscribe(&["session".into()]).unwrap())
            .collect::<Vec<_>>();
        assert!(hub.subscribe(&["other".into()]).is_none());
        assert_eq!(hub.0.lock().unwrap().sessions.len(), 1);
        subscriptions.pop();
        let replacement = hub.subscribe(&["other".into()]).unwrap();
        drop(subscriptions);
        drop(replacement);
        assert!(hub.0.lock().unwrap().sessions.is_empty());
    }

    #[tokio::test]
    async fn empty_selection_is_idle_and_drop_releases_its_subscriber_slot() {
        let hub = Changes::default();
        for _ in 0..=MAXIMUM_SUBSCRIBERS {
            let mut subscription = hub.subscribe(&[]).unwrap();
            hub.notify_all();
            assert!(subscription.next().now_or_never().is_none());
            assert_eq!(hub.0.lock().unwrap().subscribers, 1);
            drop(subscription);
            assert_eq!(hub.0.lock().unwrap().subscribers, 0);
            assert!(hub.0.lock().unwrap().sessions.is_empty());
        }
    }
}
