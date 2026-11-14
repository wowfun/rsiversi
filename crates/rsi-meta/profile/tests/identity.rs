use rsi_meta_profile::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn compiler(value: u64, limits: ProfileLimits) -> ProfileCompiler {
    ProfileCompiler::new(
        ProfileEnvironment::without_paths("test", BTreeMap::from([("value".into(), value.into())]))
            .unwrap(),
        limits,
    )
}

#[test]
fn appended_groups_obey_absolute_depth_before_hashing() {
    let compiler = compiler(0, ProfileLimits::default());
    let steps = (1..=128)
        .map(|depth| {
            let node = ProfileNode::Group(ProfileGroup::new(format!("g{depth}"), []));
            if depth == 1 {
                ProfileStep::Node(node)
            } else {
                ProfileStep::Patch(ProfilePatch::Append {
                    target: format!("g{}", depth - 1),
                    nodes: vec![node],
                })
            }
        })
        .collect::<Vec<_>>();
    let candidate = compiler
        .compile(&ProfileProgram::from_profile(Profile::program(
            steps.clone(),
        )))
        .unwrap();
    assert_eq!(candidate.evaluated_digest().unwrap().len(), 64);
    let mut excessive = steps;
    excessive.push(ProfileStep::Patch(ProfilePatch::Append {
        target: "g128".into(),
        nodes: vec![ProfileNode::Group(ProfileGroup::new("g129", []))],
    }));
    assert!(matches!(
        compiler.compile(&ProfileProgram::from_profile(Profile::program(excessive))),
        Err(ProfileError::CapacityExceeded {
            resource: "group depth",
            maximum: 128
        })
    ));
}

#[test]
fn removing_an_obsolete_include_invalidates_the_source_cache() {
    let root = tempfile::tempdir().unwrap();
    let included = root.path().join("a.toml");
    let path = root.path().join("z.toml");
    std::fs::write(&included, "format = 1\n").unwrap();
    std::fs::write(
        &path,
        "format = 1\n[[steps]]\nkind = 'include'\npath = 'a.toml'\n",
    )
    .unwrap();
    let compiler = compiler(0, ProfileLimits::default());
    let program = ProfileProgram::from_file(&path);
    let old = compiler.compile(&program).unwrap();
    assert!(compiler.sources_unchanged(&old).unwrap());
    std::fs::remove_file(included).unwrap();
    assert!(!compiler.sources_unchanged(&old).unwrap());
    assert!(
        compiler.compile(&program).is_err(),
        "a still-required include must fail"
    );
    std::fs::write(&path, "format = 1\n").unwrap();
    assert!(compiler.compile(&program).is_ok());
}

#[test]
fn evaluated_identity_ignores_source_spelling_but_preserves_order_and_json_numbers() {
    let compiler = compiler(0, ProfileLimits::default());
    let identity = |program| {
        compiler
            .compile(&program)
            .unwrap()
            .evaluated_digest()
            .unwrap()
    };
    let literal = |config| {
        ProfileProgram::from_profile(Profile::new([ProfileEntry::new(
            "one",
            "test.plugin",
            config,
        )]))
    };
    // The JSON parser normalizes integer -0 before Profile receives typed values.
    let negative_zero: Value = serde_json::from_str("{\"value\":-0}").unwrap();
    assert_eq!(negative_zero, json!({"value":0}));
    assert_eq!(
        identity(literal(negative_zero)),
        identity(literal(json!({"value":0})))
    );
    let digests = [
        "{\"value\":1}",
        "{\"value\":1.0}",
        "{\"value\":0}",
        "{\"value\":-0.0}",
        "{\"value\":9007199254740992}",
        "{\"value\":9007199254740993}",
    ]
    .map(|config| identity(literal(serde_json::from_str(config).unwrap())));
    assert_eq!(
        digests
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        6
    );
    let leaf = |name| ProfileEntry::new(name, "test.plugin", Value::Null);
    assert_ne!(
        identity(ProfileProgram::from_profile(Profile::new([
            leaf("one"),
            leaf("two")
        ]))),
        identity(ProfileProgram::from_profile(Profile::new([
            leaf("two"),
            leaf("one")
        ])))
    );
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("profile.toml");
    std::fs::write(&file,"format=1\n[[steps]]\nkind='plugin'\nid='one'\nplugin='test.plugin'\nconfig_rhai='#{ value: defines.value + 1 }'\n").unwrap();
    assert_eq!(
        identity(ProfileProgram::from_file(file)),
        identity(literal(json!({"value":1})))
    );
}

#[test]
fn identity_includes_disabled_nodes_group_shape_and_each_isolation_lane() {
    let compiler = compiler(0, ProfileLimits::default());
    let group = |isolation, enabled, config| {
        ProfileProgram::from_profile(Profile::program([ProfileStep::Node(ProfileNode::Group(
            ProfileGroup::new(
                "group",
                [ProfileNode::Plugin(ProfileEntry::new(
                    "leaf",
                    "test.plugin",
                    config,
                ))],
            )
            .isolation(isolation)
            .enabled(enabled),
        ))]))
    };
    let digest = |program| {
        compiler
            .compile(&program)
            .unwrap()
            .evaluated_digest()
            .unwrap()
    };
    let ordinary = IsolationSpec::default();
    let base = digest(group(ordinary.clone(), false, json!({"secret":"one"})));
    let variants = [
        group(ordinary.clone(), true, json!({"secret":"one"})),
        group(ordinary, false, json!({"secret":"two"})),
        group(
            IsolationSpec::new(["key".into()], [], []),
            false,
            json!({"secret":"one"}),
        ),
        group(
            IsolationSpec::new([], ["key".into()], []),
            false,
            json!({"secret":"one"}),
        ),
        group(
            IsolationSpec::new([], [], ["key".into()]),
            false,
            json!({"secret":"one"}),
        ),
        group(
            IsolationSpec::default().with_named(IsolationLane::Local, "key", "named"),
            false,
            json!({"secret":"one"}),
        ),
        ProfileProgram::from_profile(Profile::program([
            ProfileStep::Node(ProfileNode::Group(
                ProfileGroup::new("group", []).enabled(false),
            )),
            ProfileStep::Node(ProfileNode::Plugin(ProfileEntry::new(
                "leaf",
                "test.plugin",
                json!({"secret":"one"}),
            ))),
        ])),
    ];
    for variant in variants {
        assert_ne!(digest(variant), base);
    }
}

#[test]
fn maximum_group_and_configuration_depth_hash_without_a_recipe_buffer() {
    let limits = ProfileLimits::default();
    let mut config = Value::Null;
    for _ in 1..rsi_meta::MAXIMUM_JSON_DEPTH {
        config = json!([config]);
    }
    let mut node = ProfileNode::Plugin(ProfileEntry::new("leaf", "test.plugin", config));
    for depth in 0..limits.maximum_group_depth {
        node = ProfileNode::Group(ProfileGroup::new(format!("group-{depth}"), [node]));
    }
    let candidate = compiler(0, limits)
        .compile(&ProfileProgram::from_profile(Profile::program([
            ProfileStep::Node(node),
        ])))
        .unwrap();
    assert_eq!(
        candidate.evaluated_digest().unwrap(),
        candidate.evaluated_digest().unwrap()
    );
}
