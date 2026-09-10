use rsi_agent_composition_protocol::{
    DomainCatalog, DomainCatalogBuilder, DomainDefinition, DomainError,
};
use rsi_agent_session_protocol::{DomainIdentity, DomainRevision};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    enabled: bool,
    remaining: u32,
}

fn definition() -> DomainDefinition<Plan> {
    DomainDefinition::new(
        DomainIdentity::new("example.plan", 1).unwrap(),
        &Plan {
            enabled: false,
            remaining: 3,
        },
        |state| {
            if state.remaining <= 10 {
                Ok(())
            } else {
                Err("remaining exceeds ten".into())
            }
        },
    )
    .unwrap()
}

#[test]
fn typed_proposals_require_valid_state_and_the_exact_frozen_generation() {
    let definition = definition();
    let first = DomainCatalog::new([definition.registration()]).unwrap();
    let second = DomainCatalog::new([definition.registration()]).unwrap();
    let handle = first.bind(&definition).unwrap();
    let replacement = Plan {
        enabled: true,
        remaining: 2,
    };
    let proposal = handle
        .propose(DomainRevision::new(7), &replacement)
        .unwrap();
    assert_eq!(proposal.expected_revision(), DomainRevision::new(7));
    assert_eq!(
        first.validate_proposal(&proposal).unwrap().value(),
        &serde_json::json!({"enabled": true, "remaining": 2})
    );
    assert!(matches!(
        second.validate_proposal(&proposal),
        Err(DomainError::WrongGeneration)
    ));
    assert!(
        handle
            .propose(
                DomainRevision::new(7),
                &Plan {
                    enabled: true,
                    remaining: 11
                }
            )
            .is_err()
    );
    let baseline = first.baseline();
    assert_eq!(
        handle.decode(&baseline[0]).unwrap(),
        Plan {
            enabled: false,
            remaining: 3
        }
    );
    assert_eq!(baseline[0].identity(), definition.identity());
}

#[test]
fn withdrawn_registration_cannot_propose_into_the_frozen_replacement() {
    let definition = definition();
    let mut stage = DomainCatalogBuilder::new();
    let old = stage.register(definition.registration()).unwrap();
    let old_handle = definition.bind(&old).unwrap();
    assert!(matches!(
        stage.register(definition.registration()),
        Err(DomainError::Duplicate(_))
    ));
    assert!(stage.withdraw(&old));
    let replacement = stage.register(definition.registration()).unwrap();
    assert!(!stage.withdraw(&old));
    let current = definition.bind(&replacement).unwrap();
    let catalog = stage.finish().unwrap();
    let replacement_state = Plan {
        enabled: true,
        remaining: 1,
    };
    assert!(matches!(
        catalog.validate_proposal(
            &old_handle
                .propose(DomainRevision::new(0), &replacement_state)
                .unwrap()
        ),
        Err(DomainError::WrongGeneration)
    ));
    catalog
        .validate_proposal(
            &current
                .propose(DomainRevision::new(0), &replacement_state)
                .unwrap(),
        )
        .unwrap();
}

#[test]
fn cold_typed_decode_rejects_missing_wrong_and_invalid_codecs_without_mutating_raw_history() {
    use rsi_agent_session_protocol::{DomainSnapshot, DomainStateValue};
    let definition = definition();
    let catalog = DomainCatalog::new([definition.registration()]).unwrap();
    let handle = catalog.bind(&definition).unwrap();
    let invalid = DomainSnapshot::new(
        definition.identity().clone(),
        DomainStateValue::new(serde_json::json!({"enabled": true, "remaining": 11})).unwrap(),
    );
    assert!(catalog.validate_snapshot(&invalid).is_err());
    assert!(handle.decode(&invalid).is_err());
    let unknown = DomainSnapshot::new(
        DomainIdentity::new("example.plan", 2).unwrap(),
        invalid.state().clone(),
    );
    assert!(matches!(
        catalog.validate_snapshot(&unknown),
        Err(DomainError::Unsupported(_))
    ));
    assert!(matches!(
        handle.decode(&unknown),
        Err(DomainError::Unsupported(_))
    ));
    assert!(matches!(
        DomainCatalog::default().validate_snapshot(&invalid),
        Err(DomainError::Unsupported(_))
    ));
    assert_eq!(
        invalid.state().value(),
        &serde_json::json!({"enabled": true, "remaining": 11})
    );
    assert!(
        DomainDefinition::new(
            definition.identity().clone(),
            &Plan {
                enabled: false,
                remaining: 11
            },
            |state| if state.remaining <= 10 {
                Ok(())
            } else {
                Err("invalid".into())
            }
        )
        .is_err()
    );
}

