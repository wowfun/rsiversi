use rsi_ui_protocol::{RendererCatalog, RendererFile};
use serde_json::{Value, json};

fn catalog() -> RendererCatalog {
    serde_json::from_value(json!({"format":1,"renderers":[{
        "id":"fixture.renderer", "abi":1, "entry":"main.js",
        "files":[{"name":"main.js", "sha256":"a".repeat(64)}],
        "schemas":[{"name":"fixture.model", "version":1}],
        "capabilities":["invoke","source","clipboard","focus"],
        "surfaces":["root","pane","sidebar","dialog"]
    }]}))
    .unwrap()
}

#[test]
fn renderer_admission_rejects_invalid_identity_import_graph_and_authority() {
    let valid = catalog();
    valid.validate().unwrap();
    let base = serde_json::to_value(&valid).unwrap();
    let renderer = &base["renderers"][0];
    for (path, bad) in [
        ("/format", json!(2)),
        ("/renderers", json!([])),
        ("/renderers", json!(vec![renderer; 33])),
        ("/renderers", json!([renderer, renderer])),
        ("/renderers/0/id", json!("../renderer")),
        ("/renderers/0/abi", json!(2)),
        ("/renderers/0/entry", json!("missing.js")),
        ("/renderers/0/entry", json!("main.css")),
        ("/renderers/0/files", json!([])),
        (
            "/renderers/0/files",
            json!(vec![&renderer["files"][0]; 129]),
        ),
        (
            "/renderers/0/files",
            json!([renderer["files"][0], renderer["files"][0]]),
        ),
        ("/renderers/0/files/0/name", json!("../main.js")),
        ("/renderers/0/files/0/name", json!(".hidden.js")),
        ("/renderers/0/files/0/sha256", json!("A".repeat(64))),
        ("/renderers/0/files/0/sha256", json!("g".repeat(64))),
        ("/renderers/0/files/0/sha256", json!("a".repeat(63))),
        ("/renderers/0/schemas", json!([])),
        (
            "/renderers/0/schemas",
            json!(vec![&renderer["schemas"][0]; 33]),
        ),
        (
            "/renderers/0/schemas",
            json!([renderer["schemas"][0], renderer["schemas"][0]]),
        ),
        ("/renderers/0/schemas/0/name", json!("bad/model")),
        ("/renderers/0/schemas/0/version", json!(0)),
        ("/renderers/0/capabilities", json!(["runtime"])),
        ("/renderers/0/capabilities", json!(["invoke", "invoke"])),
        ("/renderers/0/capabilities", json!(vec!["invoke"; 9])),
        ("/renderers/0/surfaces", json!([])),
        ("/renderers/0/surfaces", json!(["pane", "pane"])),
        ("/renderers/0/surfaces", json!(vec!["pane"; 5])),
    ] {
        let mut input = base.clone();
        *input.pointer_mut(path).unwrap() = bad;
        let decoded: RendererCatalog = serde_json::from_value(input).unwrap();
        assert!(decoded.validate().is_err(), "admitted {path}: {decoded:?}");
    }
    for path in [
        "",
        "/renderers/0",
        "/renderers/0/files/0",
        "/renderers/0/schemas/0",
    ] {
        let mut input = base.clone();
        input
            .pointer_mut(path)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("authority".into(), Value::Bool(true));
        assert!(
            serde_json::from_value::<RendererCatalog>(input).is_err(),
            "unknown field admitted at {path}"
        );
    }
}

#[test]
fn catalog_encoding_is_bounded_even_when_each_renderer_is_valid() {
    let mut catalog = catalog();
    catalog.renderers[0]
        .files
        .extend((0..127).map(|index| RendererFile {
            name: format!("chunk-{index}.js"),
            sha256: "0".repeat(64),
        }));
    catalog.validate().unwrap();
    catalog.renderers = (0..32)
        .map(|index| {
            let mut renderer = catalog.renderers[0].clone();
            renderer.id = format!("renderer-{index}");
            renderer
        })
        .collect();
    assert!(serde_json::to_vec(&catalog).unwrap().len() > rsi_ui_protocol::MAXIMUM_VIEW_BYTES);
    assert!(catalog.validate().is_err());
}

#[test]
fn leading_dot_is_a_nominal_identity_but_not_an_asset_filename() {
    let mut value = catalog();
    value.renderers[0].id = ".renderer".into();
    value.renderers[0].schemas[0].name = ".schema".into();
    value.validate().unwrap();
    assert!(rsi_ui_protocol::name_valid(".action"));
    assert!(!rsi_ui_protocol::asset_name_valid(".module.js"));
}
