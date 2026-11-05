use super::{Arc, BTreeMap, Mutex, SessionId};
type Lane = Arc<tokio::sync::Mutex<()>>;

pub(super) fn acquire(
    lanes: &Mutex<BTreeMap<SessionId, Lane>>,
    session: &SessionId,
) -> Option<Lane> {
    let mut lanes = lanes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // An outstanding clone pins both held and waiting operations, even before lock().
    lanes.retain(|_, lane| Arc::strong_count(lane) > 1);
    if lanes.len() >= 256 && !lanes.contains_key(session) {
        return None;
    }
    Some(lanes.entry(session.clone()).or_default().clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capacity_counts_owned_lanes_and_never_splits_waiters() {
        let lanes = Mutex::new(BTreeMap::new());
        let id = |n| SessionId::new(format!("session-{n}")).unwrap();
        let mut held: Vec<_> = (0..256).map(|n| acquire(&lanes, &id(n)).unwrap()).collect();
        assert!(acquire(&lanes, &id(256)).is_none());
        let same = acquire(&lanes, &id(0)).unwrap();
        assert!(Arc::ptr_eq(&same, &held[0]));
        held.pop();
        let next = acquire(&lanes, &id(256)).unwrap();
        assert!(acquire(&lanes, &id(257)).is_none());
        drop((held, next));
        for n in 257..1024 {
            assert!(acquire(&lanes, &id(n)).is_some());
        }
        assert!(Arc::ptr_eq(&same, &acquire(&lanes, &id(0)).unwrap()));
    }
}