#[test]
fn frozen_domain_sets_bound_names_counts_and_aggregate_baseline_bytes() {
    let definitions: Vec<_> = (0..65)
        .map(|index| {
            DomainDefinition::new(
                DomainIdentity::new(format!("domain.{index}"), 1).unwrap(),
                &false,
                |_| Ok(()),
            )
            .unwrap()
        })
        .collect();
    assert!(
        DomainCatalog::new(definitions[..64].iter().map(DomainDefinition::registration)).is_ok()
    );
    assert!(matches!(
        DomainCatalog::new(definitions.iter().map(DomainDefinition::registration)),
        Err(DomainError::Capacity)
    ));
    let duplicate =
        DomainDefinition::new(DomainIdentity::new("domain.0", 2).unwrap(), &true, |_| {
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        DomainCatalog::new([definitions[0].registration(), duplicate.registration()]),
        Err(DomainError::Duplicate(_))
    ));
    let large: Vec<_> = (0..5)
        .map(|index| {
            DomainDefinition::new(
                DomainIdentity::new(format!("large.{index}"), 1).unwrap(),
                &"x".repeat(220_000),
                |_| Ok(()),
            )
            .unwrap()
        })
        .collect();
    assert!(DomainCatalog::new(large[..4].iter().map(DomainDefinition::registration)).is_ok());
    assert!(matches!(
        DomainCatalog::new(large.iter().map(DomainDefinition::registration)),
        Err(DomainError::Capacity)
    ));
}

#[test]
fn a_draft_baseline_freezes_actual_initial_values_and_preserves_them_on_rejection() {
    use rsi_agent_composition_protocol::DomainBaseline;
    let definition = definition();
    let catalog = DomainCatalog::new([definition.registration()]).unwrap();
    let handle = catalog.bind(&definition).unwrap();
    let mut baseline = DomainBaseline::new(catalog.clone()).unwrap();
    let original_digest = baseline.digest().to_owned();
    let next = Plan {
        enabled: true,
        remaining: 2,
    };
    baseline
        .apply(&handle.propose(DomainRevision::new(0), &next).unwrap())
        .unwrap();
    assert_ne!(baseline.digest(), original_digest);
    let frozen = baseline.commit().unwrap().clone();
    assert_eq!(handle.decode(frozen.updates()[0].snapshot()).unwrap(), next);
    assert!(
        baseline
            .apply(&handle.propose(DomainRevision::new(1), &next).unwrap())
            .is_err()
    );
    assert_eq!(baseline.commit().unwrap(), &frozen);
    let missing = DomainCatalog::new([definition.registration()]).unwrap();
    let foreign = missing
        .bind(&definition)
        .unwrap()
        .propose(DomainRevision::new(0), &next)
        .unwrap();
    assert!(matches!(
        baseline.apply(&foreign),
        Err(DomainError::WrongGeneration)
    ));
    assert_eq!(baseline.commit().unwrap(), &frozen);
    assert!(catalog.validate_complete_states(&[]).is_err());
    catalog
        .validate_complete_states(&[frozen.updates()[0].snapshot().clone()])
        .unwrap();
}

#[test]
fn draft_batches_never_publish_a_valid_prefix_before_a_later_rejection() {
    use rsi_agent_composition_protocol::DomainBaseline;
    let first = DomainDefinition::new(DomainIdentity::new("first", 1).unwrap(), &false, |_| Ok(()))
        .unwrap();
    let second =
        DomainDefinition::new(
            DomainIdentity::new("second", 1).unwrap(),
            &false,
            |_| Ok(()),
        )
        .unwrap();
    let catalog = DomainCatalog::new([first.registration(), second.registration()]).unwrap();
    let a = catalog.bind(&first).unwrap();
    let b = catalog.bind(&second).unwrap();
    let mut baseline = DomainBaseline::new(catalog).unwrap();
    let original = baseline.digest().to_owned();
    let valid = a.propose(DomainRevision::new(0), &true).unwrap();
    assert!(
        baseline
            .apply_batch(&[
                valid.clone(),
                b.propose(DomainRevision::new(1), &true).unwrap()
            ])
            .is_err()
    );
    assert_eq!(baseline.digest(), original);
    assert!(
        baseline
            .apply_batch(&[valid.clone(), valid.clone()])
            .is_err()
    );
    assert_eq!(baseline.digest(), original);
    baseline
        .apply_batch(&[valid, b.propose(DomainRevision::new(0), &true).unwrap()])
        .unwrap();
    assert!(
        baseline
            .commit()
            .unwrap()
            .updates()
            .iter()
            .all(|update| update.snapshot().state().value() == &serde_json::json!(true))
    );
}
