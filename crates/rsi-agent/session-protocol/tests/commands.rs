use rsi_agent_session_protocol::{
    CommandArguments, CommandRevision, ContributionId, DomainIdentity, DomainMutationSource,
    DomainRequestId, DomainRevision, DomainSnapshot, DomainStateCommit, DomainStateUpdate,
    DomainStateValue, MAXIMUM_COMMAND_ARGUMENT_BYTES, SessionCommandInvocation,
};

fn invocation() -> SessionCommandInvocation {
    SessionCommandInvocation {
        command: ContributionId::new("plan.command").unwrap(),
        request_id: DomainRequestId::new("request").unwrap(),
        expected_revision: CommandRevision::Durable { control_seq: 3 },
        arguments: CommandArguments::new(serde_json::json!({"enabled":true})).unwrap(),
    }
}

fn commit(
    input: SessionCommandInvocation,
) -> rsi_agent_session_protocol::Result<DomainStateCommit> {
    DomainStateCommit::new(
        Some(DomainRequestId::new("request").unwrap()),
        DomainMutationSource::Command { invocation: input },
        vec![
            DomainStateUpdate::new(
                DomainRevision::new(1),
                DomainSnapshot::new(
                    DomainIdentity::new("plan", 1).unwrap(),
                    DomainStateValue::new(true.into()).unwrap(),
                ),
            )
            .unwrap(),
        ],
    )
}

#[test]
fn canonical_receipt_binds_invocation_even_when_replacements_are_identical() {
    let original = commit(invocation()).unwrap();
    let mut changed = invocation();
    changed.arguments = CommandArguments::new(serde_json::json!({"enabled":false})).unwrap();
    assert_ne!(
        original.request_sha256(),
        commit(changed).unwrap().request_sha256()
    );
    let mut changed = invocation();
    changed.expected_revision = CommandRevision::Durable { control_seq: 4 };
    assert_ne!(
        original.request_sha256(),
        commit(changed).unwrap().request_sha256()
    );
    let mut wire = serde_json::to_value(&original).unwrap();
    wire["source"]["invocation"]["arguments"] = serde_json::json!({"enabled":false});
    assert!(serde_json::from_value::<DomainStateCommit>(wire).is_err());
    let wire = serde_json::to_vec(&original).unwrap();
    assert_eq!(
        serde_json::from_slice::<DomainStateCommit>(&wire).unwrap(),
        original
    );
}

#[test]
fn durable_commands_reject_draft_revisions_and_different_request_ids() {
    let mut input = invocation();
    input.expected_revision = CommandRevision::Draft { revision: 0 };
    assert!(commit(input).is_err());
    let mut input = invocation();
    input.request_id = DomainRequestId::new("other").unwrap();
    assert!(commit(input).is_err());
}

#[test]
fn command_arguments_and_invocations_are_closed_and_bounded_on_decode() {
    let large = serde_json::json!("x".repeat(MAXIMUM_COMMAND_ARGUMENT_BYTES));
    assert!(CommandArguments::new(large.clone()).is_err());
    assert!(serde_json::from_value::<CommandArguments>(large).is_err());
    let mut wire = serde_json::to_value(invocation()).unwrap();
    wire["authority"] = serde_json::json!("forged");
    assert!(serde_json::from_value::<SessionCommandInvocation>(wire).is_err());
}

#[test]
fn compact_receipts_bind_the_original_invocation_and_actual_mutation_boundary() {
    use rsi_agent_session_protocol::{CommandOutcome, SessionCommandReceipt};
    let mut input = invocation();
    input.expected_revision = CommandRevision::Draft { revision: 7 };
    let draft = SessionCommandReceipt::draft_changed(&input, "a".repeat(64)).unwrap();
    assert_eq!(
        draft.outcome(),
        CommandOutcome::DraftChanged { revision: 8 }
    );
    assert_eq!(draft.invocation_sha256(), input.digest().unwrap());
    assert_eq!(draft.state_sha256(), "a".repeat(64));
    let canonical = commit(invocation()).unwrap();
    let durable = SessionCommandReceipt::committed(4, &canonical).unwrap();
    assert_eq!(
        durable.outcome(),
        CommandOutcome::Committed { control_seq: 4 }
    );
    assert_eq!(durable.state_sha256(), canonical.request_sha256());
    assert!(SessionCommandReceipt::committed(5, &canonical).is_err());
    assert!(SessionCommandReceipt::draft_changed(&invocation(), "a".repeat(64)).is_err());
    for receipt in [draft, durable] {
        let wire = serde_json::to_value(&receipt).unwrap();
        assert_eq!(
            serde_json::from_value::<SessionCommandReceipt>(wire.clone()).unwrap(),
            receipt
        );
        let mut corrupt = wire.clone();
        corrupt["state_sha256"] = serde_json::json!("bad");
        assert!(serde_json::from_value::<SessionCommandReceipt>(corrupt).is_err());
        let mut zero = wire;
        zero["outcome"] = serde_json::json!({"kind":"draft_changed","revision":0});
        assert!(serde_json::from_value::<SessionCommandReceipt>(zero).is_err());
    }
}

#[test]
fn discovery_decode_rejects_duplicate_names_and_unbounded_metadata() {
    use rsi_agent_session_protocol::{SessionCommandDescriptor, SessionCommandsView};
    let first = SessionCommandDescriptor::new(
        ContributionId::new("first").unwrap(),
        "plan",
        "Plan mode",
        true,
    )
    .unwrap();
    let second = SessionCommandDescriptor::new(
        ContributionId::new("second").unwrap(),
        "plan",
        "Another plan",
        false,
    )
    .unwrap();
    assert!(
        SessionCommandsView::new(
            CommandRevision::Draft { revision: 0 },
            vec![first.clone(), second.clone()]
        )
        .is_err()
    );
    let wire =
        serde_json::json!({"revision":{"kind":"draft","revision":0},"commands":[first,second]});
    assert!(serde_json::from_value::<SessionCommandsView>(wire).is_err());
    assert!(
        SessionCommandDescriptor::new(
            ContributionId::new("first").unwrap(),
            "plan",
            "x".repeat(4097),
            true
        )
        .is_err()
    );
    assert!(
        SessionCommandDescriptor::new(
            ContributionId::new("first").unwrap(),
            "/plan",
            "Plan mode",
            true
        )
        .is_err()
    );
}
