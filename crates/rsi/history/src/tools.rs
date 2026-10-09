use super::{Arc, HistoryContract, ProductHistorySearch, Reply, Request};
use async_trait::async_trait;
use rsi_agent_turn_protocol::AgentCallerAuthority;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_tools_protocol::{
    ToolContent, ToolDefinition, ToolError, ToolExecution, ToolExecutor, ToolOutputDeclaration,
    ToolRegistrarContract, ToolRegistration, ToolResult, ToolTimeoutPolicy, TypedToolOutput,
};
use serde_json::{Value, json};

/// Explicit history tools use the same owner and the caller's immutable workspace.
#[derive(Clone, Debug, Default)]
pub struct HistoryToolsFactory;
fn meta(e: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(e.to_string())
}
#[async_trait]
impl PluginFactory for HistoryToolsFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(meta("history Tool config must be null"));
        }
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<HistoryContract>()
            .requiring_local::<ToolRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let declaration=ToolOutputDeclaration::new("rsi.history",2,json!({"type":"object","required":["kind"],"properties":{"kind":{"type":"string","enum":["coverage","hits","original","frozen","matches","progress","stale"]},"progress":{"type":"object"},"sources":{"type":"array"},"matches":{"type":"array"},"reason":{"type":"string"},"coverage":{"type":"object"},"hits":{"type":"array"},"next":{},"hit":{"type":"object"},"offset":{"type":"integer"},"next_offset":{"type":"integer"},"has_more":{"type":"boolean"},"text":{"type":"string"},"reference":{"type":"object"}},"additionalProperties":false})).map_err(meta)?;
        let output = TypedToolOutput::new(declaration, |reply: &Reply| {
            vec![ToolContent::Text {
                text: serde_json::to_string(reply).expect("bounded history reply"),
            }]
        });
        let definition = ToolDefinition::new("history_search", "Search saved history in the caller's workspace without knowing conversation IDs. All arguments are top-level: operation, conversation, query, after, hit, offset, start, end. discover advances one finite metadata/source batch; reuse progress.continuation as after on discover. query searches the partial index; progress reports coverage. Do not wait for every live source to catch up before querying. conversation optionally narrows discover/query/progress, and is required for read/freeze/rebuild. For read, copy ONLY matches[].hit unchanged, and set conversation to matches[].scope.conversation; offset defaults to 0. Do not copy the whole match object into hit. freeze saves an exact original range for this Session. Sources exclude reasoning and provider requests. Never claim unindexed history has no matches.", json!({
            "type":"object", "required":["operation"],
            "properties": {
                "operation":{"type":"string","enum":["discover","query","progress","read","freeze","rebuild"]},
                "conversation":{"description":"Copy matches[].scope.conversation; required for read/freeze/rebuild.","type":"object","required":["kind","id"],"properties":{"kind":{"enum":["native","external"]},"id":{"type":"string","maxLength":256}},"additionalProperties":false},
                "query":{"type":"string","maxLength":256},
                "after":{"description":"Discover: progress.continuation. Query/progress: their own next cursor. Never exchange these cursors."},
                "hit":{"description":"Only matches[].hit unchanged, excluding the match label/scope/reference_allowed wrapper.","type":"object","required":["source","original","preview"],"properties":{"source":{"type":"object"},"original":{"type":"object"},"preview":{"type":"string"}},"additionalProperties":false},
                "offset":{"type":"integer","minimum":0,"maximum":1_048_576,"default":0},
                "start":{"type":"integer","minimum":0,"maximum":1_048_576},
                "end":{"type":"integer","minimum":1,"maximum":1_048_576}
            },
            "allOf":[
                {"if":{"properties":{"operation":{"const":"query"}}},"then":{"required":["query"]}},
                {"if":{"properties":{"operation":{"enum":["read","freeze","rebuild"]}}},"then":{"required":["conversation"]}},
                {"if":{"properties":{"operation":{"enum":["read","freeze"]}}},"then":{"required":["hit"]}},
                {"if":{"properties":{"operation":{"const":"freeze"}}},"then":{"required":["start","end"]}}
            ],
            "additionalProperties":false
        })).map_err(meta)?;
        let lease = plan
            .local::<ToolRegistrarContract>()?
            .register_batch(vec![ToolRegistration {
                definition,
                output: Some(output.declaration().clone()),
                timeout: ToolTimeoutPolicy::Execution { timeout_ms: 30_000 },
                executor: Arc::new(Executor {
                    owner: plan.local::<HistoryContract>()?,
                    output,
                }),
            }])
            .map_err(meta)?;
        plan.defer(
            "release history Tool",
            Box::new(move || Box::pin(async move { lease.retire().map_err(|e| e.to_string()) })),
        )
    }
}
struct Executor {
    owner: Arc<ProductHistorySearch>,
    output: TypedToolOutput<Reply>,
}
impl std::fmt::Debug for Executor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryTool").finish_non_exhaustive()
    }
}
#[async_trait]
impl ToolExecutor for Executor {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let caller = execution
            .extension::<AgentCallerAuthority>()
            .ok_or_else(|| {
                ToolError::InvalidInput("history requires a native Agent caller".into())
            })?;
        if serde_json::to_vec(&arguments)
            .map_err(|e| ToolError::InvalidInput(e.to_string()))?
            .len()
            > 16384
        {
            return Err(ToolError::InvalidInput(
                "history request exceeds 16 KiB".into(),
            ));
        }
        let request = request(
            arguments,
            &rsi_workspace_protocol::WorkspaceId::from_coordinates(caller.header().coordinates()),
            caller.session_id(),
        )?;
        match self
            .owner
            .call(
                super::HistoryAuthority::Agent(caller.clone()),
                request,
                execution.cancellation.clone(),
            )
            .await
        {
            Ok(mut reply) => {
                // Keep worst-case escaped original text inside the Tool output's 256 KiB bound.
                if let Reply::Original {
                    hit,
                    offset,
                    next_offset,
                    has_more,
                    text,
                } = &mut reply
                {
                    let end = text.floor_char_boundary(text.len().min(32 * 1024));
                    text.truncate(end);
                    *next_offset = *offset + end;
                    *has_more = *next_offset < hit.original.end;
                }
                self.output.result(&reply)
            }
            Err(error) => ToolResult::new(
                json!({"error":"history_unavailable"}),
                vec![ToolContent::Text {
                    text: error.to_string(),
                }],
                true,
            ),
        }
    }
}

