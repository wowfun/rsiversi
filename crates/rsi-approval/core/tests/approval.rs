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
        review: None,
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
    let (_owner, caller) = owner(&runtime.root()).await;
    let credential = caller.registration_context().unwrap();
    let first = answerers.register(&credential, Arc::new(Abstain)).unwrap();
    let second = answerers.register(&credential, Arc::new(Allow)).unwrap();
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

#[derive(Debug)]
struct Capture(Arc<std::sync::Mutex<Option<rsi_meta::Context>>>);
#[async_trait]
impl rsi_meta::PluginFactory for Capture {
    fn prepare(&self, desired: &Value) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        Ok(rsi_meta::PreparedActivation::new(desired.clone()))
    }
    async fn activate(&self, plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        *self.0.lock().unwrap() = Some(plan.context().clone());
        Ok(())
    }
}
async fn owner(parent: &rsi_meta::Context) -> (rsi_meta::FiberHandle, rsi_meta::Context) {
    let captured = Arc::new(std::sync::Mutex::new(None));
    let fiber = parent
        .apply(
            ResolvedFactory::linked(
                "fixture.owner",
                "1",
                UpdateMode::Replayable,
                Arc::new(Capture(captured.clone())),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    let context = captured.lock().unwrap().take().unwrap();
    (fiber, context)
}
#[derive(Debug)]
struct Answer(&'static str);
#[async_trait]
impl ApprovalAnswerer for Answer {
    async fn answer(
        &self,
        _: ApprovalRequest,
        _: CancellationToken,
    ) -> Result<Option<ApprovalOutcome>> {
        Ok(Some(ApprovalOutcome {
            decision: ApprovalDecision::AllowOnce,
            answerer: self.0.into(),
            reason: None,
        }))
    }
}
async fn winner(approval: &dyn rsi_approval_protocol::Approval) -> String {
    approval
        .ask(request(), CancellationToken::new())
        .await
        .unwrap()
        .answerer
}

#[tokio::test]
async fn approval_precedence_follows_declaration_reorder_and_selective_owner_rebuild() {
    let runtime = Runtime::default();
    let root = runtime.root();
    root.apply(
        ResolvedFactory::linked(
            "rsi.approval",
            "1",
            UpdateMode::Replayable,
            Arc::new(ApprovalFactory),
        ),
        Value::Null,
    )
    .await
    .unwrap();
    let a = root.child_position().unwrap();
    let b = root.child_position().unwrap();
    let (first, ac) = owner(&root.with_child_position(&a).unwrap()).await;
    let (second, bc) = owner(&root.with_child_position(&b).unwrap()).await;
    let answerers = root.lookup_local::<ApprovalAnswerersContract>().unwrap();
    let approval = root.lookup_local::<ApprovalContract>().unwrap();
    let _b = answerers
        .register(&bc.registration_context().unwrap(), Arc::new(Answer("b")))
        .unwrap();
    let _a = answerers
        .register(&ac.registration_context().unwrap(), Arc::new(Answer("a")))
        .unwrap();
    assert_eq!(winner(approval.as_ref()).await, "a");
    let original = second.snapshot().generation;
    root.reorder_children(&[b.clone(), a.clone()]).unwrap();
    assert_eq!(winner(approval.as_ref()).await, "b");
    root.reorder_children(&[a.clone(), b]).unwrap();
    assert!(first.dispose().await.is_clean());
    let (_, replacement) = owner(&root.with_child_position(&a).unwrap()).await;
    let _replacement = answerers
        .register(
            &replacement.registration_context().unwrap(),
            Arc::new(Answer("a2")),
        )
        .unwrap();
    assert_eq!(winner(approval.as_ref()).await, "a2");
    assert_eq!(second.snapshot().generation, original);
    assert!(ac.registration_context().is_err());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn approval_rejects_foreign_runtime_and_capacity_before_publication() {
    let runtime = Runtime::default();
    let other = Runtime::default();
    let root = runtime.root();
    root.apply(
        ResolvedFactory::linked(
            "rsi.approval",
            "1",
            UpdateMode::Replayable,
            Arc::new(ApprovalFactory),
        ),
        Value::Null,
    )
    .await
    .unwrap();
    let (_, context) = owner(&root).await;
    let (_, foreign) = owner(&other.root()).await;
    let answerers = root.lookup_local::<ApprovalAnswerersContract>().unwrap();
    assert!(
        answerers
            .register(&foreign.registration_context().unwrap(), Arc::new(Allow))
            .is_err()
    );
    let credential = context.registration_context().unwrap();
    let mut leases = Vec::new();
    for _ in 0..rsi_approval_protocol::MAXIMUM_APPROVAL_ANSWERERS {
        leases.push(answerers.register(&credential, Arc::new(Abstain)).unwrap());
    }
    assert!(answerers.register(&credential, Arc::new(Allow)).is_err());
    drop(leases.pop());
    let _replacement = answerers.register(&credential, Arc::new(Allow)).unwrap();
    assert!(runtime.shutdown().await.is_clean());
    assert!(other.shutdown().await.is_clean());
}
