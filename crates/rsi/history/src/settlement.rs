use rsi_api_protocol::{ApiError, Result};
use rsi_meta::{Deadline, Task};
use tokio_util::sync::CancellationToken;

pub(super) struct PendingReply {
    reply: Option<rsi_history_api::Reply>,
    references: std::sync::Arc<rsi_agent_references::References>,
}
impl PendingReply {
    pub(super) fn new(
        reply: rsi_history_api::Reply,
        references: std::sync::Arc<rsi_agent_references::References>,
    ) -> Self {
        Self {
            reply: Some(reply),
            references,
        }
    }
    pub(super) fn deliver(mut self) -> rsi_history_api::Reply {
        self.reply.take().expect("one reply delivery")
    }
}
impl Drop for PendingReply {
    fn drop(&mut self) {
        if let Some(rsi_history_api::Reply::Frozen { reference }) = &self.reply {
            self.references
                .release_pending(&reference.metadata.target.session_id, &reference.snapshot);
        }
    }
}

pub(super) async fn worker<T>(
    future: impl std::future::Future<Output = Result<T>>,
    mutation: bool,
    stop: &CancellationToken,
) -> Result<T> {
    let result = future.await;
    if !mutation {
        super::check(stop)?;
    }
    result
}

// A deadline is a cancellation request, never evidence that a mutation stopped.
pub(super) async fn mutation<T>(
    mut task: Task<Result<T>>,
    deadline: Deadline,
    stop: CancellationToken,
) -> Result<T> {
    match deadline.timeout(&mut task).await {
        Ok(Ok(Err(ApiError::ShuttingDown)) | Err(_)) => Err(ApiError::OutcomeUnknown),
        Ok(Ok(result)) => result,
        Err(_) => {
            stop.cancel();
            // The handler must retain its registration until actual settlement;
            // transport response deadlines are independent of this owner join.
            match task.await {
                Ok(Err(ApiError::ShuttingDown)) | Err(_) => Err(ApiError::OutcomeUnknown),
                Ok(result) => result,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use rsi_api_protocol::*;
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    #[derive(Debug)]
    struct Worker {
        execution: rsi_meta::Execution,
        entered: Arc<Semaphore>,
        release: Arc<Semaphore>,
        cancelled: CancellationToken,
        refuse: bool,
    }
    #[async_trait]
    impl ApiHandler for Worker {
        async fn invoke(
            &self,
            _: ApiContext,
            _: RetainedBytes,
            output: ApiResponseCapacity,
        ) -> Result<ApiOutput> {
            let entered = self.entered.clone();
            let release = self.release.clone();
            let refuse = self.refuse;
            let stop = self.cancelled.clone();
            let task = self.execution.spawn(async move {
                worker(
                    async move {
                        entered.add_permits(1);
                        release.acquire().await.unwrap().forget();
                        if refuse {
                            return Err(ApiError::Unavailable);
                        }
                        let ApiResponseCapacity::Finite(capacity) = output else {
                            unreachable!()
                        };
                        Ok(ApiOutput::Reply(ApiMessage {
                            json: capacity.encode(&true)?,
                            binary: None,
                        }))
                    },
                    true,
                    &stop,
                )
                .await
            });
            mutation(
                task,
                self.execution
                    .deadline_after(std::time::Duration::from_secs(30)),
                self.cancelled.clone(),
            )
            .await
        }
    }
    #[tokio::test]
    async fn worker_preserves_definitive_mutations_and_fences_cancelled_reads() {
        let stop = CancellationToken::new();
        stop.cancel();
        assert_eq!(worker(async { Ok(42) }, true, &stop).await, Ok(42));
        assert_eq!(
            worker::<()>(async { Err(ApiError::Unavailable) }, true, &stop).await,
            Err(ApiError::Unavailable)
        );
        assert_eq!(
            worker(async { Ok(42) }, false, &stop).await,
            Err(ApiError::ShuttingDown)
        );
    }

    #[tokio::test]
    async fn cancelled_frozen_replies_release_captures_before_admitting_more() {
        use rsi_agent_references::{CaptureContext, ObservedReferenceText, References};
        use rsi_agent_session_protocol::*;
        use sha2::{Digest as _, Sha256};
        let runtime = rsi_meta::Runtime::default();
        let references = Arc::new(References::new(
            Arc::new(rsi_agent_testkit::MemoryStore::default()),
            runtime.execution().clone(),
        ));
        let header = SessionHeader::new_local(
            SessionId::new("target").unwrap(),
            1,
            "/workspace",
            AgentPresetId::new("test").unwrap(),
            FrozenAgentSettings::new(
                "default",
                "",
                rsi_ai_protocol::ModelRef::new("test", "model").unwrap(),
                rsi_sandbox::SandboxMode::ReadOnly,
                false,
            )
            .unwrap(),
        )
        .unwrap();
        let source = ReferenceSource::Observed {
            owner: "acp".into(),
            id: "source".into(),
            epoch: 1,
        };
        for _ in 0..6 {
            let reference = references
                .capture_observed(
                    ObservedReferenceText {
                        source: source.clone(),
                        coordinates: header.coordinates().clone(),
                        text: "text".into(),
                        selection: ReferenceSelection {
                            record: ReferenceRecord {
                                sequence: 1,
                                kind: ReferenceContentKind::Assistant,
                                content_index: 0,
                            },
                            through_seq: 1,
                            start: 0,
                            end: 4,
                            text_sha256: hex::encode(Sha256::digest(b"text")),
                            scanned_bytes: 128,
                        },
                    },
                    CaptureContext::new(
                        source.clone(),
                        header.coordinates().clone(),
                        header.clone(),
                        (),
                        || true,
                    )
                    .unwrap(),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
            let pending = PendingReply::new(
                rsi_history_api::Reply::Frozen { reference },
                references.clone(),
            );
            let stop = CancellationToken::new();
            stop.cancel();
            assert!(matches!(
                worker(async { Ok(pending) }, false, &stop).await,
                Err(ApiError::ShuttingDown)
            ));
        }
        references.close().await;
        assert!(runtime.shutdown().await.is_clean());
    }
    #[tokio::test(start_paused = true)]
    async fn retired_history_route_keeps_its_name_until_mutation_worker_settles() {
        for (deadline, refuse) in [(false, false), (true, false), (true, true)] {
            let execution = rsi_meta::Execution::native(tokio::runtime::Handle::current());
            let registry = Arc::new(rsi_api::ApiRegistry::new(execution.clone()));
            let worker = Arc::new(Worker {
                execution,
                entered: Arc::new(Semaphore::new(0)),
                release: Arc::new(Semaphore::new(0)),
                cancelled: CancellationToken::new(),
                refuse,
            });
            let spec = rsi_history_api::operations()
                .into_iter()
                .find(|spec| spec.id.name() == "reset")
                .unwrap();
            let registration = registry.register(spec.clone(), worker.clone()).unwrap();
            let invocation = registry.admit(&spec.call(), CallOrigin::Local).unwrap();
            let input = invocation.input_budget().copy(b"{}").unwrap();
            let request = tokio::spawn(invocation.invoke(input));
            worker.entered.acquire().await.unwrap().forget();
            let closing = tokio::spawn(async move { registration.close().await });
            tokio::task::yield_now().await;
            if deadline {
                tokio::time::advance(std::time::Duration::from_secs(31)).await;
                worker.cancelled.cancelled().await;
            }
            assert!(!request.is_finished());
            assert!(!closing.is_finished());
            assert!(registry.register(spec.clone(), worker.clone()).is_err());
            worker.release.add_permits(1);
            let result = request.await.unwrap();
            if refuse {
                assert!(
                    matches!(result, Err(ApiError::Unavailable)),
                    "a definitive pre-effect refusal survives the deadline"
                );
            } else {
                assert!(
                    result.is_ok(),
                    "settled worker result survives the response deadline"
                );
            }
            closing.await.unwrap();
            let replacement = registry.register(spec, worker).unwrap();
            replacement.close().await;
            registry.close().await;
        }
    }
}
