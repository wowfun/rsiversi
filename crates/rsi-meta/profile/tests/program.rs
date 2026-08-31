use rsi_meta_profile::{
    Profile, ProfileCompiler, ProfileEntry, ProfileEnvironment, ProfileError, ProfileFragment,
    ProfileGroup, ProfileLimits, ProfileNode, ProfilePatch, ProfileProgram, ProfileStep,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn environment(root: &std::path::Path) -> ProfileEnvironment {
    ProfileEnvironment::new(
        root.join("config"),
        root.join("state"),
        root.join("cache"),
        "linux-x86_64",
        BTreeMap::from([
            ("enabled".to_owned(), json!(true)),
            ("value".to_owned(), json!(2)),
        ]),
    )
    .unwrap()
}

#[test]
fn rhai_defines_accept_exact_i64_and_reject_every_inexact_json_number() {
    let temp = tempfile::tempdir().unwrap();
    for value in [
        json!(i64::MIN),
        json!(i64::MAX),
        json!(9_007_199_254_740_993_i64),
    ] {
        let environment = ProfileEnvironment::new(
            temp.path().join("config"),
            temp.path().join("state"),
            temp.path().join("cache"),
            "test",
            BTreeMap::from([("value".to_owned(), value.clone())]),
        )
        .unwrap();
        environment.validate(&ProfileLimits::default()).unwrap();
    }

    for value in [
        json!(0.1),
        json!(0.5),
        json!(-0.0),
        serde_json::from_str::<Value>("1e2").unwrap(),
        json!(u64::MAX),
        json!({"nested": [1, 0.25]}),
    ] {
        let environment = ProfileEnvironment::new(
            temp.path().join("config"),
            temp.path().join("state"),
            temp.path().join("cache"),
            "test",
            BTreeMap::from([("value".to_owned(), value.clone())]),
        )
        .unwrap();
        let diagnostic = environment
            .validate(&ProfileLimits::default())
            .unwrap_err()
            .to_string();
        assert!(!diagnostic.contains(&value.to_string()));
        assert!(diagnostic.contains("signed 64-bit integers"));
    }
}

#[test]
fn linked_program_root_and_launch_patches_execute_left_to_right() {
    let temp = tempfile::tempdir().unwrap();
    let linked = ProfileFragment::program(
        "linked",
        [ProfileStep::Node(ProfileNode::Group(ProfileGroup::new(
            "group",
            [ProfileNode::Plugin(ProfileEntry::new(
                "linked-leaf",
                "linked.plugin",
                json!({"layer": "linked"}),
            ))],
        )))],
    );
    let program = ProfileProgram::from_profile(Profile::new([ProfileEntry::new(
        "root-leaf",
        "root.plugin",
        json!({"value": 1}),
    )]))
    .with_linked_fragments(vec![linked])
    .with_launch_patches(vec![
        ProfilePatch::ReplaceConfig {
            target: "root-leaf".to_owned(),
            config: json!({"value": 2}),
        },
        ProfilePatch::Append {
            target: "group".to_owned(),
            nodes: vec![ProfileNode::Plugin(ProfileEntry::new(
                "launched",
                "launch.plugin",
                Value::Null,
            ))],
        },
    ]);
    let candidate = ProfileCompiler::new(environment(temp.path()), ProfileLimits::default())
        .compile(&program)
        .unwrap();
    let ids = candidate
        .leaves()
        .iter()
        .map(|leaf| leaf.id().as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["linked-leaf", "launched", "root-leaf"]);
    assert_eq!(candidate.leaves()[2].config(), &json!({"value": 2}));
}

#[test]
fn maximum_flat_profile_compiles_at_the_bounded_node_limit() {
    let temp = tempfile::tempdir().unwrap();
    let limits = ProfileLimits::default();
    let entries = (0..limits.maximum_nodes)
        .map(|index| ProfileEntry::new(format!("instance-{index}"), "test.plugin", Value::Null))
        .collect::<Vec<_>>();
    let candidate = ProfileCompiler::new(environment(temp.path()), limits)
        .compile(&ProfileProgram::from_profile(Profile::new(entries)))
        .unwrap();
    assert_eq!(
        candidate.leaves().len(),
        ProfileLimits::default().maximum_nodes
    );
}

#[test]
fn disabled_plugin_configs_count_toward_the_retained_tree_bound() {
    let temp = tempfile::tempdir().unwrap();
    let disabled = ProfileNode::Group(
        ProfileGroup::new(
            "disabled",
            [
                ProfileNode::Plugin(ProfileEntry::new(
                    "one",
                    "test.one",
                    json!({"value": "x".repeat(60)}),
                )),
                ProfileNode::Plugin(ProfileEntry::new(
                    "two",
                    "test.two",
                    json!({"value": "x".repeat(60)}),
                )),
            ],
        )
        .enabled(false),
    );
    let limits = ProfileLimits {
        maximum_config_bytes: 100,
        ..ProfileLimits::default()
    };
    assert!(matches!(
        ProfileCompiler::new(environment(temp.path()), limits).compile(
            &ProfileProgram::from_profile(Profile::new([])).with_linked_fragments(vec![
                ProfileFragment::program("disabled", [ProfileStep::Node(disabled)])
            ])
        ),
        Err(ProfileError::CapacityExceeded {
            resource: "resolved config bytes",
            ..
        })
    ));
}

#[test]
fn ordered_program_executes_required_includes_pure_rhai_and_strict_patches() {
    let temp = tempfile::tempdir().unwrap();
    let child = temp.path().join("child.toml");
    std::fs::write(
        &child,
        r#"
format = 1

[[steps]]
kind = "plugin"
id = "child"
plugin = "test.child"
config = { source = "include" }
"#,
    )
    .unwrap();
    let root = temp.path().join("profile.toml");
    std::fs::write(
        &root,
        r##"
format = 1

[[steps]]
kind = "group"
id = "main"
enabled_rhai = "defines.enabled"

[steps.isolation]
local = ["test.local"]
events = ["test.event"]
portable = ["test.portable"]

[[steps.nodes]]
kind = "plugin"
id = "first"
plugin = "test.first"
config_rhai = "#{ value: defines.value }"

[[steps]]
kind = "include"
path = "child.toml"

[[steps]]
kind = "patch"
target = "first"
config_rhai = "#{ value: defines.value + 1 }"
"##,
    )
    .unwrap();

    let candidate = ProfileCompiler::new(environment(temp.path()), ProfileLimits::default())
        .compile(&ProfileProgram::from_file(&root))
        .unwrap();
    assert_eq!(candidate.leaves().len(), 2);
    assert_eq!(candidate.leaves()[0].id().as_str(), "first");
    assert_eq!(candidate.leaves()[0].config(), &json!({"value": 3}));
    assert_eq!(candidate.leaves()[0].groups(), &["main"]);
    assert_eq!(candidate.leaves()[1].id().as_str(), "child");
    assert_eq!(
        candidate.watch_paths(),
        &[child.canonicalize().unwrap(), root.canonicalize().unwrap()]
    );
    assert_eq!(candidate.source_digest().len(), 64);

    std::fs::write(
        &child,
        "format = 1\n[[steps]]\nkind = \"patch\"\ntarget = \"missing\"\nenabled = false\n",
    )
    .unwrap();
    assert!(matches!(
        ProfileCompiler::new(environment(temp.path()), ProfileLimits::default())
            .compile(&ProfileProgram::from_file(&root)),
        Err(ProfileError::MissingPatchTarget { .. })
    ));
}

#[test]
fn source_and_expression_boundary_rejects_datetime_cycles_and_ambient_access() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile.toml");
    std::fs::write(
        &root,
        "format = 1\n[[steps]]\nkind = \"include\"\npath = \"profile.toml\"\n",
    )
    .unwrap();
    assert!(matches!(
        ProfileCompiler::new(environment(temp.path()), ProfileLimits::default())
            .compile(&ProfileProgram::from_file(&root)),
        Err(ProfileError::IncludeCycle { .. })
    ));

    std::fs::write(
        &root,
        "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"x\"\nplugin = \"p\"\nconfig = { at = 1979-05-27T07:32:00Z }\n",
    )
    .unwrap();
    assert!(matches!(
        ProfileCompiler::new(environment(temp.path()), ProfileLimits::default())
            .compile(&ProfileProgram::from_file(&root)),
        Err(ProfileError::NonJsonToml { .. })
    ));

    std::fs::write(
        &root,
        "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"x\"\nplugin = \"p\"\nconfig_rhai = \"env(\\\"HOME\\\")\"\n",
    )
    .unwrap();
    let error = ProfileCompiler::new(environment(temp.path()), ProfileLimits::default())
        .compile(&ProfileProgram::from_file(&root))
        .unwrap_err();
    assert!(matches!(error, ProfileError::Expression { .. }));
    assert!(!error.to_string().contains("HOME"));

    std::fs::write(
        &root,
        "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"x\"\nplugin = \"p\"\nenabled_rhai = \"timestamp() == timestamp()\"\n",
    )
    .unwrap();
    assert!(matches!(
        ProfileCompiler::new(environment(temp.path()), ProfileLimits::default())
            .compile(&ProfileProgram::from_file(&root)),
        Err(ProfileError::Expression { .. })
    ));

    for expression in [r#"{ print("x"); true }"#, r#"{ debug("x"); true }"#] {
        std::fs::write(
            &root,
            format!(
                "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"x\"\nplugin = \"p\"\nenabled_rhai = {}\n",
                serde_json::to_string(expression).unwrap()
            ),
        )
        .unwrap();
        assert!(matches!(
            ProfileCompiler::new(environment(temp.path()), ProfileLimits::default())
                .compile(&ProfileProgram::from_file(&root)),
            Err(ProfileError::Expression { .. })
        ));
    }
}

