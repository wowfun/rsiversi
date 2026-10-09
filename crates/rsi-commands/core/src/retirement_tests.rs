use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
#[derive(Debug)]
struct Handler {
    state: Weak<State>,
    child: Mutex<Option<CommandLease>>,
    unlocked: Arc<AtomicBool>,
}
impl Drop for Handler {
    fn drop(&mut self) {
        let state = self.state.upgrade().unwrap();
        let unlocked = state.inner.try_lock().is_ok();
        self.unlocked.store(unlocked, Ordering::SeqCst);
        if unlocked {
            drop(self.child.get_mut().unwrap().take());
        } else {
            std::mem::forget(self.child.get_mut().unwrap().take());
        }
    }
}
#[async_trait]
impl rsi_commands_protocol::CommandHandler for Handler {
    async fn execute(&self, _: String, _: CancellationToken) -> Result<CommandResult> {
        unreachable!()
    }
}
#[test]
fn handler_destruction_can_withdraw_a_dependent_registration() {
    let state = Arc::new(State {
        inner: Mutex::new(Inner::default()),
    });
    let registry = Registry {
        state: state.clone(),
    };
    let unlocked = Arc::new(AtomicBool::new(false));
    let handler = |child| {
        Arc::new(Handler {
            state: Arc::downgrade(&state),
            child: Mutex::new(child),
            unlocked: unlocked.clone(),
        })
    };
    let child = registry
        .register(CommandDefinition {
            name: "child".into(),
            description: "child".into(),
            handler: handler(None),
        })
        .unwrap();
    let parent = registry
        .register(CommandDefinition {
            name: "parent".into(),
            description: "parent".into(),
            handler: handler(Some(child)),
        })
        .unwrap();
    drop(parent);
    assert!(unlocked.load(Ordering::SeqCst));
    assert!(registry.descriptors().is_empty());
}
