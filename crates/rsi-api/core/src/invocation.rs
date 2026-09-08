use crate::registry::CallOwner;
use futures_util::{FutureExt, future::BoxFuture};
use rsi_api_protocol::{
    ApiAdmission, ApiContext, ApiError, ApiInvocation, ApiOutput, ApiResponseCapacity, ByteBudget,
    CallOrigin, OperationClass, OperationEffect, OperationSpec, Result, RetainedBytes,
};
use rsi_meta::Execution;
use std::panic::AssertUnwindSafe;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(crate) struct Invocation {
    pub execution: Execution,
    pub owner: CallOwner,
    pub input: ByteBudget,
    pub output: ApiResponseCapacity,
}

impl ApiInvocation for Invocation {
    fn spec(&self) -> &OperationSpec {
        &self.owner.entry.spec
    }
    fn input_budget(&self) -> ByteBudget {
        self.input.clone()
    }
    fn retiring(&self) -> CancellationToken {
        self.owner.entry.retiring.clone()
    }
    fn retain_admission(&self) -> ApiAdmission {
        ApiAdmission::new(self.owner.quota.clone())
    }
    fn invoke(self: Box<Self>, input: RetainedBytes) -> BoxFuture<'static, Result<ApiOutput>> {
        if let CallOrigin::Device(device) = &self.owner.origin
            && device.revoked.is_cancelled()
        {
            return Box::pin(async { Err(ApiError::Unauthorized) });
        }
        if input.len() > self.spec().maximum_request_bytes {
            return Box::pin(async {
                Err(ApiError::Invalid("request exceeds registered bound".into()))
            });
        }
        if self
            .owner
            .entry
            .retired
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Box::pin(async { Err(ApiError::ShuttingDown) });
        }
        let (sender, receiver) = oneshot::channel();
        let mutation = self.spec().effect == OperationEffect::Mutation;
        let execution = self.execution.clone();
        // Owned before the caller can discard an unpolled result waiter. Read jobs
        // additionally observe receiver closure; mutation jobs intentionally do not.
        drop(execution.spawn(AssertUnwindSafe(self.run(input, sender)).catch_unwind()));
        Box::pin(async move {
            receiver.await.unwrap_or_else(|_| {
                Err(if mutation {
                    ApiError::OutcomeUnknown
                } else {
                    ApiError::Backend("operation task stopped without a result".into())
                })
            })
        })
    }
}

impl Invocation {
    async fn run(self, input: RetainedBytes, mut sender: oneshot::Sender<Result<ApiOutput>>) {
        let Self { owner, output, .. } = self;
        let entry = owner.entry.clone();
        let revoked = match &owner.origin {
            CallOrigin::Local => CancellationToken::new(),
            CallOrigin::Device(device) => device.revoked.clone(),
        };
        let context = ApiContext {
            origin: owner.origin.clone(),
            retiring: entry.retiring.clone(),
        };
        let invoke = entry.handler.invoke(context, input, output);
        let result = if entry.spec.effect == OperationEffect::Mutation {
            invoke.await
        } else {
            tokio::select! {
                biased;
                () = entry.retiring.cancelled() => Err(ApiError::ShuttingDown),
                () = revoked.cancelled() => Err(ApiError::Unauthorized),
                () = sender.closed() => return,
                result = invoke => result,
            }
        };
        match result {
            Ok(ApiOutput::Stream(stream)) if entry.spec.class == OperationClass::Subscription => {
                crate::stream::forward(stream, sender, &entry.spec, &entry.retiring, &revoked)
                    .await;
            }
            Ok(ApiOutput::Stream(_)) => {
                let _ = sender.send(Err(response_failure(
                    entry.spec.effect,
                    "finite operation returned a stream",
                )));
            }
            Ok(ApiOutput::Reply(message)) => {
                let result = if entry.spec.class == OperationClass::Subscription {
                    Err(ApiError::Backend(
                        "subscription returned a finite reply".into(),
                    ))
                } else if message.encoded_len() > entry.spec.maximum_response_bytes {
                    Err(response_failure(
                        entry.spec.effect,
                        "response exceeds registered bound",
                    ))
                } else {
                    Ok(ApiOutput::Reply(message))
                };
                let _ = sender.send(result);
            }
            Err(error) => {
                let error = if entry.spec.effect == OperationEffect::Mutation
                    && matches!(error, ApiError::Backend(_))
                {
                    ApiError::OutcomeUnknown
                } else {
                    error
                };
                let _ = sender.send(Err(error));
            }
        }
        // Retained reply bytes carry their own lifetime after the call slot ends.
        drop(owner);
    }
}

fn response_failure(effect: OperationEffect, message: &str) -> ApiError {
    if effect == OperationEffect::Mutation {
        ApiError::OutcomeUnknown
    } else {
        ApiError::Backend(message.into())
    }
}
