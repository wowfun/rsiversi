use async_trait::async_trait;
use rsi_approval::ApprovalFactory;
use rsi_approval_protocol::{
    ApprovalAnswerer, ApprovalAnswerersContract, ApprovalContract, ApprovalDecision,
    ApprovalOutcome, ApprovalRequest, ApprovalSubject, Result,
};
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Abstain;

#[async_trait]
impl ApprovalAnswerer for Abstain {
    async fn answer(
        &self,
        _request: ApprovalRequest,
        _cancellation: CancellationToken,
    ) -> Result<Option<ApprovalOutcome>> {
        Ok(None)
    }
}

#[derive(Debug)]
struct Allow;

#[async_trait]
impl ApprovalAnswerer for Allow {
    async fn answer(
        &self,
        _request: ApprovalRequest,
        _cancellation: CancellationToken,
    ) -> Result<Option<ApprovalOutcome>> {
        Ok(Some(ApprovalOutcome {
            decision: ApprovalDecision::AllowOnce,
            answerer: "test.allow".into(),
            reason: None,
        }))
    }
}

fn request() -> ApprovalRequest {
    ApprovalRequest {
        subject: ApprovalSubject::new("session-1", "turn-1", "effect-1").unwrap(),
        id: "approval-1".into(),
        action: "write file".into(),
        reason: "tool requested a workspace mutation".into(),
    }
}

#[tokio::test]
async fn waterfall_short_circuits_and_missing_answerer_denies() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.approval",
                "test",
                UpdateMode::Replayable,
                Arc::new(ApprovalFactory),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    let approval = runtime.root().lookup_local::<ApprovalContract>().unwrap();
    let answerers = runtime
        .root()
        .lookup_local::<ApprovalAnswerersContract>()
        .unwrap();
    let first = answerers.register(Arc::new(Abstain)).unwrap();
    let second = answerers.register(Arc::new(Allow)).unwrap();
    let allowed = approval
        .ask(request(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(allowed.decision, ApprovalDecision::AllowOnce);
    drop(second);
    drop(first);
    let denied = approval
        .ask(request(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(denied.decision, ApprovalDecision::Deny);
    assert_eq!(denied.answerer, "rsi.approval.default-deny");

    drop(answerers);
    drop(approval);
    assert!(fiber.dispose().await.is_clean());
}
