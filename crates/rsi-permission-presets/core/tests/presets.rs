use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_permission_presets::{PermissionPresetsContract, PermissionPresetsFactory};
use rsi_sandbox::SandboxMode;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn exact_frozen_preset_is_data_not_enforcement() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.permission-presets",
                "test",
                UpdateMode::Replayable,
                Arc::new(PermissionPresetsFactory),
            ),
            json!({"standard":{"sandbox":"workspace-write","require_approval":true}}),
        )
        .await
        .unwrap();
    let presets = runtime
        .root()
        .lookup_local::<PermissionPresetsContract>()
        .unwrap();
    let standard = presets.get("standard").unwrap();
    assert_eq!(standard.sandbox, SandboxMode::WorkspaceWrite);
    assert!(standard.require_approval);
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_sandbox::SandboxContract>()
            .is_none()
    );
    drop(presets);
    assert!(fiber.dispose().await.is_clean());
}