#[test]
fn strict_schema_rejects_forbidden_patch_shapes_and_redacts_source_values() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile.toml");
    for forbidden in [
        "remove = true",
        "plugin = \"replacement.plugin\"",
        "move = \"elsewhere\"",
    ] {
        std::fs::write(
            &root,
            format!(
                "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"x\"\nplugin = \"p\"\n[[steps]]\nkind = \"patch\"\ntarget = \"x\"\n{forbidden}\n"
            ),
        )
        .unwrap();
        let error = ProfileCompiler::new(environment(temp.path()), ProfileLimits::default())
            .compile(&ProfileProgram::from_file(&root))
            .unwrap_err();
        assert!(matches!(error, ProfileError::Source { .. }));
        assert!(!error.to_string().contains("replacement.plugin"));
    }

    std::fs::write(
        &root,
        "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"x\"\nplugin = \"p\"\nconfig = { secret = \"literal-secret\" }\nconfig_rhai = \"#{ secret: \\\"expression-secret\\\" }\"\n",
    )
    .unwrap();
    let error = ProfileCompiler::new(environment(temp.path()), ProfileLimits::default())
        .compile(&ProfileProgram::from_file(&root))
        .unwrap_err();
    assert!(matches!(error, ProfileError::InvalidProgram(_)));
    assert!(!error.to_string().contains("literal-secret"));
    assert!(!error.to_string().contains("expression-secret"));

    std::fs::write(
        &root,
        "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"x\"\nplugin = \"secret-value\n",
    )
    .unwrap();
    let error = ProfileCompiler::new(environment(temp.path()), ProfileLimits::default())
        .compile(&ProfileProgram::from_file(&root))
        .unwrap_err();
    assert!(!error.to_string().contains("secret-value"));
}

