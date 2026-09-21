use rsi_tools_protocol::{ToolContent, ToolOutputDeclaration, TypedToolOutput};
use serde::Serialize;
use serde_json::{Value, json};

fn declaration(schema: Value) -> ToolOutputDeclaration {
    ToolOutputDeclaration::new("example.result", 1, schema).unwrap()
}

#[test]
fn declaration_digest_is_order_independent_and_verified_at_decode() {
    let first = declaration(
        json!({"type":"object", "properties":{"z":{"type":"integer"},"a":{"type":"string"}}}),
    );
    let second = declaration(
        json!({"properties":{"a":{"type":"string"},"z":{"type":"integer"}}, "type":"object"}),
    );
    assert_eq!(first, second);
    let wire = serde_json::to_value(&first).unwrap();
    assert_eq!(
        serde_json::from_value::<ToolOutputDeclaration>(wire.clone()).unwrap(),
        first
    );
    let mut tampered = wire.clone();
    tampered["schema"]["properties"]["a"]["type"] = json!("boolean");
    assert!(serde_json::from_value::<ToolOutputDeclaration>(tampered).is_err());
    let mut extra = wire;
    extra["extra"] = json!(true);
    assert!(serde_json::from_value::<ToolOutputDeclaration>(extra).is_err());
    assert!(ToolOutputDeclaration::new("example.result", 0, json!({})).is_err());
}

#[test]
fn schemas_reject_resource_resolution_recursion_and_unbounded_shapes() {
    for schema in [
        json!({"$ref":"https://localhost/private"}),
        json!({"properties":{"secret":{"$ref":"#"}}}),
        json!({"pattern":"(a+)+$"}),
        json!({"oneOf":[{},{}]}),
        json!({"type":"imaginary"}),
        json!({"description":"x".repeat(64 * 1024)}),
        json!({"enum":(0..1024).collect::<Vec<_>>()}),
    ] {
        assert!(ToolOutputDeclaration::new("test", 1, schema).is_err());
    }
    let mut deep = json!({});
    for _ in 0..33 {
        deep = json!({"items":deep});
    }
    assert!(ToolOutputDeclaration::new("test", 1, deep).is_err());
    // Annotation data and user property names are not schema keywords.
    declaration(
        json!({"type":"object","properties":{"$ref":{"type":"string"}},"examples":[{"$ref":"data"}]}),
    );
}

#[derive(Serialize)]
struct Count {
    count: u32,
}

#[test]
fn author_helper_uses_one_value_and_rejects_mismatches_without_leaking_it() {
    let output = TypedToolOutput::new(
        declaration(json!({
            "type":"object", "properties":{"count":{"type":"integer"}},
            "required":["count"], "additionalProperties":false
        })),
        |value: &Count| {
            vec![ToolContent::Text {
                text: format!("Count: {}", value.count),
            }]
        },
    );
    let result = output.result(&Count { count: 7 }).unwrap();
    assert_eq!(result.value, json!({"count":7}));
    assert_eq!(
        result.content,
        vec![ToolContent::Text {
            text: "Count: 7".into()
        }]
    );
    assert!(!result.is_error);
    let error = output
        .declaration()
        .validate_value(&json!({"count":"private-secret"}))
        .unwrap_err();
    assert!(!error.to_string().contains("private-secret"));
    assert!(
        output
            .declaration()
            .validate_value(&json!({"count":1,"extra":true}))
            .is_err()
    );
    let wrong = TypedToolOutput::new(declaration(json!({"type":"string"})), |_: &Count| {
        panic!("invalid output must not render")
    });
    assert!(wrong.result(&Count { count: 7 }).is_err());
}

#[test]
fn portable_v2_roundtrips_declarations_and_preserves_result_shape() {
    use rsi_tools_protocol::{
        ToolDefinition,
        portable::{self, Definition, Response, Scheduling},
    };
    let output = declaration(json!({"type":"string"}));
    let response = Response::Description {
        tools: vec![Definition {
            definition: ToolDefinition::new("echo", "Echo", json!({"type":"object"})).unwrap(),
            output: Some(output.clone()),
            timeout_ms: 1000,
            scheduling: Scheduling::Exclusive,
        }],
    };
    let Response::Description { tools } =
        portable::decode(&portable::encode(&response).unwrap()).unwrap()
    else {
        panic!()
    };
    assert_eq!(tools[0].output.as_ref(), Some(&output));
    assert_eq!(portable::VERSION, 2);
    assert!(
        serde_json::to_value(&tools[0].definition)
            .unwrap()
            .get("output")
            .is_none()
    );
    let result = rsi_tools_protocol::ToolResult::new(json!("ok"), vec![], false).unwrap();
    let wire = serde_json::to_value(result).unwrap();
    assert_eq!(wire.as_object().unwrap().len(), 4);
    assert!(wire.get("output").is_none());
}

#[test]
fn object_constants_and_enums_ignore_property_order_without_rewriting_output() {
    let original: Value = serde_json::from_str(r#"{"z":1,"a":{"z":2,"a":3}}"#).unwrap();
    for schema in [json!({"const":original}), json!({"enum":[original]})] {
        let contract = declaration(schema);
        contract.validate_value(&original).unwrap();
        let reordered: Value = serde_json::from_str(r#"{"a":{"a":3,"z":2},"z":1}"#).unwrap();
        contract.validate_value(&reordered).unwrap();
        assert!(
            contract
                .validate_value(&json!({"z":2,"a":{"z":2,"a":3}}))
                .is_err()
        );
        let output = TypedToolOutput::new(contract, |_: &Value| Vec::new());
        let result = output.result(&original).unwrap();
        assert_eq!(
            serde_json::to_string(&result.value).unwrap(),
            serde_json::to_string(&original).unwrap()
        );
    }
}