fn request(
    mut input: Value,
    workspace: &rsi_workspace_protocol::WorkspaceId,
    target: &rsi_agent_session_protocol::SessionId,
) -> rsi_tools_protocol::Result<Request> {
    let object = input
        .as_object_mut()
        .ok_or_else(|| ToolError::InvalidInput("history arguments must be an object".into()))?;
    if object.contains_key("scope") || object.contains_key("target") {
        return Err(ToolError::InvalidInput(
            "history scope and target come from the actual caller".into(),
        ));
    }
    let conversation = object
        .remove("conversation")
        .map(serde_json::from_value::<rsi_history_api::ConversationIdentity>)
        .transpose()
        .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
    let operation = object
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let broad = match operation.as_str() {
        "discover" | "query" | "progress" => true,
        "read" | "freeze" | "rebuild" => false,
        _ => {
            return Err(ToolError::InvalidInput(
                "unsupported history Tool operation".into(),
            ));
        }
    };
    let scope = if broad {
        match conversation {
            Some(conversation) => {
                json!({"kind":"conversation","source":{"workspace":workspace,"conversation":conversation}})
            }
            None => json!({"kind":"workspace","workspace":workspace}),
        }
    } else {
        let conversation = conversation.ok_or_else(|| {
            ToolError::InvalidInput("this operation requires an exact conversation".into())
        })?;
        json!({"workspace":workspace,"conversation":conversation})
    };
    object.insert("scope".into(), scope);
    if operation == "read" {
        object.entry("offset").or_insert(json!(0));
    }
    if operation == "freeze" {
        object.insert("target".into(), json!(target));
    }
    serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_indexing_uses_discovery_continuations_and_originals_require_an_exact_source() {
        let workspace = rsi_workspace_protocol::WorkspaceId::parse("a".repeat(64)).unwrap();
        let target = rsi_agent_session_protocol::SessionId::new("target").unwrap();
        let resolve = |value| request(serde_json::from_value(value).unwrap(), &workspace, &target);
        let continuation = "b".repeat(32);
        let Request::Discover { scope, after } =
            resolve(json!({"operation":"discover","after":continuation})).unwrap()
        else {
            panic!("discovery")
        };
        assert_eq!(
            scope,
            rsi_history_api::QueryScope::Workspace {
                workspace: workspace.clone()
            }
        );
        assert_eq!(after, Some(continuation));
        let Request::Query { scope, .. } =
            resolve(json!({"operation":"query","query":"needle"})).unwrap()
        else {
            panic!("query")
        };
        assert_eq!(
            scope,
            rsi_history_api::QueryScope::Workspace {
                workspace: workspace.clone()
            }
        );
        let exact = json!({"kind":"native","id":"source"});
        assert!(matches!(
            resolve(json!({"conversation":exact,"operation":"discover"})).unwrap(),
            Request::Discover {
                scope: rsi_history_api::QueryScope::Conversation { .. },
                ..
            }
        ));
        assert!(resolve(json!({"conversation":exact,"operation":"rebuild"})).is_ok());
        let hit = json!({"source":{"kind":"native","binding":{"session_id":"source","header_sha256":"c".repeat(64)}},"original":{"record":{"sequence":"1","kind":"human","content_index":0},"through_seq":"1","start":0,"end":6,"text_sha256":"d".repeat(64),"scanned_bytes":6},"preview":"needle"});
        assert!(matches!(
            resolve(json!({"operation":"read","conversation":exact,"hit":hit})).unwrap(),
            Request::Read { offset: 0, .. }
        ));
        assert!(
            resolve(
                json!({"operation":"read","conversation":exact,"hit":{"label":"wrapper","hit":hit}})
            )
            .is_err()
        );
        assert!(resolve(json!({"request":{"operation":"discover"}})).is_err());
        for operation in ["advance", "search"] {
            assert!(resolve(json!({"conversation":exact,"operation":operation})).is_err());
        }
        for operation in ["read", "freeze", "rebuild", "advance", "search"] {
            assert!(
                resolve(json!({"operation":operation})).is_err(),
                "{operation}"
            );
        }
        assert!(
            resolve(json!({"operation":"discover","scope":{"kind":"accessible_host"}})).is_err()
        );
        assert!(resolve(json!({"operation":"discover","target":"foreign"})).is_err());
    }
}