#[test]
fn rhai_expression_operation_limit_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile.toml");
    let expression = format!("#{{ value: {} }}", vec!["1"; 128].join(" + "));
    std::fs::write(
        &root,
        format!(
            "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"x\"\nplugin = \"p\"\nconfig_rhai = {}\n",
            serde_json::to_string(&expression).unwrap()
        ),
    )
    .unwrap();
    let limits = ProfileLimits {
        maximum_expression_operations: 8,
        ..ProfileLimits::default()
    };

    assert!(matches!(
        ProfileCompiler::new(environment(temp.path()), limits)
            .compile(&ProfileProgram::from_file(&root)),
        Err(ProfileError::Expression { .. })
    ));
}

#[test]
fn rhai_operation_limit_is_shared_across_one_complete_rebuild() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile.toml");
    let expression = "#{ value: defines.value + defines.value + defines.value }";
    std::fs::write(
        &root,
        format!(
            "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"first\"\nplugin = \"p\"\nconfig_rhai = {}\n[[steps]]\nkind = \"plugin\"\nid = \"second\"\nplugin = \"p\"\nconfig_rhai = {}\n",
            serde_json::to_string(expression).unwrap(),
            serde_json::to_string(expression).unwrap(),
        ),
    )
    .unwrap();
    let limits = ProfileLimits {
        maximum_expression_operations: 16,
        ..ProfileLimits::default()
    };

    assert!(matches!(
        ProfileCompiler::new(environment(temp.path()), limits)
            .compile(&ProfileProgram::from_file(&root)),
        Err(ProfileError::Expression { .. })
    ));
}

