use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
#[derive(Debug)]
struct Unit {
    state: Weak<State>,
    child: Mutex<Option<ProjectionLease>>,
    unlocked: Arc<AtomicBool>,
}
impl Drop for Unit {
    fn drop(&mut self) {
        let state = self.state.upgrade().unwrap();
        let unlocked = state.units.try_lock().is_ok();
        self.unlocked.store(unlocked, Ordering::SeqCst);
        if unlocked {
            drop(self.child.get_mut().unwrap().take());
        } else {
            std::mem::forget(self.child.get_mut().unwrap().take());
        }
    }
}
impl ProjectionUnit for Unit {
    fn project(&self, _: &Value) -> Result<Value> {
        unreachable!()
    }
}
#[test]
fn unit_destruction_can_withdraw_a_dependent_registration() {
    let state = Arc::new(State {
        units: Mutex::new(BTreeMap::new()),
    });
    let registry = Registry {
        state: state.clone(),
    };
    let unlocked = Arc::new(AtomicBool::new(false));
    let unit = |child| {
        Arc::new(Unit {
            state: Arc::downgrade(&state),
            child: Mutex::new(child),
            unlocked: unlocked.clone(),
        })
    };
    let child = registry.register("child", unit(None)).unwrap();
    let parent = registry.register("parent", unit(Some(child))).unwrap();
    drop(parent);
    assert!(unlocked.load(Ordering::SeqCst));
    assert!(state.units.lock().unwrap().is_empty());
}
