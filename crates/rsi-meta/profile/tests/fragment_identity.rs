use rsi_meta_profile::{ProfileFragment, ProfilePatch, ProfileStep};
use serde_json::json;

#[test]
fn patch_fingerprint_is_available_before_its_target_fragment_and_tracks_source_changes() {
    let fragment = |target: &str, value| {
        ProfileFragment::program(
            "edit",
            [ProfileStep::Patch(ProfilePatch::ReplaceConfig {
                target: target.into(),
                config: json!({"value":value}),
            })],
        )
    };
    let original = fragment("provided-by-another-addon", 1);
    assert_eq!(original.source_digest(), original.clone().source_digest());
    assert_ne!(
        original.source_digest(),
        fragment("provided-by-another-addon", 2).source_digest()
    );
    assert_ne!(
        original.source_digest(),
        fragment("another-target", 1).source_digest()
    );
    assert_ne!(
        original.source_digest(),
        ProfileFragment::new("edit", []).source_digest()
    );
}
