use super::*;
use rsi_agent_session_protocol::{EffectId, TurnId};
use rsi_tools_protocol::{ToolResult, ToolResultIdentity};
use serde_json::json;
fn facts() -> (Entry, SessionFact, SessionFact) {
    let identity = ToolResultIdentity::new("owner", "invoke", "call", "a".repeat(64)).unwrap();
    let intent = SessionFact::new(
        3,
        1,
        SessionFactBody::ToolIntent {
            turn_id: TurnId::new("turn").unwrap(),
            effect_id: EffectId::new("effect").unwrap(),
            source_model_effect_id: EffectId::new("model").unwrap(),
            identity: identity.clone(),
            name: "web_search".into(),
            arguments: json!({"query":"a question"}),
            approval: None,
            parallel_safe: false,
        },
    )
    .unwrap();
    let result=SessionFact::new(5,1,SessionFactBody::ToolResult{turn_id:TurnId::new("turn").unwrap(),effect_id:EffectId::new("effect").unwrap(),identity,result:ToolResult::new(json!({"version":1,"operation":"search","request":"a question","sources":[{"url":"https://example.com","title":"<script>literal title</script>","text":"external <b>data</b>","published_at":null,"truncated":false}],"omitted":0,"truncated":false}),vec![],false).unwrap()}).unwrap();
    (
        Entry {
            intent: SourceRef {
                seq: 3,
                field: FactField::ToolArguments,
            },
            result: SourceRef {
                seq: 5,
                field: FactField::ToolValue,
            },
            index: 0,
            offset: 0,
        },
        intent,
        result,
    )
}
#[test]
fn source_projection_requires_exact_intent_request_operation_and_full_result_identity() {
    let (entry, intent, result) = facts();
    let actual = resolve(
        &entry,
        std::slice::from_ref(&intent),
        std::slice::from_ref(&result),
    )
    .unwrap();
    let view = card(&entry, &actual);
    view.validate().unwrap();
    assert!(
        matches!(&view.elements[2],UiElement::Field{label,..} if label=="<script>literal title</script>")
    );
    for identity in [
        ToolResultIdentity::new("other", "invoke", "call", "a".repeat(64)).unwrap(),
        ToolResultIdentity::new("owner", "other", "call", "a".repeat(64)).unwrap(),
        ToolResultIdentity::new("owner", "invoke", "other", "a".repeat(64)).unwrap(),
        ToolResultIdentity::new("owner", "invoke", "call", "b".repeat(64)).unwrap(),
    ] {
        let mut body = result.body().clone();
        if let SessionFactBody::ToolResult {
            identity: target, ..
        } = &mut body
        {
            *target = identity;
        }
        assert!(
            resolve(
                &entry,
                std::slice::from_ref(&intent),
                &[SessionFact::new(5, 1, body).unwrap()]
            )
            .is_err()
        );
    }
    for mode in 0..7 {
        let mut body = result.body().clone();
        if let SessionFactBody::ToolResult {
            turn_id,
            effect_id,
            result,
            ..
        } = &mut body
        {
            match mode {
                0 => *turn_id = TurnId::new("other").unwrap(),
                1 => *effect_id = EffectId::new("other").unwrap(),
                2 => result.is_error = true,
                3 => result.value["request"] = json!("different query"),
                4 => result.value["operation"] = json!("fetch"),
                5 => result.value["version"] = json!(2),
                _ => result.value["sources"][0]["url"] = json!("javascript:alert(1)"),
            }
        }
        assert!(
            resolve(
                &entry,
                std::slice::from_ref(&intent),
                &[SessionFact::new(5, 1, body).unwrap()]
            )
            .is_err(),
            "case {mode}"
        );
    }
    let mut body = intent.body().clone();
    if let SessionFactBody::ToolIntent { name, .. } = &mut body {
        *name = "mcp__server__web_search".into();
    }
    assert!(
        resolve(
            &entry,
            &[SessionFact::new(3, 1, body).unwrap()],
            std::slice::from_ref(&result)
        )
        .is_err()
    );
    assert!(
        resolve(
            &Entry {
                result: SourceRef {
                    seq: 6,
                    ..entry.result
                },
                ..entry.clone()
            },
            &[intent],
            &[result]
        )
        .is_err()
    );
}
