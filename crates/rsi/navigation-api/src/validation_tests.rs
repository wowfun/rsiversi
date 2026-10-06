use super::*;
#[test]
fn operation_envelopes_admit_maximal_identifiers_and_escaped_small_requests() {
    use serde_json::json;
    let session = SessionId::new("s".repeat(256)).unwrap();
    let filter = NavigationFilter {
        query: "\"".repeat(128),
        archived: false,
        workspace: WorkspaceFilter::Registered {
            id: WorkspaceId::parse("a".repeat(64)).unwrap(),
        },
    };
    filter.validate().unwrap();
    let cursor = NavigationCursor {
        filter: filter.clone(),
        host_epoch: HostEpoch::from_bytes([1; 16]),
        metadata_revision: u64::MAX.to_string(),
        token: "a".repeat(32),
        after: Some(ActivityCursor {
            last_activity_ms: u64::MAX,
            session_id: session.clone(),
        }),
    };
    let metadata = SessionMetadata {
        title: Some("\"".repeat(256)),
        ..Default::default()
    };
    metadata.validate().unwrap();
    let summaries = SummaryRequest {
        sessions: (0..64)
            .map(|i| SessionId::new(format!("{i:02}{}", "s".repeat(254))).unwrap())
            .collect(),
        metadata_revision: u64::MAX.to_string(),
    };
    summaries.validate().unwrap();
    let coordinates = rsi_agent_session_protocol::ExecutionCoordinates::new(
        rsi_agent_session_protocol::ExecutionLocation::Local,
        format!("/{}", "\"".repeat(16 * 1024 - 1)),
    )
    .unwrap();
    let order = serde_json::to_value(OrderScope::Coordinates { coordinates }).unwrap();
    let summaries = serde_json::to_value(summaries).unwrap();
    for (operation, value) in [
        (
            NavigationOperation::Query,
            json!({"filter":filter,"after":cursor}),
        ),
        (NavigationOperation::Pinned, json!(filter)),
        (
            NavigationOperation::Replace,
            json!({"session":session,"expected_revision":u64::MAX.to_string(),"metadata":metadata}),
        ),
        (NavigationOperation::Summaries, summaries.clone()),
        (NavigationOperation::OrderSeed, order.clone()),
    ] {
        assert!(
            rsi_api_protocol::measure_json(&value, operation.spec().maximum_request_bytes).is_ok(),
            "{operation:?}"
        );
    }
    assert!(
        rsi_api_protocol::measure_json(
            &summaries,
            NavigationOperation::Query.spec().maximum_request_bytes
        )
        .is_err()
    );
    assert!(
        rsi_api_protocol::measure_json(
            &order,
            NavigationOperation::Replace.spec().maximum_request_bytes
        )
        .is_err()
    );
}
fn fixture() -> (NavigationPage, NavigationFilter, HostEpoch) {
    let id = SessionId::new("session").unwrap();
    let key = ActivityCursor {
        last_activity_ms: 2,
        session_id: id.clone(),
    };
    (
        NavigationPage {
            metadata_revision: "1".into(),
            newest: Some(key),
            entries: vec![NavigationEntry {
                session: id,
                created_at_ms: "1".into(),
                last_activity_ms: "2".into(),
                location: rsi_agent_session_protocol::ExecutionLocation::Local,
                path: "/project".into(),
                workspace: None,
                metadata: SessionMetadata::default(),
            }],
            scanned: 1,
            next: None,
        },
        NavigationFilter::default(),
        HostEpoch::from_bytes([1; 16]),
    )
}
#[test]
fn remote_page_rejects_wrong_scope_order_and_impossible_activity() {
    let (valid, filter, epoch) = fixture();
    validation::page(&valid, &filter, None, &epoch).unwrap();
    for bad in 0..7 {
        let mut page = valid.clone();
        match bad {
            0 => page.scanned = 257,
            1 => page.entries[0].last_activity_ms = "0".into(),
            2 => page.entries[0].created_at_ms = "01".into(),
            3 => page.entries[0].path = "relative".into(),
            4 => page.entries[0].workspace = Some(WorkspaceId::parse("a".repeat(64)).unwrap()),
            5 => {
                page.entries.push(page.entries[0].clone());
                page.scanned = 2;
            }
            6 => page.newest.as_mut().unwrap().last_activity_ms = 1,
            _ => unreachable!(),
        }
        assert!(
            validation::page(&page, &filter, None, &epoch).is_err(),
            "case {bad}"
        );
    }
    let cursor = NavigationCursor {
        filter: filter.clone(),
        host_epoch: epoch.clone(),
        metadata_revision: "1".into(),
        token: "b".repeat(32),
        after: valid.newest.clone(),
    };
    assert!(validation::page(&valid, &filter, Some(&cursor), &epoch).is_err());
    let mut wrong_search = filter.clone();
    wrong_search.query = "no match".into();
    assert!(validation::page(&valid, &wrong_search, None, &epoch).is_err());
}
#[test]
fn pins_validate_machine_paths_and_partition_order() {
    let (mut page, filter, _) = fixture();
    page.entries[0].metadata.pinned = true;
    let mut pins = PinnedPage {
        metadata_revision: "1".into(),
        entries: vec![PinnedEntry::Available {
            entry: page.entries.remove(0),
        }],
    };
    validation::pins(&pins, &filter).unwrap();
    let PinnedEntry::Available { entry } = &mut pins.entries[0] else {
        unreachable!()
    };
    entry.path = r"C:\project".into();
    validation::pins(&pins, &filter).unwrap();
    let PinnedEntry::Available { entry } = &mut pins.entries[0] else {
        unreachable!()
    };
    entry.location = rsi_agent_session_protocol::ExecutionLocation::Ssh {
        target: serde_json::from_value(serde_json::json!("a".repeat(32))).unwrap(),
    };
    assert!(validation::pins(&pins, &filter).is_err());
}

#[test]
fn opaque_continuations_preserve_visible_order_without_disclosing_hidden_newest_or_cuts() {
    let (mut page, filter, epoch) = fixture();
    page.newest = None;
    let previous = NavigationCursor {
        filter: filter.clone(),
        host_epoch: epoch.clone(),
        metadata_revision: "1".into(),
        token: "a".repeat(32),
        after: None,
    };
    page.next = Some(NavigationCursor {
        token: "b".repeat(32),
        after: Some(ActivityCursor {
            last_activity_ms: 2,
            session_id: SessionId::new("session").unwrap(),
        }),
        ..previous.clone()
    });
    validation::page(&page, &filter, Some(&previous), &epoch).unwrap();
    let mut empty = page.clone();
    empty.entries.clear();
    empty.next.as_mut().unwrap().after = None;
    validation::page(&empty, &filter, Some(&previous), &epoch).unwrap();
    empty.next.as_mut().unwrap().token = previous.token.clone();
    assert!(validation::page(&empty, &filter, Some(&previous), &epoch).is_err());
}
