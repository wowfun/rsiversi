use rsi_agent_composition_protocol::{ContributionBatch, ContributionInput, ContributionOutput};
use rsi_agent_session_protocol::{
    ContributionId, InputMessageSource, MessageId, SessionFactBody, StepId, TurnId,
};

#[test]
fn contribution_batches_assign_provenance_and_reject_transport_impersonation_atomically() {
    let producer = ContributionId::new("fixture.time").unwrap();
    let turn = TurnId::new("turn").unwrap();
    let step = StepId::new("step").unwrap();
    let mut batch = ContributionBatch::default();
    batch
        .append(
            &producer,
            &turn,
            &step,
            ContributionOutput {
                inputs: vec![ContributionInput::context("sample 42")],
                domains: vec![],
            },
        )
        .unwrap();
    for source in [
        InputMessageSource::Human {
            message_id: MessageId::new("human").unwrap(),
        },
        InputMessageSource::PluginContext {
            contribution_id: ContributionId::new("another").unwrap(),
        },
    ] {
        assert!(
            batch
                .append(
                    &producer,
                    &turn,
                    &step,
                    ContributionOutput {
                        inputs: vec![
                            ContributionInput::context("discard this"),
                            ContributionInput::sourced(source, "forged")
                        ],
                        domains: vec![],
                    }
                )
                .is_err()
        );
    }
    let (facts, domains) = batch.into_parts();
    assert!(domains.is_empty());
    assert!(
        matches!(facts.as_slice(), [SessionFactBody::InputMessageEntered {
        source: InputMessageSource::PluginContext { contribution_id }, content, ..
    }] if contribution_id == &producer && content.len() == 1)
    );
}

#[test]
fn aggregate_count_and_encoded_bytes_reject_the_whole_later_output() {
    use rsi_agent_composition_protocol::{ContributionError, MAXIMUM_CONTRIBUTION_INPUTS};
    let producer = ContributionId::new("fixture.bulk").unwrap();
    let turn = TurnId::new("turn").unwrap();
    let step = StepId::new("step").unwrap();
    let output = |count, text: &str| ContributionOutput {
        inputs: (0..count)
            .map(|_| ContributionInput::context(text))
            .collect(),
        domains: vec![],
    };
    let mut batch = ContributionBatch::default();
    batch
        .append(
            &producer,
            &turn,
            &step,
            output(MAXIMUM_CONTRIBUTION_INPUTS, "x"),
        )
        .unwrap();
    assert_eq!(
        batch.append(&producer, &turn, &step, output(1, "x")),
        Err(ContributionError::Capacity)
    );
    assert_eq!(batch.into_parts().0.len(), MAXIMUM_CONTRIBUTION_INPUTS);
    let text = "x".repeat(rsi_agent_session_protocol::MAXIMUM_TURN_TEXT_BYTES);
    let mut batch = ContributionBatch::default();
    batch
        .append(&producer, &turn, &step, output(15, &text))
        .unwrap();
    assert_eq!(
        batch.append(&producer, &turn, &step, output(1, &text)),
        Err(ContributionError::Capacity)
    );
    assert_eq!(batch.into_parts().0.len(), 15);
}

#[test]
fn duplicate_domain_proposals_do_not_admit_their_companion_inputs() {
    use rsi_agent_composition_protocol::{DomainCatalog, DomainDefinition};
    use rsi_agent_session_protocol::{DomainIdentity, DomainRevision};
    let definition = DomainDefinition::new(
        DomainIdentity::new("fixture.state", 1).unwrap(),
        &false,
        |_| Ok(()),
    )
    .unwrap();
    let catalog = DomainCatalog::new([definition.registration()]).unwrap();
    let handle = catalog.bind(&definition).unwrap();
    let mut batch = ContributionBatch::default();
    let producer = ContributionId::new("fixture.duplicate").unwrap();
    let turn = TurnId::new("turn").unwrap();
    let step = StepId::new("step").unwrap();
    assert!(
        batch
            .append(
                &producer,
                &turn,
                &step,
                ContributionOutput {
                    inputs: vec![ContributionInput::context("must not enter")],
                    domains: vec![
                        handle.propose(DomainRevision::new(1), &true).unwrap(),
                        handle.propose(DomainRevision::new(1), &false).unwrap()
                    ],
                }
            )
            .is_err()
    );
    let (facts, domains) = batch.into_parts();
    assert!(facts.is_empty() && domains.is_empty());
}
