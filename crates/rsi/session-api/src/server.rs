use crate::wire::{self, Failure, HandleReply, HandleRequest, Operation, Target, domain};
use rsi_agent_session_protocol::{MessageId, SessionHeader};
use rsi_api_protocol::{ApiHandler, ApiRegistrar, ApiRegistration, OperationClass, json_handler};
use rsi_session_protocol::{
    CreateSession, SessionHandle, SessionIngress, SessionService, SubmitDirectImage, SubmitInput,
};
use serde::{Serialize, de::DeserializeOwned};
use std::{future::Future, sync::Arc};

#[derive(Clone, Debug)]
struct Scratch {
    control: Arc<tokio::sync::Semaphore>,
    data: Arc<tokio::sync::Semaphore>,
}
impl Scratch {
    async fn reserve(
        &self,
        operation: Operation,
    ) -> rsi_api_protocol::Result<tokio::sync::OwnedSemaphorePermit> {
        let spec = operation.spec();
        let budget = if spec.class == OperationClass::Control {
            &self.control
        } else {
            &self.data
        };
        budget
            .clone()
            .acquire_many_owned(
                u32::try_from(spec.maximum_response_bytes).expect("bounded API maximum"),
            )
            .await
            .map_err(|_| rsi_api_protocol::ApiError::ShuttingDown)
    }
}
#[derive(Serialize)]
#[serde(transparent)]
struct Admitted<T> {
    value: T,
    #[serde(skip)]
    _reservation: tokio::sync::OwnedSemaphorePermit,
}
fn admitted<T>(
    value: rsi_session_protocol::Result<T>,
    reservation: tokio::sync::OwnedSemaphorePermit,
) -> rsi_api_protocol::Result<Result<Admitted<T>, Failure>> {
    domain(value).map(|value| {
        value.map(|value| Admitted {
            value,
            _reservation: reservation,
        })
    })
}
pub(super) async fn handle(
    service: &dyn SessionService,
    target: &Target,
) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
    target.validate()?;
    let handle = service.attach(&target.session_id).await?;
    let header = handle.header().await?;
    if header.session_id() != &target.session_id
        || header
            .fingerprint()
            .map_err(|error| rsi_session_protocol::SessionError::Backend(error.to_string()))?
            != target.header_key
    {
        return Err(rsi_session_protocol::SessionError::NotFound(
            "Session Header no longer matches".into(),
        ));
    }
    Ok(handle)
}
fn finite<I, O, F, Fut>(
    service: Arc<dyn SessionService>,
    scratch: Scratch,
    operation: Operation,
    call: F,
) -> Arc<dyn ApiHandler>
where
    I: DeserializeOwned + Send + 'static,
    O: Serialize + Send + 'static,
    F: Fn(Arc<dyn SessionHandle>, I) -> Fut + Send + Sync + Clone + 'static,
    Fut: Future<Output = rsi_session_protocol::Result<O>> + Send + 'static,
{
    json_handler(move |_, request: HandleRequest<I>| {
        let service = service.clone();
        let scratch = scratch.clone();
        let call = call.clone();
        async move {
            let reservation = scratch.reserve(operation).await?;
            let value = async {
                let owner = handle(service.as_ref(), &request.target).await?;
                let body = call(owner, request.input).await?;
                Ok(HandleReply {
                    target: request.target,
                    body,
                })
            }
            .await;
            admitted(value, reservation)
        }
    })
}
/// Owns Session endpoint registrations without owning domain state.
#[derive(Debug)]
pub struct SessionApi {
    registrations: Vec<ApiRegistration>,
}
impl SessionApi {
    /// Registers operations against the same service and trusted draft ingress generation.
    pub fn register(
        registrar: &dyn ApiRegistrar,
        service: Arc<dyn SessionService>,
        ingress: Arc<dyn SessionIngress>,
    ) -> rsi_api_protocol::Result<Self> {
        let scratch = Scratch {
            control: Arc::new(tokio::sync::Semaphore::new(2 * 1024 * 1024)),
            data: Arc::new(tokio::sync::Semaphore::new(
                rsi_api_protocol::MAXIMUM_API_BYTES,
            )),
        };
        let mut registrations =
            root_operations(registrar, service.clone(), ingress, scratch.clone())?;
        for (operation, handler) in handle_operations(&service, &scratch) {
            registrations.push(registrar.register(operation.spec(), handler)?);
        }
        for (operation, service) in [Operation::Observe, Operation::Interactions]
            .into_iter()
            .zip([service.clone(), service])
        {
            registrations.push(registrar.register(
                operation.spec(),
                Arc::new(crate::server_stream::Handler { service, operation }),
            )?);
        }
        Ok(Self { registrations })
    }
    /// Retires admission and drains server-owned mutations before dependencies retire.
    pub async fn close(self) {
        futures_util::future::join_all(self.registrations.into_iter().map(ApiRegistration::close))
            .await;
    }
}
fn root_operations(
    registrar: &dyn ApiRegistrar,
    service: Arc<dyn SessionService>,
    ingress: Arc<dyn SessionIngress>,
    scratch: Scratch,
) -> rsi_api_protocol::Result<Vec<ApiRegistration>> {
    let create_scratch = scratch.clone();
    let create = registrar.register(
        Operation::Create.spec(),
        json_handler(move |context, request: CreateSession| {
            let ingress = ingress.clone();
            let scratch = create_scratch.clone();
            async move {
                let reservation = scratch.reserve(Operation::Create).await?;
                let result = async {
                    ingress
                        .create_from(request, context.origin)
                        .await?
                        .header()
                        .await
                }
                .await;
                admitted(result, reservation)
            }
        }),
    )?;
    let attach_scratch = scratch.clone();
    let attach_service = service.clone();
    let attach = registrar.register(
        Operation::Attach.spec(),
        json_handler(move |_, request: wire::Attach| {
            let service = attach_service.clone();
            let scratch = attach_scratch.clone();
            async move {
                let reservation = scratch.reserve(Operation::Attach).await?;
                let result: rsi_session_protocol::Result<SessionHeader> =
                    async { service.attach(&request.session_id).await?.header().await }.await;
                admitted(result, reservation)
            }
        }),
    )?;
    let recent = registrar.register(
        Operation::Recent.spec(),
        json_handler(move |_, request: wire::Recent| {
            let service = service.clone();
            let scratch = scratch.clone();
            async move {
                let reservation = scratch.reserve(Operation::Recent).await?;
                if rsi_agent_store_protocol::validate_session_read_limit(request.limit).is_err() {
                    return Ok(Err(Failure::Invalid {
                        message: "recent Session limit must be within 1..=256".into(),
                    }));
                }
                admitted(
                    service
                        .list_recent(
                            request.after.as_ref(),
                            request.limit.min(wire::RECENT_READ_LIMIT),
                        )
                        .await,
                    reservation,
                )
            }
        }),
    )?;
    Ok(vec![create, attach, recent])
}
fn handle_operations(
    service: &Arc<dyn SessionService>,
    scratch: &Scratch,
) -> Vec<(Operation, Arc<dyn ApiHandler>)> {
    macro_rules! add {
        ($operation:ident, $call:expr) => {
            (
                Operation::$operation,
                finite(
                    service.clone(),
                    scratch.clone(),
                    Operation::$operation,
                    $call,
                ),
            )
        };
    }
    vec![
        add!(Submit, |owner, request: SubmitInput| async move {
            rsi_session_protocol::validate_session_input(&request.content)?;
            owner.submit(request).await
        }),
        add!(Image, |owner, request: SubmitDirectImage| async move {
            owner.generate_image(request).await
        }),
        add!(MessageStatus, |owner, message: MessageId| async move {
            owner.message_status(&message).await
        }),
        add!(
            ReadMessage,
            |owner, request: wire::MessageRead| async move {
                let message = owner
                    .read_message(&request.message_id, request.accepted_control_seq)
                    .await?;
                Ok(wire::MessageReadReply {
                    accepted_control_seq: request.accepted_control_seq,
                    message,
                })
            }
        ),
        add!(Cancel, |owner, request: wire::Cancel| async move {
            owner.cancel(request.target, request.reason).await
        }),
        add!(History, |owner, request: wire::History| async move {
            Ok(bound_history(
                owner.history_before(request.before, request.limit).await?,
            ))
        }),
        add!(
            Inspect,
            |owner, (): ()| async move { owner.inspect().await }
        ),
        add!(Questions, |owner, (): ()| async move {
            owner.pending_questions().await
        }),
        add!(
            AnswerQuestion,
            |owner, request: wire::QuestionAnswer| async move {
                owner.answer_question(&request.id, request.answer).await
            }
        ),
        add!(Approvals, |owner, (): ()| async move {
            owner.pending_approvals().await
        }),
        add!(
            AnswerApproval,
            |owner, request: wire::ApprovalAnswer| async move {
                owner
                    .answer_approval(&request.owner, &request.id, request.decision)
                    .await
            }
        ),
    ]
}

