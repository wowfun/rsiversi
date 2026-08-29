use async_trait::async_trait;
use rsi_commands::CommandsFactory;
use rsi_commands_protocol::{
    CommandDefinition, CommandHandler, CommandRequest, CommandResult, CommandRuntimeContract,
    MAXIMUM_COMMAND_MEDIA_REFS, Result,
};
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Echo;

#[async_trait]
impl CommandHandler for Echo {
    async fn execute(
        &self,
        text: String,
        _cancellation: CancellationToken,
    ) -> Result<CommandResult> {
        Ok(CommandResult {
            text,
            media: vec![],
        })
    }
}

#[tokio::test]
async fn commands_require_explicit_dispatch_and_lease_controls_visibility() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.commands",
                "test",
                UpdateMode::Replayable,
                Arc::new(CommandsFactory),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    let commands = runtime
        .root()
        .lookup_local::<CommandRuntimeContract>()
        .unwrap();
    let lease = commands
        .register(CommandDefinition {
            name: "echo".into(),
            description: "echo exact text".into(),
            handler: Arc::new(Echo),
        })
        .unwrap();
    let result = commands
        .execute(
            CommandRequest {
                name: "echo".into(),
                text: "/not-parsed".into(),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.text, "/not-parsed");
    let oversized = serde_json::json!({
        "text": "result",
        "media": (0..=MAXIMUM_COMMAND_MEDIA_REFS).map(|_| serde_json::json!({
            "id": "a".repeat(64),
            "mime": "image/png",
            "bytes": 1,
            "width": 1,
            "height": 1
        })).collect::<Vec<_>>()
    });
    assert!(serde_json::from_value::<CommandResult>(oversized).is_err());
    drop(lease);
    assert!(commands.descriptors().is_empty());
    drop(commands);
    assert!(fiber.dispose().await.is_clean());
}
