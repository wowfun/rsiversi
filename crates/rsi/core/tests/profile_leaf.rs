#![cfg(unix)]
use async_trait::async_trait;
use rsi::{HostLeafEdit, HostProfileId, ProfileCatalog, ProfileEditError};
use rsi_host::{HostBuilder, HostPaths};
use rsi_meta::{
    ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation, Runtime,
    RuntimeLimits, UpdateMode,
};
use serde_json::{Value, json};
use std::{
    fs,
    sync::{Arc, Mutex},
};
#[derive(Debug)]
struct Validate(Arc<Mutex<Vec<Value>>>);
#[async_trait]
impl PluginFactory for Validate {
    fn prepare(&self, value: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if value == "invalid" {
            return Err(MetaError::InvalidInput("invalid fixture config".into()));
        }
        self.0.lock().unwrap().push(value.clone());
        Ok(PreparedActivation::new(value.clone()))
    }
    async fn activate(&self, _: ActivationPlan) -> rsi_meta::Result<()> {
        panic!("leaf preview must not activate")
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    catalog: ProfileCatalog,
    id: HostProfileId,
    path: std::path::PathBuf,
    host: rsi_host::Host,
    runtime: Runtime,
    observed: Arc<Mutex<Vec<Value>>>,
}
impl Fixture {
    fn new(source: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let catalog = ProfileCatalog::new(
            HostPaths::new(root.join("config"), root.join("state"), root.join("cache")).unwrap(),
        );
        let id = HostProfileId::new("leaf-fixture").unwrap();
        let path = catalog
            .copy_host(&HostProfileId::new("standard").unwrap(), &id)
            .unwrap();
        fs::write(&path, source).unwrap();
        let observed = Arc::new(Mutex::new(vec![]));
        let mut builder = HostBuilder::new(catalog.paths().clone());
        builder
            .register_linked(
                "fixture.validate",
                "1",
                UpdateMode::Replayable,
                Arc::new(Validate(observed.clone())),
            )
            .unwrap();
        Self {
            _temp: temp,
            catalog,
            id,
            path,
            host: builder.build().unwrap(),
            runtime: Runtime::new(RuntimeLimits::default()).unwrap(),
            observed,
        }
    }
    fn edit(
        &self,
        target: &str,
        change: HostLeafEdit<'_>,
    ) -> Result<rsi::ProfileEdit<'_>, ProfileEditError> {
        self.catalog
            .preview_host_leaf_edit(&self.host, &self.runtime, &self.id, target, change)
    }
}
#[tokio::test]
async fn imported_leaf_uses_prepared_exact_json_root_override_without_rewriting_includes() {
    let original =
        "format=1\n# preserve this comment\n[[steps]]\nkind='include'\npath='import.toml'\n";
    let f = Fixture::new(original);
    let child = f.path.with_file_name("import.toml");
    let imported = "format=1\n[[steps]]\nkind='plugin'\nid='target'\nplugin='fixture.validate'\nconfig={ value=1 }\n";
    fs::write(&child, imported).unwrap();
    let exact: Value = serde_json::from_str(r#"{"value":18446744073709551615,"decimal":1.0000000000000001,"secret":"do-not-print","null":null}"#).unwrap();
    let edit = f
        .edit("target", HostLeafEdit::Configuration(&exact))
        .unwrap();
    assert!(edit.is_prepared());
    assert!(!format!("{edit:?}").contains("do-not-print"));
    assert_eq!(*f.observed.lock().unwrap(), vec![exact.clone()]);
    assert_eq!(edit.effective().changes.as_ref().unwrap().len(), 1);
    assert_eq!(edit.effective().changes.as_ref().unwrap()[0].id, "target");
    assert!(
        std::str::from_utf8(edit.proposed_source())
            .unwrap()
            .contains("# preserve this comment")
    );
    assert_eq!(fs::read_to_string(&f.path).unwrap(), original);
    let receipt = edit.commit_once().unwrap();
    assert_eq!(
        receipt.source_digest,
        f.host.preview_file(&f.path).unwrap().source_digest
    );
    assert_eq!(fs::read_to_string(&child).unwrap(), imported);
    let stale = f.edit("target", HostLeafEdit::Enabled(false)).unwrap();
    let before = fs::read(&f.path).unwrap();
    fs::write(&child, imported.replace("value=1", "value=2")).unwrap();
    assert!(matches!(
        stale.commit_once(),
        Err(ProfileEditError::Conflict)
    ));
    assert_eq!(fs::read(&f.path).unwrap(), before);
    f.runtime.shutdown().await;
}
#[tokio::test]
async fn disabled_ancestors_and_non_leaf_targets_never_expand_edit_authority() {
    let f = Fixture::new(
        "format=1\n[[steps]]\nkind='group'\nid='disabled'\nenabled=false\n[[steps.nodes]]\nkind='plugin'\nid='target'\nplugin='fixture.validate'\n",
    );
    let before = fs::read(&f.path).unwrap();
    assert!(
        matches!(f.edit("target", HostLeafEdit::Enabled(true)), Err(ProfileEditError::DisabledAncestor(parent)) if parent == "disabled")
    );
    for target in [
        "disabled",
        "missing",
        " leading",
        "trailing ",
        "line\nbreak",
    ] {
        assert!(matches!(
            f.edit(target, HostLeafEdit::Enabled(false)),
            Err(ProfileEditError::NotLeaf)
        ));
    }
    assert!(
        f.edit("target", HostLeafEdit::Configuration(&json!("invalid")))
            .is_err()
    );
    let value = json!({"enabled-later":null});
    let edit = f
        .edit("target", HostLeafEdit::Configuration(&value))
        .unwrap();
    assert!(edit.is_prepared());
    assert!(edit.effective().leaves.is_empty());
    assert_eq!(*f.observed.lock().unwrap(), vec![value]);
    drop(edit);
    assert_eq!(fs::read(&f.path).unwrap(), before);
    f.runtime.shutdown().await;
}
#[tokio::test]
async fn inline_steps_preserve_comments_and_leaf_configuration_is_bounded_before_preparation() {
    let f = Fixture::new(
        "format=1\n# keep\nsteps=[{kind='plugin',id='target',plugin='fixture.validate'}]\n",
    );
    let before = fs::read(&f.path).unwrap();
    let mut nested = Value::Null;
    for _ in 0..33 {
        nested = json!([nested]);
    }
    for invalid in [json!("x".repeat(65537)), json!(vec![0; 4097]), nested] {
        assert!(matches!(
            f.edit("target", HostLeafEdit::Configuration(&invalid)),
            Err(ProfileEditError::ConfigurationBounds)
        ));
    }
    assert!(f.observed.lock().unwrap().is_empty());
    assert_eq!(fs::read(&f.path).unwrap(), before);
    let edit = f.edit("target", HostLeafEdit::Enabled(false)).unwrap();
    assert!(edit.effective().leaves.is_empty());
    edit.commit_once().unwrap();
    assert!(fs::read_to_string(&f.path).unwrap().contains("# keep"));
    f.runtime.shutdown().await;
}
