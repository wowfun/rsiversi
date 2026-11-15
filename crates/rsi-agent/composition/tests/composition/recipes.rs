use super::*;
use rsi_agent_composition_protocol::AgentGenerationSeed;
use rsi_agent_session_protocol::{FrozenAgentSettings, SessionHeader, SessionId};

pub(super) fn header(workspace: &std::path::Path) -> SessionHeader {
    SessionHeader::new_local(
        SessionId::new("restore").unwrap(),
        1,
        workspace.to_str().unwrap(),
        AgentPresetId::new("default").unwrap(),
        FrozenAgentSettings::new(
            "fixture",
            "system",
            rsi_ai_protocol::ModelRef::new("provider", "model").unwrap(),
            rsi_sandbox::SandboxMode::ReadOnly,
            true,
        )
        .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn successor_preparation_captures_source_without_rebuilding_unchanged_or_retained_pins() {
    let fixture = Fixture::new(&profile("old")).await;
    let old = fixture.service.pin(&fixture.id, None).await.unwrap();
    let header = header(fixture.temporary.path());
    let seed = AgentGenerationSeed::new(old.domains().baseline().to_vec()).unwrap();
    assert!(
        fixture
            .service
            .prepare_update(&header, &old)
            .await
            .unwrap()
            .is_none()
    );
    fs::write(&fixture.source, profile("next")).unwrap();
    let candidate = fixture
        .service
        .prepare_update(&header, &old)
        .await
        .unwrap()
        .unwrap();
    // Holding a candidate must not serialize another Session's unchanged probe.
    fs::write(&fixture.source, profile("old")).unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            fixture.service.prepare_update(&header, &old)
        )
        .await
        .expect("unchanged source probe waited for another candidate")
        .unwrap()
        .is_none()
    );
    fs::write(&fixture.source, failing_profile("later")).unwrap();
    let next = candidate.build(&seed).await.unwrap();
    assert_eq!(next.tools().definitions()[0].name(), "probe-next");
    assert_eq!(old.tools().definitions()[0].name(), "probe-old");
    let failed = fixture
        .service
        .prepare_update(&header, &next)
        .await
        .unwrap()
        .unwrap();
    assert!(failed.build(&seed).await.is_err());
    assert_eq!(next.tools().definitions()[0].name(), "probe-next");
    fs::write(&fixture.source, profile("next")).unwrap();
    assert!(
        fixture
            .service
            .prepare_update(&header, &next)
            .await
            .unwrap()
            .is_none()
    );
    drop((old, next));
    fixture.stop().await;
}

#[tokio::test]
async fn cold_recovery_uses_current_plugins_and_refuses_missing_current_source() {
    let original = Fixture::new(&profile("saved")).await;
    let pin = original.service.pin(&original.id, None).await.unwrap();
    let selection = pin.selection().clone();
    let seed = AgentGenerationSeed::new(pin.domains().baseline().to_vec()).unwrap();
    drop(pin);
    original.stop().await;
    let current = Fixture::new(&profile("current")).await;
    let header = header(current.temporary.path());
    let restored = current
        .service
        .pin_session(&header, Some(&seed))
        .await
        .unwrap();
    assert_eq!(restored.tools().definitions()[0].name(), "probe-current");
    assert_ne!(restored.selection(), &selection);
    fs::remove_file(&current.source).unwrap();
    assert!(
        current
            .service
            .pin_session(&header, Some(&seed))
            .await
            .is_err()
    );
    assert_eq!(restored.tools().definitions()[0].name(), "probe-current");
    drop(restored);
    current.stop().await;
}
