use super::*;

#[tokio::test]
async fn membership_cache_reuses_arcs_and_keeps_captured_order_after_reorder_and_removal() {
    let runtime = Runtime::default();
    let root = runtime.root();
    let a = root.child_position().unwrap();
    let b = root.child_position().unwrap();
    let mut listeners = LocalEventListeners::default();
    let first = Arc::new(LocalEventBinding::new(
        EventListenerId(1),
        Arc::new(()),
        false,
        Arc::new(|| true),
    ));
    let second = Arc::new(LocalEventBinding::new(
        EventListenerId(2),
        Arc::new(()),
        false,
        Arc::new(|| true),
    ));
    listeners.insert(EventListenerId(2), second.clone(), b.clone(), false);
    listeners.insert(EventListenerId(1), first.clone(), a.clone(), false);
    let capture = |listeners: &mut LocalEventListeners| {
        runtime
            .inner
            .composition_order
            .snapshot(|order| listeners.snapshot(&runtime, order))
    };
    let before = capture(&mut listeners);
    assert!(Arc::ptr_eq(&before.bindings[0], &first));
    for _ in 0..1000 {
        assert!(Arc::ptr_eq(&before, &capture(&mut listeners)));
    }
    let _unrelated = root.child_position().unwrap();
    assert!(Arc::ptr_eq(&before, &capture(&mut listeners)));
    root.reorder_children(&[b, a]).unwrap();
    let reordered = capture(&mut listeners);
    assert!(!Arc::ptr_eq(&before, &reordered));
    assert!(Arc::ptr_eq(&reordered.bindings[0], &second));
    let removed = listeners.remove(EventListenerId(1));
    drop(removed);
    let after = capture(&mut listeners);
    assert_eq!(after.bindings.len(), 1);
    assert_eq!(before.bindings.len(), 2);
    assert!(Arc::ptr_eq(&before.bindings[0], &first));
}