#[test]
fn include_file_depth_and_byte_limits_fail_at_the_source_owner() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root.toml");
    let child = temp.path().join("child.toml");
    std::fs::write(
        &root,
        "format = 1\n[[steps]]\nkind = \"include\"\npath = \"child.toml\"\n",
    )
    .unwrap();
    std::fs::write(&child, "format = 1\n").unwrap();

    let mut limits = ProfileLimits {
        maximum_include_depth: 1,
        ..ProfileLimits::default()
    };
    assert!(matches!(
        ProfileCompiler::new(environment(temp.path()), limits.clone())
            .compile(&ProfileProgram::from_file(&root)),
        Err(ProfileError::CapacityExceeded {
            resource: "include depth",
            ..
        })
    ));
    limits.maximum_include_depth = 2;
    limits.maximum_source_files = 1;
    assert!(matches!(
        ProfileCompiler::new(environment(temp.path()), limits.clone())
            .compile(&ProfileProgram::from_file(&root)),
        Err(ProfileError::CapacityExceeded {
            resource: "source files",
            ..
        })
    ));
    limits.maximum_source_files = 2;
    limits.maximum_document_bytes = 8;
    assert!(matches!(
        ProfileCompiler::new(environment(temp.path()), limits)
            .compile(&ProfileProgram::from_file(&root)),
        Err(ProfileError::CapacityExceeded {
            resource: "document bytes",
            ..
        })
    ));
}

#[test]
fn identities_are_global_across_nested_groups_and_includes() {
    let temp = tempfile::tempdir().unwrap();
    let child = temp.path().join("child.toml");
    std::fs::write(
        &child,
        "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"same\"\nplugin = \"child\"\n",
    )
    .unwrap();
    let root = temp.path().join("root.toml");
    std::fs::write(
        &root,
        "format = 1\n[[steps]]\nkind = \"group\"\nid = \"outer\"\n[[steps.nodes]]\nkind = \"plugin\"\nid = \"same\"\nplugin = \"root\"\n[[steps]]\nkind = \"include\"\npath = \"child.toml\"\n",
    )
    .unwrap();
    assert!(matches!(
        ProfileCompiler::new(environment(temp.path()), ProfileLimits::default())
            .compile(&ProfileProgram::from_file(&root)),
        Err(ProfileError::DuplicateInstance { id }) if id == "same"
    ));
}

#[test]
fn source_digest_includes_fragment_identity_and_frozen_environment_paths() {
    let temp = tempfile::tempdir().unwrap();
    let profile = Profile::new([ProfileEntry::new("leaf", "test.leaf", Value::Null)]);
    let program = |fragment_id| {
        ProfileProgram::from_profile(Profile::new([])).with_linked_fragments(vec![
            ProfileFragment::new(fragment_id, profile.entries().iter().cloned()),
        ])
    };
    let limits = ProfileLimits::default();
    let first = ProfileCompiler::new(environment(temp.path()), limits.clone())
        .compile(&program("first"))
        .unwrap();
    let second = ProfileCompiler::new(environment(temp.path()), limits.clone())
        .compile(&program("second"))
        .unwrap();
    assert_ne!(first.source_digest(), second.source_digest());

    let moved_environment = ProfileEnvironment::new(
        temp.path().join("different-config"),
        temp.path().join("state"),
        temp.path().join("cache"),
        "linux-x86_64",
        BTreeMap::from([
            ("enabled".to_owned(), json!(true)),
            ("value".to_owned(), json!(2)),
        ]),
    )
    .unwrap();
    let moved = ProfileCompiler::new(moved_environment, limits)
        .compile(&program("first"))
        .unwrap();
    assert_ne!(first.source_digest(), moved.source_digest());
}

