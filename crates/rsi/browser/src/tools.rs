use crate::{SessionAuthority, SessionBrowserContract, SessionOperation};
use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    ContributionContext, ContributionKind, ContributionRegistrarContract, ContributionRegistration,
    ContributionResult, ToolPolicy, ToolPolicyDecision, ToolPolicyRequest,
};
use rsi_agent_session_protocol::ContributionId;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_tools_protocol::{
    ToolContent, ToolDefinition, ToolError, ToolExecution, ToolExecutor, ToolRegistrarContract,
    ToolRegistration, ToolResult, ToolTimeoutPolicy,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
fn meta(e: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(e.to_string())
}
#[derive(Clone, Debug, Default)]
pub struct SessionBrowserToolsFactory;
#[async_trait]
impl PluginFactory for SessionBrowserToolsFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(meta("browser Tool config must be null"));
        }
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<ToolRegistrarContract>()
            .requiring_local::<ContributionRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let Some(owner) = plan.context().lookup_local::<SessionBrowserContract>() else {
            return Ok(());
        };
        let policy = plan
            .local::<ContributionRegistrarContract>()?
            .register(
                &plan.context().registration_context()?,
                ContributionRegistration::new(
                    ContributionId::new("rsi.browser.local-approval").map_err(meta)?,
                    20,
                    ContributionKind::ToolPolicy(Arc::new(LocalApproval)),
                ),
            )
            .map_err(meta)?;
        let definition=ToolDefinition::new("session_browser","Operate the one browser shared with the user in this native Local Session. status takes only operation and reads live state without extending its life. Open takes policy and url, choosing anonymous public_web HTTPS/443 or local_dev with one exact HTTP loopback origin; local_dev requires human Approval. Copy binding from results. Only click/fill take document_version, observation_id and node IDs from the latest observation. observe/screenshot/close take only operation and binding. observe returns at most 256 current nodes. Stale refusals and invalid_input with not_dispatched did not dispatch. Busy is not started: wait retry_after_ms. Navigate, click and fill are never automatically replayed after uncertain outcomes. Screenshot returns durable image evidence. Close/reopen is required to change policy. No arbitrary scripts, selectors, files or login profiles.",input_schema()).map_err(meta)?;
        let lease = plan
            .local::<ToolRegistrarContract>()?
            .register_batch(vec![ToolRegistration {
                definition,
                output: None,
                timeout: ToolTimeoutPolicy::Execution { timeout_ms: 65_000 },
                executor: Arc::new(Executor { owner }),
            }])
            .map_err(meta)?;
        plan.defer(
            "withdraw Session browser Tools",
            Box::new(move || {
                Box::pin(async move {
                    lease.retire().map_err(|e| e.to_string())?;
                    drop(policy);
                    Ok(())
                })
            }),
        )
    }
}
#[derive(Debug)]
struct LocalApproval;
#[async_trait]
impl ToolPolicy for LocalApproval {
    async fn decide(
        &self,
        _: &ContributionContext,
        request: &ToolPolicyRequest<'_>,
        _: CancellationToken,
    ) -> ContributionResult<ToolPolicyDecision> {
        if request.name == "session_browser"
            && request.arguments["operation"] == "open"
            && request.arguments["policy"]["mode"] == "local_dev"
        {
            Ok(ToolPolicyDecision::RequireApproval)
        } else {
            Ok(ToolPolicyDecision::Abstain)
        }
    }
}
#[derive(Debug)]
struct Executor {
    owner: Arc<crate::SessionBrowser>,
}
#[async_trait]
impl ToolExecutor for Executor {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let caller = execution
            .extension::<rsi_agent_turn_protocol::AgentCallerAuthority>()
            .ok_or_else(|| {
                ToolError::InvalidInput("browser requires an actual native Agent caller".into())
            })?;
        let operation = match model_operation(arguments)? {
            Ok(operation) => operation,
            Err(result) => return Ok(result),
        };
        let screenshot = matches!(operation, SessionOperation::Screenshot { .. });
        match self
            .owner
            .call(
                SessionAuthority::Agent(caller),
                operation,
                execution.cancellation,
            )
            .await
        {
            Ok(reply) => {
                let mut content = vec![ToolContent::Text {
                    text: serde_json::to_string(&reply).expect("bounded browser result"),
                }];
                if let Some(media) = reply.screenshot.as_ref().filter(|_| screenshot) {
                    content.push(ToolContent::Image {
                        media: media.clone(),
                    });
                }
                ToolResult::new(
                    serde_json::to_value(&reply).expect("bounded browser result"),
                    content,
                    false,
                )
            }
            Err(error) => ToolResult::new(
                json!({"status":"unavailable","do_not_replay":true}),
                vec![ToolContent::Text { text: error }],
                true,
            ),
        }
    }
}

