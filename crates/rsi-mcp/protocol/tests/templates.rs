use rsi_mcp_protocol::{McpResourceTemplate, TemplateCatalog, TemplateParameters};
use serde_json::{Value, json};

fn template(text: &str) -> McpResourceTemplate {
    serde_json::from_value(json!({"uriTemplate":text,"name":"fixture","_meta":{"complete":true}}))
        .unwrap()
}

#[test]
fn rfc6570_uri_operators_lists_maps_prefixes_and_missing_values() {
    let parameters = TemplateParameters::from_value(
        json!({"path":"/a/b","q":"hello world","list":["red","green"],"map":{"a":"one","b":"two"}}),
    )
    .unwrap();
    let values =
        json!({"path":"/a/b","q":"hello world","list":["red","green"],"map":{"a":"one","b":"two"}});
    for (source, names, expected) in [
        (
            "fixture:{+path}{?q}",
            vec!["path", "q"],
            "fixture:/a/b?q=hello%20world",
        ),
        ("fixture:{#path}", vec!["path"], "fixture:#/a/b"),
        ("fixture:{.list*}", vec!["list"], "fixture:.red.green"),
        ("fixture:{/list*}", vec!["list"], "fixture:/red/green"),
        ("fixture:{;map*}", vec!["map"], "fixture:;a=one;b=two"),
        ("fixture:{?map*}", vec!["map"], "fixture:?a=one&b=two"),
        (
            "fixture:{&list*}",
            vec!["list"],
            "fixture:&list=red&list=green",
        ),
        ("fixture:{q:5}", vec!["q"], "fixture:hello"),
        ("fixture:{?missing}", vec![], "fixture:"),
    ] {
        let selected = names
            .into_iter()
            .map(|name| (name.to_owned(), values[name].clone()))
            .collect::<serde_json::Map<_, _>>();
        assert_eq!(
            template(source)
                .expand(&TemplateParameters::from_value(Value::Object(selected)).unwrap())
                .unwrap(),
            expected
        );
    }
    assert!(template("fixture:{q}").expand(&parameters).is_err());
    assert_eq!(
        template("fixture:{+path}")
            .expand(&TemplateParameters::from_value(json!({"path":"../outside"})).unwrap())
            .unwrap(),
        "fixture:../outside"
    );
}

#[test]
fn unknown_names_types_leaf_and_encoded_budgets_are_rejected() {
    for value in [
        json!({"a":true}),
        json!({"a":null}),
        json!({"a":[["nested"]]}),
        json!({"a/b":"bad"}),
        json!({"a":"x".repeat(4097)}),
        json!({"a":vec!["";257]}),
        json!({"a":vec!["x".repeat(4096);17]}),
    ] {
        assert!(TemplateParameters::from_value(value).is_err());
    }
    let too_many = (0..33)
        .map(|index| (format!("v{index}"), json!("")))
        .collect::<serde_json::Map<_, _>>();
    assert!(TemplateParameters::from_value(Value::Object(too_many)).is_err());
    let params = TemplateParameters::from_value(json!({"q":"é".repeat(1000)})).unwrap();
    assert!(
        template("fixture:{q}").expand(&params).is_err(),
        "URI percent encoding crosses 4096 bytes"
    );
    let params = TemplateParameters::from_value(json!({"q":"a".repeat(4088)})).unwrap();
    assert_eq!(template("fixture:{q}").expand(&params).unwrap().len(), 4096);
}

#[test]
fn one_bad_or_duplicate_template_rejects_the_complete_catalog() {
    for templates in [
        vec![template("fixture:{q}"), template("fixture:{bad")],
        vec![template("fixture:{q}"); 2],
    ] {
        assert!(TemplateCatalog::Available { templates }.validate().is_err());
    }
    assert_ne!(
        serde_json::to_value(TemplateCatalog::Unsupported).unwrap(),
        serde_json::to_value(TemplateCatalog::Available { templates: vec![] }).unwrap()
    );
}
