use rsi_meta_profile::{
    IsolationLane, IsolationSpec, ProfileEntry, ProfileFragment, ProfileGroup, ProfileNode,
    ProfilePatch, ProfileStep,
};
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

#[test]
fn fragment_identity_preserves_the_v1_domain_and_tagged_declarations() {
    let entry = || ProfileEntry::new("leaf", "test.plugin", json!({"text":"界", "value":1}));
    let isolation = IsolationSpec::new(
        ["local.key".into()],
        ["event.key".into()],
        ["portable.key".into()],
    )
    .with_named(IsolationLane::Local, "named.local", "shared")
    .with_named(IsolationLane::Event, "named.event", "shared")
    .with_named(IsolationLane::Portable, "named.portable", "shared");
    let fragments = [
        ProfileFragment::new("empty", []),
        ProfileFragment::new("leaves", [entry()]),
        ProfileFragment::program(
            "program",
            [
                ProfileStep::Node(ProfileNode::Group(
                    ProfileGroup::new(
                        "group",
                        [ProfileNode::Group(ProfileGroup::new(
                            "nested",
                            [ProfileNode::Plugin(entry())],
                        ))],
                    )
                    .enabled(false)
                    .isolation(isolation.clone()),
                )),
                ProfileStep::Patch(ProfilePatch::Append {
                    target: "group".into(),
                    nodes: vec![ProfileNode::Plugin(entry())],
                }),
                ProfileStep::Patch(ProfilePatch::ReplaceConfig {
                    target: "leaf".into(),
                    config: json!({"next":[true, null]}),
                }),
                ProfileStep::Patch(ProfilePatch::SetEnabled {
                    target: "group".into(),
                    enabled: true,
                }),
                ProfileStep::Patch(ProfilePatch::ReplaceIsolation {
                    target: "group".into(),
                    isolation,
                }),
            ],
        ),
    ];
    // Fixed v1 vectors cover the identity domain, ordered declarations and every patch lane.
    let expected = [
        "1268f6b56d8f99a457adc0a055121840675949a20a1df53a5efed1cc5a4c4dc2",
        "2fdd1190e921b588861b2738b6485e4295b2f5bf48f563581ce41656c6723fe4",
        "1bff73db5f78d079ce3816689c02f714f1595275f87b6964660eb08aefcb2586",
    ];
    for (fragment, expected) in fragments.iter().zip(expected) {
        assert_eq!(fragment.source_digest(), expected, "{}", fragment.id());
    }
}
