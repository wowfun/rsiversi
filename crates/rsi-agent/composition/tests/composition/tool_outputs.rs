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
async fn current_outputs_replace_saved_declarations_and_keep_old_pins_readable() {
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
    assert!(fixture.service.pin(&fixture.id, Some(&saved)).await.is_ok());
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
    assert!(fixture.service.pin(&fixture.id, Some(&saved)).await.is_ok());
    drop((first, restored, current));
    fixture.stop().await;
}

#[tokio::test]
async fn new_output_domain_is_initialized_but_unknown_stored_codecs_are_rejected() {
    let fixture = Fixture::new(&typed_profile("object")).await;
    let missing = AgentGenerationSeed::default();
    assert!(
        ToolOutputCatalog::from_baseline(missing.states())
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .service
            .pin(&fixture.id, Some(&missing))
            .await
            .is_ok()
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
    assert!(
        fixture
            .service
            .pin(&fixture.id, Some(&missing))
            .await
            .is_ok()
    );
    fixture.stop().await;
}