#[test]
fn source_digest_changes_when_a_frozen_define_value_changes_at_the_same_paths() {
    let temp = tempfile::tempdir().unwrap();
    let profile = Profile::new([ProfileEntry::new("leaf", "test.leaf", Value::Null)]);
    let program = ProfileProgram::from_profile(profile);
    let compile = |value| {
        let environment = ProfileEnvironment::new(
            temp.path().join("config"),
            temp.path().join("state"),
            temp.path().join("cache"),
            "linux-x86_64",
            BTreeMap::from([
                ("enabled".to_owned(), json!(true)),
                ("value".to_owned(), json!(value)),
            ]),
        )
        .unwrap();
        ProfileCompiler::new(environment, ProfileLimits::default())
            .compile(&program)
            .unwrap()
    };

    let first = compile(2);
    let changed = compile(3);

    assert_ne!(first.source_digest(), changed.source_digest());
}

#[test]
fn source_digest_changes_when_included_file_bytes_change_at_the_same_path() {
    let temp = tempfile::tempdir().unwrap();
    let child = temp.path().join("child.toml");
    let root = temp.path().join("root.toml");
    std::fs::write(
        &root,
        "format = 1\n[[steps]]\nkind = \"include\"\npath = \"child.toml\"\n",
    )
    .unwrap();
    std::fs::write(
        &child,
        "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"leaf\"\nplugin = \"test.leaf\"\nconfig = { value = 1 }\n",
    )
    .unwrap();
    let compile = || {
        ProfileCompiler::new(environment(temp.path()), ProfileLimits::default())
            .compile(&ProfileProgram::from_file(&root))
            .unwrap()
    };
    let first = compile();

    std::fs::write(
        &child,
        "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"leaf\"\nplugin = \"test.leaf\"\nconfig = { value = 2 }\n",
    )
    .unwrap();
    let changed = compile();

    assert_eq!(first.watch_paths(), changed.watch_paths());
    assert_ne!(first.source_digest(), changed.source_digest());
}

#[test]
fn deeply_nested_public_groups_fail_at_the_group_depth_boundary_without_overflowing() {
    let temp = tempfile::tempdir().unwrap();
    let mut node = ProfileNode::Plugin(ProfileEntry::new("leaf", "test.leaf", Value::Null));
    for depth in (0..4_095).rev() {
        node = ProfileNode::Group(ProfileGroup::new(format!("group-{depth}"), [node]));
    }
    let program = ProfileProgram::from_profile(Profile::program([ProfileStep::Node(node)]));

    assert!(matches!(
        ProfileCompiler::new(environment(temp.path()), ProfileLimits::default()).compile(&program),
        Err(ProfileError::CapacityExceeded {
            resource: "group depth",
            ..
        })
    ));
}

#[cfg(unix)]
#[test]
fn selected_profile_symlink_is_rejected_at_the_reader_boundary() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("target.toml");
    let selected = temp.path().join("selected.toml");
    std::fs::write(&target, "format = 1\n").unwrap();
    symlink(&target, &selected).unwrap();

    assert!(matches!(
        ProfileCompiler::new(environment(temp.path()), ProfileLimits::default())
            .compile(&ProfileProgram::from_file(&selected)),
        Err(ProfileError::Source { .. })
    ));
}

#[cfg(unix)]
#[test]
fn included_profile_symlink_is_rejected_at_the_same_reader_boundary() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("target.toml");
    let included = temp.path().join("included.toml");
    let root = temp.path().join("profile.toml");
    std::fs::write(&target, "format = 1\n").unwrap();
    symlink(&target, &included).unwrap();
    std::fs::write(
        &root,
        "format = 1\n[[steps]]\nkind = \"include\"\npath = \"included.toml\"\n",
    )
    .unwrap();

    assert!(matches!(
        ProfileCompiler::new(environment(temp.path()), ProfileLimits::default())
            .compile(&ProfileProgram::from_file(&root)),
        Err(ProfileError::Source { .. })
    ));
}
