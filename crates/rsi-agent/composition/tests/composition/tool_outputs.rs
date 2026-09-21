use super::*;
use rsi_agent_composition_protocol::{AgentGenerationSeed, ToolOutputCatalog};
use rsi_agent_session_protocol::{DomainIdentity, DomainSnapshot, DomainStateValue};

fn typed_profile(kind: &str) -> String {
    profile("stable").replace(
        "marker = \"stable\"",
        &format!("marker = \"stable\", output_kind = \"{kind}\""),
    )
}

#[tokio::test]
async fn frozen_outputs_reject_changed_restores_and_keep_old_pins_readable() {
    let fixture = Fixture::new(&typed_profile("object")).await;
    let first = fixture.service.pin(&fixture.id, None).await.unwrap();
    let saved = AgentGenerationSeed::new(first.domains().baseline().to_vec()).unwrap();
    let historical = ToolOutputCatalog::from_baseline(saved.states())
        .unwrap()
        .unwrap();
    assert_eq!(
        historical.get("probe-stable").unwrap().schema(),
        &serde_json::json!({"type":"object"})
    );
    let restored = fixture
        .service
        .pin(&fixture.id, Some(&saved))
        .await
        .unwrap();
    assert_eq!(restored.domains().baseline(), first.domains().baseline());
    fixture.replace_source(&typed_profile("string"));
    let current = fixture.service.pin(&fixture.id, None).await.unwrap();
    assert_eq!(
        fixture
            .service
            .pin(&fixture.id, Some(&saved))
            .await
            .unwrap_err(),
        AgentCompositionError::OutputCatalogMismatch
    );
    assert_eq!(
        first.tools().output_declarations()["probe-stable"].schema(),
        &serde_json::json!({"type":"object"})
    );
    assert_eq!(
        current.tools().output_declarations()["probe-stable"].schema(),
        &serde_json::json!({"type":"string"})
    );
    assert_eq!(
        historical,
        ToolOutputCatalog::from_baseline(saved.states())
            .unwrap()
            .unwrap()
    );
    assert!(
        fixture
            .service
            .pin(&fixture.id, None)
            .await
            .unwrap()
            .same_generation(&current)
    );
    fixture.replace_source(&profile("stable"));
    assert_eq!(
        fixture
            .service
            .pin(&fixture.id, Some(&saved))
            .await
            .unwrap_err(),
        AgentCompositionError::OutputCatalogMismatch
    );
    drop((first, restored, current));
    fixture.stop().await;
}

#[tokio::test]
async fn legacy_missing_and_unknown_output_baselines_fail_before_publication() {
    let fixture = Fixture::new(&typed_profile("object")).await;
    let missing = AgentGenerationSeed::default();
    assert!(
        ToolOutputCatalog::from_baseline(missing.states())
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fixture
            .service
            .pin(&fixture.id, Some(&missing))
            .await
            .unwrap_err(),
        AgentCompositionError::MissingOutputCatalog
    );
    let unknown = AgentGenerationSeed::new(vec![DomainSnapshot::new(
        DomainIdentity::new("rsi.tools.outputs", 2).unwrap(),
        DomainStateValue::new(serde_json::json!({})).unwrap(),
    )])
    .unwrap();
    assert!(matches!(
        fixture.service.pin(&fixture.id, Some(&unknown)).await,
        Err(AgentCompositionError::UnsupportedSeedCodec { .. })
    ));
    fixture.replace_source(&profile("stable"));
    let untyped = fixture
        .service
        .pin(&fixture.id, Some(&missing))
        .await
        .unwrap();
    assert!(untyped.tools().output_declarations().is_empty());
    assert!(untyped.domains().baseline().is_empty());
    drop(untyped);
    fixture.stop().await;
}
