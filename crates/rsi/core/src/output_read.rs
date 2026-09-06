//! Standard model presentation over read-only completed Process output.

use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_process::{
    DEFAULT_OUTPUT_READ_BYTES, MAXIMUM_OUTPUT_READ_BYTES, ProcessOutputCache,
    ProcessOutputCacheContract,
};
use rsi_tools_protocol::{
    ToolContent, ToolDefinition, ToolError, ToolExecution, ToolExecutor, ToolRegistrarContract,
    ToolRegistration, ToolResult, ToolScheduling,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct OutputReadToolFactory;

#[async_trait]
impl PluginFactory for OutputReadToolFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "output_read configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::with_state(Value::Null, (), 0)
            .requiring_local::<ProcessOutputCacheContract>()
            .requiring_local::<ToolRegistrarContract>())
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let (): () = plan.take_state()?;
        let cache = plan.local::<ProcessOutputCacheContract>()?;
        let definition = ToolDefinition::new("output_read",
            "Read a page of a completed command's full output using its full_output identity. Offsets count raw bytes. Cache entries can expire; this tool cannot read arbitrary paths.",
            json!({"type":"object","properties":{
                "id":{"type":"string","pattern":"^[0-9a-f]{32}$","minLength":32,"maxLength":32},
                "offset":{"type":"integer","minimum":0},
                "limit":{"type":"integer","minimum":1,"maximum":MAXIMUM_OUTPUT_READ_BYTES}
            },"required":["id"],"additionalProperties":false}))
            .map_err(|error| MetaError::Activation(error.to_string()))?
            .with_scheduling(ToolScheduling::ParallelSafe);
        let lease = plan
            .local::<ToolRegistrarContract>()?
            .register_batch(vec![ToolRegistration {
                definition,
                timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 10_000 },
                executor: Arc::new(OutputReadTool(cache)),
            }])
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "release output read contribution",
            Box::new(move || {
                Box::pin(async move { lease.retire().map_err(|error| error.to_string()) })
            }),
        )
    }
}

#[derive(Debug)]
struct OutputReadTool(Arc<dyn ProcessOutputCache>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    id: String,
    #[serde(default)]
    offset: u64,
    #[serde(default = "default_limit")]
    limit: usize,
}

const fn default_limit() -> usize {
    DEFAULT_OUTPUT_READ_BYTES
}

#[async_trait]
impl ToolExecutor for OutputReadTool {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let arguments = serde_json::from_value::<Arguments>(arguments);
        let result = match arguments {
            Ok(arguments) => tokio::select! {
                biased;
                () = execution.cancellation.cancelled() => return Err(ToolError::Cancelled),
                result = self.0.read(&arguments.id, arguments.offset, arguments.limit) => result,
            },
            Err(error) => Err(rsi_process::ProcessError::InvalidInput(error.to_string())),
        };
        match result {
            Ok(page) => {
                let text = safe_text(&page.bytes);
                let bytes_hex = hex::encode(&page.bytes);
                let mut rendered = format!(
                    "{text}\n[output: {}; bytes: {}..{} of {}]",
                    page.id, page.offset, page.next_offset, page.total_bytes
                );
                if text.as_bytes() != page.bytes {
                    use std::fmt::Write as _;
                    write!(rendered, "\n[raw bytes hex: {bytes_hex}]")
                        .expect("writing a String cannot fail");
                }
                ToolResult::new(
                    json!({"id":page.id,"offset":page.offset,"next_offset":page.next_offset,"total_bytes":page.total_bytes,"text":text,"bytes_hex":bytes_hex}),
                    vec![ToolContent::Text { text: rendered }],
                    false,
                )
            }
            Err(error) => ToolResult::new(
                json!({"code":"output_unavailable","message":error.to_string()}),
                vec![ToolContent::Text {
                    text: error.to_string(),
                }],
                true,
            ),
        }
    }
}

pub(crate) fn safe_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .map(|character| {
            if character.is_control() && !matches!(character, '\n' | '\t')
                || matches!(character, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}