fn input_schema() -> Value {
    let identity = json!({"type":"string","pattern":"^[a-f0-9]{32}$"});
    let fields = json!({
        "binding":{"type":"object","required":["service_epoch","browser_id"],"properties":{"service_epoch":identity,"browser_id":identity},"additionalProperties":false},
        "policy":{"oneOf":[
            {"type":"object","required":["mode"],"properties":{"mode":{"const":"public_web"}},"additionalProperties":false},
            {"type":"object","required":["mode","origin"],"properties":{"mode":{"const":"local_dev"},"origin":{"type":"string","maxLength":8192,"description":"Canonical HTTP loopback origin, at most 8192 UTF-8 bytes."}},"additionalProperties":false}
        ]},
        "url":{"type":"string","minLength":1,"maxLength":8192,"description":"Navigation URL, at most 8192 UTF-8 bytes."},
        "document_version":{"type":"string","pattern":"^[1-9][0-9]{0,19}$"},
        "observation_id":identity,
        "node":{"type":"string","pattern":"^(?:[1-9]|[1-9][0-9]|1[0-9]{2}|2[0-4][0-9]|25[0-6])$"},
        "text":{"type":"string","maxLength":16384,"description":"Fill value, at most 16384 UTF-8 bytes; non-ASCII characters may consume several bytes."},
        "direction":{"enum":["up","down"]}
    });
    let operations: &[(&str, &[&str])] = &[
        ("status", &[]),
        ("open", &["policy", "url"]),
        ("navigate", &["binding", "url"]),
        ("observe", &["binding"]),
        (
            "click",
            &["binding", "document_version", "observation_id", "node"],
        ),
        (
            "fill",
            &[
                "binding",
                "document_version",
                "observation_id",
                "node",
                "text",
            ],
        ),
        ("scroll", &["binding", "direction"]),
        ("screenshot", &["binding"]),
        ("close", &["binding"]),
    ];
    let cases: Vec<_> = operations.iter().map(|(operation, names)| {
        let mut properties = serde_json::Map::new();
        properties.insert("operation".into(), json!({"const":operation}));
        let mut required = vec!["operation"];
        for name in *names {
            properties.insert((*name).into(), fields[name].clone());
            required.push(name);
        }
        json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
    }).collect();
    json!({"type":"object","oneOf":cases})
}