fn bound_history(
    mut page: rsi_session_protocol::SessionHistoryPage,
) -> rsi_session_protocol::SessionHistoryPage {
    let mut bytes = page
        .facts
        .iter()
        .map(rsi_agent_session_protocol::SessionFact::encoded_len)
        .sum::<usize>();
    let mut remove = 0;
    while bytes > wire::LARGE_REPLY - 64 * 1024 {
        bytes -= page.facts[remove].encoded_len();
        remove += 1;
    }
    if remove > 0 {
        page.facts.drain(..remove);
        page.has_more = true;
    }
    page
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{EffectId, SessionFact, SessionFactBody, SessionId, TurnId};

    fn sized_fact(sequence: u64, bytes: usize) -> SessionFact {
        let make = |text| {
            SessionFact::new(
                sequence,
                1,
                SessionFactBody::ModelEvent {
                    turn_id: TurnId::new("turn").unwrap(),
                    effect_id: EffectId::new("effect").unwrap(),
                    event: rsi_ai_protocol::LanguageEvent::ContentDelta {
                        index: 0,
                        delta: rsi_ai_protocol::ContentDelta::Text(text),
                    },
                },
            )
            .unwrap()
        };
        let payload = bytes - (make("x".into()).encoded_len() - 1);
        let fact = make("\0".repeat(payload / 6) + &"x".repeat(payload % 6));
        assert_eq!(fact.encoded_len(), bytes);
        fact
    }

    #[test]
    fn full_native_history_page_keeps_the_maximum_fact_and_a_valid_backward_cursor() {
        let page = bound_history(rsi_session_protocol::SessionHistoryPage {
            before_seq: 3,
            durable_seq: 2,
            has_more: false,
            facts: vec![
                sized_fact(1, 28 * 1024 * 1024),
                sized_fact(2, 36 * 1024 * 1024),
            ],
        });
        assert_eq!(page.facts.len(), 1);
        assert_eq!(page.facts[0].seq(), 2);
        assert_eq!(
            page.facts[0].encoded_len(),
            rsi_agent_session_protocol::MAXIMUM_SESSION_FACT_BYTES
        );
        assert!(page.has_more);
        let checked = rsi_agent_store_protocol::StoreBackwardFactPage {
            before_seq: page.before_seq,
            durable_seq: page.durable_seq,
            has_more: page.has_more,
            facts: page.facts,
        };
        checked.validate().unwrap();
        let budget = rsi_api_protocol::ByteBudget::default();
        let encoded = budget
            .encode(
                &HandleReply {
                    target: Target {
                        session_id: SessionId::new("session").unwrap(),
                        header_key: "a".repeat(64),
                    },
                    body: rsi_session_protocol::SessionHistoryPage {
                        before_seq: checked.before_seq,
                        durable_seq: checked.durable_seq,
                        has_more: checked.has_more,
                        facts: checked.facts,
                    },
                },
                wire::LARGE_REPLY,
            )
            .unwrap();
        assert!(encoded.len() > rsi_agent_session_protocol::MAXIMUM_SESSION_FACT_BYTES);
        drop(encoded);
        assert_eq!(budget.used(), 0);
    }
    #[tokio::test]
    async fn scratch_waiters_are_bounded_by_admitted_calls_and_release_on_drop() {
        use futures_util::FutureExt;
        let scratch = Scratch {
            control: Arc::new(tokio::sync::Semaphore::new(2 * 1024 * 1024)),
            data: Arc::new(tokio::sync::Semaphore::new(
                rsi_api_protocol::MAXIMUM_API_BYTES,
            )),
        };
        let small = scratch.reserve(Operation::Submit).await.unwrap();
        let mut read = Box::pin(scratch.reserve(Operation::Recent));
        assert!(read.as_mut().now_or_never().is_none());
        drop(small);
        let large = read.await.unwrap();
        let mut other = Box::pin(scratch.reserve(Operation::History));
        assert!(other.as_mut().now_or_never().is_none());
        drop(other);
        drop(large);
        let next = scratch.reserve(Operation::History).await.unwrap();
        drop(next);
        assert_eq!(
            scratch.data.available_permits(),
            rsi_api_protocol::MAXIMUM_API_BYTES
        );
    }
}
