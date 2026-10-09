use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
#[derive(Debug)]
struct Backend {
    hub: Weak<HubState>,
    child: Mutex<Option<BackendLease>>,
    unlocked: Arc<AtomicBool>,
}
impl Drop for Backend {
    fn drop(&mut self) {
        let hub = self.hub.upgrade().unwrap();
        let unlocked = hub.inner.try_lock().is_ok();
        self.unlocked.store(unlocked, Ordering::SeqCst);
        if unlocked {
            drop(self.child.get_mut().unwrap().take());
        } else {
            std::mem::forget(self.child.get_mut().unwrap().take());
        }
    }
}
#[async_trait]
impl KvBackend for Backend {
    fn ensure_available(&self) -> Result<()> {
        Ok(())
    }
    async fn load(&self, _: &str) -> Result<Option<StoredDomain>> {
        unreachable!()
    }
    async fn put(&self, _: &str, _: u32, _: &str, _: &Value) -> Result<()> {
        unreachable!()
    }
    async fn delete(&self, _: &str, _: u32, _: &str) -> Result<()> {
        unreachable!()
    }
}
#[test]
fn backend_destruction_can_withdraw_a_dependent_registration() {
    let hub = Hub::new();
    let unlocked = Arc::new(AtomicBool::new(false));
    let backend = |child| {
        Arc::new(Backend {
            hub: Arc::downgrade(&hub.state),
            child: Mutex::new(child),
            unlocked: unlocked.clone(),
        })
    };
    let child = hub.register("child", backend(None)).unwrap();
    let parent = hub.register("parent", backend(Some(child))).unwrap();
    drop(parent);
    assert!(unlocked.load(Ordering::SeqCst));
    assert!(hub.state.inner.lock().unwrap().backends.is_empty());
}
