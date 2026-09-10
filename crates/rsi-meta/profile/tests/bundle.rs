use rsi_meta_profile::{
    ProfileBundle, ProfileCompiler, ProfileEnvironment, ProfileLimits, ProfileProgram,
};
use serde_json::json;
use std::collections::BTreeMap;

fn compile(
    root: &str,
    sources: &[(&str, &str)],
) -> rsi_meta_profile::Result<rsi_meta_profile::ProfileCandidate> {
    let limits = ProfileLimits::default();
    let bundle = ProfileBundle::new(
        root,
        sources
            .iter()
            .map(|(path, source)| ((*path).into(), source.as_bytes().to_vec()))
            .collect(),
        &limits,
    )?;
    ProfileCompiler::new(
        ProfileEnvironment::without_paths(
            "browser",
            BTreeMap::from([("count".into(), json!(41))]),
        )?,
        limits,
    )
    .compile(&ProfileProgram::from_bundle(bundle))
}

#[test]
fn bundle_uses_the_shared_include_expression_and_patch_language() {
    let candidate = compile("root.toml", &[
        ("root.toml", "format = 1\n[[steps]]\nkind = 'include'\npath = 'child/plugins.toml'\n[[steps]]\nkind = 'patch'\ntarget = 'counter'\nconfig_rhai = '#{ value: defines.count + 1 }'"),
        ("child/plugins.toml", "format = 1\n[[steps]]\nkind = 'plugin'\nid = 'counter'\nplugin = 'counter'\nconfig_rhai = '#{ value: defines.count, platform: platform }'"),
    ]).unwrap();
    assert_eq!(candidate.leaves()[0].config(), &json!({"value": 42}));
    assert!(candidate.watch_paths().is_empty());
}

#[test]
fn bundle_rejects_missing_sources_cycles_and_escape_before_composition() {
    for include in [
        "absent.toml",
        "root.toml",
        "../outside.toml",
        "/outside.toml",
        "C:/outside.toml",
    ] {
        let root = format!("format = 1\n[[steps]]\nkind = 'include'\npath = '{include}'");
        assert!(
            compile("root.toml", &[("root.toml", &root)]).is_err(),
            "accepted {include}"
        );
    }
}

#[test]
fn unused_bundle_documents_are_bounded_before_retention() {
    let limits = ProfileLimits {
        maximum_document_bytes: 16,
        ..ProfileLimits::default()
    };
    assert!(
        ProfileBundle::new(
            "root.toml",
            BTreeMap::from([
                ("root.toml".into(), b"format = 1".to_vec()),
                ("unused.toml".into(), vec![b' '; 17]),
            ]),
            &limits
        )
        .is_err()
    );
}

#[test]
fn path_free_environment_does_not_expose_native_paths() {
    let candidate = compile("root.toml", &[("root.toml",
        "format = 1\n[[steps]]\nkind = 'plugin'\nid = 'counter'\nplugin = 'counter'\nconfig_rhai = 'paths'"
    )]).unwrap();
    assert_eq!(candidate.leaves()[0].config(), &json!({}));
}
