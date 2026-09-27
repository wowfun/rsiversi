use super::*;

#[test]
fn discovery_pages_are_bounded_complete_and_bound_to_one_capture() {
    let factory = Arc::new(CounterFactory::default());
    let mut builder = StandardAddonBuilder::new("fixture.discovery");
    for index in 0..130 {
        let name = format!("fixture.discovery.{index:03}");
        builder
            .register_factory(
                AddonScope::Service,
                &name,
                "1",
                UpdateMode::Replayable,
                factory.clone(),
            )
            .unwrap();
        builder
            .describe_factory(
                &name,
                "read only",
                Some(json!({"description":"a".repeat(3000)})),
            )
            .unwrap();
    }
    builder.export_domain::<Counter>().unwrap();
    let addons = StandardAddonSet::new([builder.build().unwrap()]).unwrap();
    let snapshot = addons.discovery(None).unwrap();
    let other = addons.discovery(None).unwrap();
    assert!(snapshot.get("factory:fixture.discovery.129").is_some());
    assert!(snapshot.get("contract:fixture.addon.counter").is_some());
    assert!(snapshot.get("factory:missing").is_none());
    let mut cursor = None;
    let mut keys = Vec::new();
    loop {
        let page = snapshot.page(cursor.as_ref()).unwrap();
        assert!(page.entries.len() <= rsi::MAXIMUM_ADDON_DISCOVERY_ITEMS);
        assert!(
            serde_json::to_vec(&page.entries).unwrap().len() <= rsi::MAXIMUM_ADDON_DISCOVERY_BYTES
        );
        keys.extend(page.entries.iter().map(|(key, _)| key.clone()));
        if let Some(next) = &page.next {
            assert!(other.page(Some(next)).is_err());
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(keys.len(), 131);
    assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(factory.prepared.load(Ordering::SeqCst), 0);
    assert_eq!(factory.live.load(Ordering::SeqCst), 0);
}

#[test]
fn discovery_byte_limit_shortens_pages_without_skipping_large_descriptions() {
    let factory = Arc::new(CounterFactory::default());
    let mut builder = StandardAddonBuilder::new("fixture.discovery");
    for index in 0..6 {
        let name = format!("fixture.large.{index}");
        builder
            .register_factory(
                AddonScope::Application,
                &name,
                "1",
                UpdateMode::Replayable,
                factory.clone(),
            )
            .unwrap();
        builder
            .describe_factory(
                &name,
                "read only",
                Some(json!({"description":"a".repeat(60 * 1024)})),
            )
            .unwrap();
    }
    let snapshot = StandardAddonSet::new([builder.build().unwrap()])
        .unwrap()
        .discovery(None)
        .unwrap();
    let first = snapshot.page(None).unwrap();
    assert_eq!(first.entries.len(), 4);
    let second = snapshot.page(first.next.as_ref()).unwrap();
    assert_eq!(second.entries.len(), 2);
    assert!(second.next.is_none());
    assert_eq!(factory.prepared.load(Ordering::SeqCst), 0);
}
