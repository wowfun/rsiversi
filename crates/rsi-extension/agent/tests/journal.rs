use rsi_agent_composition_protocol::DomainCatalog;
use rsi_agent_session_protocol::{
    AgentControlRecord, AgentControlRecordBody, DomainMutationSource, DomainRequestId,
    DomainRevision, DomainStateCommit, DomainStateUpdate, DomainStateValue, SessionId, TurnId,
};
use rsi_agent_testkit::MemoryStore;
use rsi_agent_turn_protocol::DomainMutationReceipt;
use rsi_extension_agent::{definition, prepare, restore};
use serde_json::json;

#[tokio::test]
async fn staged_segment_restores_exactly_and_receipt_must_bind_its_content() {
    let store = MemoryStore::default();
    let definition = definition().unwrap();
    let catalog = DomainCatalog::new([definition.registration()]).unwrap();
    let handle = catalog.bind(&definition).unwrap();
    let initial = handle.decode(&catalog.baseline()[0]).unwrap();
    let events = vec![json!({"seq":0,"type":"fixture/event","data":{"text":"中文"}})];
    let prepared = prepare(
        &store,
        &handle,
        DomainRevision::new(1),
        &initial,
        events.clone(),
        DomainStateValue::new(json!({"pending":false})).unwrap(),
        rsi_api_protocol::ByteBudget::new(1024 * 1024)
            .unwrap()
            .into(),
    )
    .await
    .unwrap();
    let state = handle.decode(prepared.proposal().snapshot()).unwrap();
    let budget = rsi_api_protocol::ByteBudget::new(1024 * 1024).unwrap();
    let tiny = rsi_api_protocol::ByteBudget::new(1).unwrap();
    assert!(matches!(
        restore(&store, &state, || tiny.clone().into()).await,
        Err(rsi_extension_agent::Error::Capacity)
    ));
    assert_eq!(
        restore(&store, &state, || budget.clone().into())
            .await
            .unwrap(),
        events
    );
    let commit = DomainStateCommit::new(
        Some(DomainRequestId::new("operation").unwrap()),
        DomainMutationSource::Turn {
            turn_id: TurnId::new("turn").unwrap(),
        },
        vec![
            DomainStateUpdate::new(
                DomainRevision::new(1),
                prepared.proposal().snapshot().clone(),
            )
            .unwrap(),
        ],
    )
    .unwrap();
    let receipt = DomainMutationReceipt::new(
        SessionId::new("session").unwrap(),
        AgentControlRecord::new(
            1,
            1,
            AgentControlRecordBody::DomainStateCommitted { commit },
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(prepared.verify_receipt(&receipt).unwrap().get(), 2);
    let other = prepare(
        &store,
        &handle,
        DomainRevision::new(1),
        &initial,
        vec![json!({"different":true})],
        DomainStateValue::new(json!({})).unwrap(),
        budget.clone().into(),
    )
    .await
    .unwrap();
    assert!(other.verify_receipt(&receipt).is_err());
    let mut wrong_count = state;
    wrong_count.segments[0].count = 2;
    assert!(
        restore(&store, &wrong_count, || budget.clone().into())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn rejected_preparation_preserves_the_prior_snapshot() {
    let store = MemoryStore::default();
    let definition = definition().unwrap();
    let catalog = DomainCatalog::new([definition.registration()]).unwrap();
    let handle = catalog.bind(&definition).unwrap();
    let mut initial = handle.decode(&catalog.baseline()[0]).unwrap();
    initial
        .segments
        .push(rsi_agent_session_protocol::ExtensionSegment {
            first: 0,
            count: 1,
            sha256: "a".repeat(64),
            byte_len: rsi_agent_session_protocol::MAXIMUM_EXTENSION_JOURNAL_BYTES as u64,
        });
    initial.validate().unwrap();
    let before = initial.clone();
    assert!(
        prepare(
            &store,
            &handle,
            DomainRevision::new(1),
            &initial,
            vec![json!(null)],
            DomainStateValue::new(json!(null)).unwrap(),
            rsi_api_protocol::ByteBudget::new(1024 * 1024)
                .unwrap()
                .into(),
        )
        .await
        .is_err()
    );
    assert_eq!(initial, before);
}

#[tokio::test]
async fn large_event_uses_its_own_cas_segment_and_small_successors_still_pack() {
    let store = MemoryStore::default();
    let definition = definition().unwrap();
    let catalog = DomainCatalog::new([definition.registration()]).unwrap();
    let handle = catalog.bind(&definition).unwrap();
    let initial = handle.decode(&catalog.baseline()[0]).unwrap();
    let mut expected = vec![
        json!({"seq":0,"data":"before"}),
        json!({"seq":1,"data":"x".repeat(3 * 1024 * 1024)}),
        json!({"seq":2,"data":"after"}),
    ];
    let budget = rsi_api_protocol::ByteBudget::new(4 * 1024 * 1024).unwrap();
    let first = prepare(
        &store,
        &handle,
        DomainRevision::new(0),
        &initial,
        expected.clone(),
        DomainStateValue::new(json!(null)).unwrap(),
        budget.clone().into(),
    )
    .await
    .unwrap();
    let journal = handle.decode(first.proposal().snapshot()).unwrap();
    assert_eq!(journal.segments.len(), 3);
    assert_eq!(
        journal
            .segments
            .iter()
            .map(|segment| (segment.first, segment.count))
            .collect::<Vec<_>>(),
        [(0, 1), (1, 1), (2, 1)]
    );
    assert!(journal.segments[1].byte_len > 3 * 1024 * 1024);
    assert!(first.proposal().snapshot().state().encoded_len() < 4096);
    let suffix = vec![json!({"seq":3}), json!({"seq":4})];
    expected.extend(suffix.clone());
    let next = prepare(
        &store,
        &handle,
        DomainRevision::new(1),
        &journal,
        suffix,
        DomainStateValue::new(json!(null)).unwrap(),
        budget.clone().into(),
    )
    .await
    .unwrap();
    let next_journal = handle.decode(next.proposal().snapshot()).unwrap();
    assert_eq!(next_journal.segments.len(), 3);
    assert_eq!(next_journal.segments[..2], journal.segments[..2]);
    assert_eq!(next_journal.segments[2].count, 3);
    assert_eq!(
        restore(&store, &next_journal, || budget.clone().into())
            .await
            .unwrap(),
        expected
    );
}

#[tokio::test]
async fn small_mutations_pack_without_losing_history_beyond_the_segment_count() {
    let store = MemoryStore::default();
    let definition = definition().unwrap();
    let catalog = DomainCatalog::new([definition.registration()]).unwrap();
    let handle = catalog.bind(&definition).unwrap();
    let mut journal = handle.decode(&catalog.baseline()[0]).unwrap();
    let budget = rsi_api_protocol::ByteBudget::new(1024 * 1024).unwrap();
    let mut owner = None;
    let mut expected = Vec::new();
    for seq in 0..300 {
        let event = json!({"seq":seq,"type":"fixture/event","data":{"text":"保留"}});
        expected.push(event.clone());
        let prepared = prepare(
            &store,
            &handle,
            DomainRevision::new(seq),
            &journal,
            vec![event],
            DomainStateValue::new(json!({"cursor":seq})).unwrap(),
            budget.clone().into(),
        )
        .await
        .unwrap();
        journal = handle.decode(prepared.proposal().snapshot()).unwrap();
        owner = Some(prepared);
    }
    assert_eq!(journal.segments.len(), 1);
    assert_eq!(journal.segments[0].count, 300);
    assert_eq!(
        restore(&store, &journal, || budget.clone().into())
            .await
            .unwrap(),
        expected
    );
    drop(owner);
}

#[tokio::test]
async fn packed_suffix_obeys_event_and_read_budgets_without_changing_the_prefix() {
    let store = MemoryStore::default();
    let definition = definition().unwrap();
    let catalog = DomainCatalog::new([definition.registration()]).unwrap();
    let handle = catalog.bind(&definition).unwrap();
    let empty = handle.decode(&catalog.baseline()[0]).unwrap();
    let budget = rsi_api_protocol::ByteBudget::new(1024 * 1024).unwrap();
    let checkpoint = DomainStateValue::new(json!(null)).unwrap();
    let full = prepare(
        &store,
        &handle,
        DomainRevision::new(0),
        &empty,
        (0..4096).map(|seq| json!({"seq":seq})).collect(),
        checkpoint.clone(),
        budget.clone().into(),
    )
    .await
    .unwrap();
    let prefix = handle.decode(full.proposal().snapshot()).unwrap();
    let next = prepare(
        &store,
        &handle,
        DomainRevision::new(1),
        &prefix,
        vec![json!({"seq":4096})],
        checkpoint.clone(),
        budget.clone().into(),
    )
    .await
    .unwrap();
    let journal = handle.decode(next.proposal().snapshot()).unwrap();
    assert_eq!(journal.segments.len(), 2);
    assert_eq!(journal.segments[0], prefix.segments[0]);
    let too_small = rsi_api_protocol::ByteBudget::new(1).unwrap();
    assert!(
        prepare(
            &store,
            &handle,
            DomainRevision::new(2),
            &journal,
            vec![json!({"seq":4097})],
            checkpoint,
            too_small.into()
        )
        .await
        .is_err()
    );
    let restored = restore(&store, &journal, || budget.clone().into())
        .await
        .unwrap();
    assert_eq!(restored.len(), 4097);
    assert_eq!(restored.last().unwrap(), &json!({"seq":4096}));
}