fn model_operation(
    arguments: Value,
) -> rsi_tools_protocol::Result<Result<SessionOperation, ToolResult>> {
    let parsed = serde_json::from_value::<SessionOperation>(arguments)
        .map_err(|error| error.to_string())
        .and_then(|operation| {
            operation.validate()?;
            Ok(operation)
        });
    match parsed {
        Ok(operation) => Ok(Ok(operation)),
        Err(error) => {
            let detail: String = error
                .chars()
                .filter(|c| !c.is_control())
                .take(256)
                .collect();
            ToolResult::new(
                json!({"status":"invalid_input","not_dispatched":true}),
                vec![ToolContent::Text { text: format!("Browser request was not dispatched: {detail}. Correct the arguments using this operation's schema; only click/fill accept observation tokens.") }],
                true,
            ).map(Err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_model_arguments_return_an_error_result_before_admission() {
        let binding = json!({"service_epoch":"a".repeat(32),"browser_id":"b".repeat(32)});
        for arguments in [
            json!({"operation":"screenshot","binding":binding,"document_version":"2","observation_id":"c".repeat(32)}),
            json!({"operation":"observe"}),
            json!({"operation":"scroll","binding":binding,"direction":"sideways"}),
            json!({"operation":"open","policy":{"mode":"public_web","origin":"http://127.0.0.1:8080"},"url":"https://public.example/"}),
        ] {
            let Err(result) = model_operation(arguments).unwrap() else {
                panic!("invalid model input was admitted")
            };
            assert!(result.is_error);
            assert_eq!(result.value["status"], "invalid_input");
            assert_eq!(result.value["not_dispatched"], true);
        }
    }

    #[test]
    fn schema_and_decoder_require_each_operations_own_fields() {
        let schema = input_schema();
        ToolDefinition::new("session_browser", "test", schema.clone()).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let binding = json!({"service_epoch":"a".repeat(32),"browser_id":"b".repeat(32)});
        for arguments in [
            json!({"operation":"status"}),
            json!({"operation":"open","policy":{"mode":"public_web"},"url":"https://public.example/"}),
            json!({"operation":"open","policy":{"mode":"local_dev","origin":"http://127.0.0.1:8080"},"url":"http://127.0.0.1:8080/"}),
            json!({"operation":"navigate","binding":binding,"url":"https://public.example/"}),
            json!({"operation":"observe","binding":binding}),
            json!({"operation":"click","binding":binding,"document_version":"2","observation_id":"c".repeat(32),"node":"2"}),
            json!({"operation":"fill","binding":binding,"document_version":"2","observation_id":"c".repeat(32),"node":"1","text":"Bob"}),
            json!({"operation":"scroll","binding":binding,"direction":"down"}),
            json!({"operation":"screenshot","binding":binding}),
            json!({"operation":"close","binding":binding}),
        ] {
            assert!(validator.is_valid(&arguments), "{arguments}");
            assert!(model_operation(arguments.clone()).unwrap().is_ok());
            for name in arguments.as_object().unwrap().keys() {
                let mut missing = arguments.clone();
                missing.as_object_mut().unwrap().remove(name);
                assert!(!validator.is_valid(&missing));
                assert!(model_operation(missing).unwrap().is_err());
            }
            let mut extra = arguments;
            extra["unrelated"] = json!("ignored");
            assert!(!validator.is_valid(&extra));
            assert!(model_operation(extra).unwrap().is_err());
        }
        assert!(!validator.is_valid(&json!({"operation":"screenshot","binding":binding,"document_version":"2","observation_id":"c".repeat(32)})));
    }

    #[test]
    fn argument_diagnostics_are_safe_and_bounded() {
        let arguments = json!({"operation":"status",format!("\u{0000}{}", "界".repeat(4000)):true});
        let Err(result) = model_operation(arguments).unwrap() else {
            panic!("unexpected model admission")
        };
        let ToolContent::Text { text } = &result.content[0] else {
            panic!("expected argument diagnostic")
        };
        assert!(text.len() < 2048);
        assert!(!text.chars().any(char::is_control));
    }

    #[test]
    fn node_schema_matches_the_admitted_range_and_unicode_fill_errors_name_the_byte_bound() {
        let validator = jsonschema::validator_for(&input_schema()).unwrap();
        let base = json!({"operation":"fill","binding":{"service_epoch":"a".repeat(32),"browser_id":"b".repeat(32)},"document_version":"1","observation_id":"c".repeat(32),"node":"256","text":""});
        for node in ["1", "99", "100", "249", "256"] {
            let mut arguments = base.clone();
            arguments["node"] = json!(node);
            assert!(validator.is_valid(&arguments));
            assert!(model_operation(arguments).unwrap().is_ok());
        }
        for node in ["0", "01", "257", "999"] {
            let mut arguments = base.clone();
            arguments["node"] = json!(node);
            assert!(!validator.is_valid(&arguments));
            assert!(model_operation(arguments).unwrap().is_err());
        }
        let mut arguments = base;
        arguments["text"] = json!("界".repeat(6000));
        assert!(validator.is_valid(&arguments));
        let Err(result) = model_operation(arguments).unwrap() else {
            panic!("oversized fill admitted")
        };
        assert!(
            matches!(&result.content[0], ToolContent::Text {text} if text.contains("16384 UTF-8 bytes"))
        );
    }
}
