use super::{AgentAdapter, CancellationToken, Deserialize, Value, json};
use rsi_extension::{Error, Result, body::Body};
use serde::de::DeserializeOwned;
use std::{
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

const CAPACITY_RETRY_DELAY: Duration = Duration::from_millis(10);

async fn retry_capacity<T, F, Fut>(
    available: &AtomicBool,
    cancellation: &CancellationToken,
    mut call: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = std::result::Result<T, rsi_extension::RequestFailure>>,
{
    if cancellation.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let result = call().await;
    if !matches!(
        result,
        Err(rsi_extension::RequestFailure::NotDispatched(
            Error::Capacity
        ))
    ) || !available.swap(false, Ordering::Relaxed)
    {
        return result.map_err(|failure| failure.cause().clone());
    }
    tokio::select! { biased;
        () = cancellation.cancelled() => return Err(Error::Cancelled),
        () = tokio::time::sleep(CAPACITY_RETRY_DELAY) => {}
    }
    if cancellation.is_cancelled() {
        return Err(Error::Cancelled);
    }
    call().await.map_err(|failure| failure.cause().clone())
}

#[derive(Clone, Copy)]
pub(super) enum ReadMethod {
    OperationBody,
    ForkPage,
    StepDecision,
}
impl ReadMethod {
    const fn name(self) -> &'static str {
        match self {
            Self::OperationBody => "agent.operation-body.read",
            Self::ForkPage => "agent.fork.read",
            Self::StepDecision => "agent.step-decision.read",
        }
    }
}

pub(super) struct Reads<'a> {
    adapter: &'a AgentAdapter,
    cancellation: &'a CancellationToken,
    retry: AtomicBool,
}
impl<'a> Reads<'a> {
    pub(super) fn new(adapter: &'a AgentAdapter, cancellation: &'a CancellationToken) -> Self {
        Self {
            adapter,
            cancellation,
            retry: AtomicBool::new(true),
        }
    }
    pub(super) async fn request(&self, method: ReadMethod, payload: Value) -> Result<Value> {
        retry_capacity(&self.retry, self.cancellation, || {
            self.adapter.connection.request(
                method.name(),
                payload.clone(),
                self.cancellation.clone(),
            )
        })
        .await
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub(super) enum Events {
    Inline(crate::events::Events),
    Paged { count: usize },
}
fn event_page_bytes(body: &Body, remaining: usize) -> Result<usize> {
    match body {
        Body::Inline { value } => rsi_api_protocol::measure_json(
            value,
            remaining.min(rsi_agent_session_protocol::MAXIMUM_EXTENSION_JOURNAL_BYTES),
        )
        .map_err(|_| Error::Capacity),
        Body::Paged { bytes, .. } => Ok(*bytes),
    }
}
impl Events {
    pub(super) async fn resolve(
        self,
        reads: &Reads<'_>,
        session: &str,
        operation: &str,
        token: &str,
    ) -> Result<crate::events::Events> {
        let count = match self {
            Self::Inline(mut events) => {
                reads.retain_events(&mut events).await?;
                return Ok(events);
            }
            Self::Paged { count } => count,
        };
        if count == 0 || count > 4096 {
            return Err(Error::Protocol);
        }
        let mut events = crate::events::Events::default();
        // Splitting one array into at most `count` nonempty pages adds at most
        // count-1 framing bytes; canonical journal validation remains separate.
        let mut remaining = rsi_agent_session_protocol::MAXIMUM_EXTENSION_JOURNAL_BYTES + count - 1;
        while events.arrays.len() < count {
            let payload = json!({"session_id":session,"operation_id":operation,"token":token,"part":"events","offset":events.arrays.len()});
            let body: Body = serde_json::from_value(
                reads
                    .request(ReadMethod::OperationBody, payload.clone())
                    .await?,
            )
            .map_err(|_| Error::Protocol)?;
            let bytes = event_page_bytes(&body, remaining)?;
            remaining = remaining.checked_sub(bytes).ok_or(Error::Capacity)?;
            let page = reads
                .events(body, ReadMethod::OperationBody, payload)
                .await?;
            if page.arrays.is_empty() || page.arrays.len() > count - events.arrays.len() {
                return Err(Error::Protocol);
            }
            events.append(page).map_err(|_| Error::Capacity)?;
        }
        Ok(events)
    }
}
impl Reads<'_> {
    pub(super) async fn resolve<T: DeserializeOwned>(
        &self,
        body: Body,
        maximum: usize,
        method: ReadMethod,
        payload: Value,
    ) -> Result<T> {
        let _reservation = self.reserve(&body).await?;
        self.decode(body, maximum, method, payload).await
    }

    async fn reserve(&self, body: &Body) -> Result<Option<rsi_api_protocol::ByteReservation>> {
        Ok(match body {
            Body::Paged { bytes, .. } => {
                Some(
                    retry_capacity(&self.retry, self.cancellation, || {
                        std::future::ready(self.adapter.read_budget.reserve(*bytes).map_err(|_| {
                            rsi_extension::RequestFailure::NotDispatched(Error::Capacity)
                        }))
                    })
                    .await?,
                )
            }
            Body::Inline { .. } => None,
        })
    }

    async fn decode<T: DeserializeOwned>(
        &self,
        body: Body,
        maximum: usize,
        method: ReadMethod,
        payload: Value,
    ) -> Result<T> {
        body.resolve(maximum, |_, offset| {
            let mut request = payload.clone();
            request["body_offset"] = json!(offset);
            async move { self.request(method, request).await }
        })
        .await
    }

    async fn retain_events(&self, events: &mut crate::events::Events) -> Result<()> {
        if events.bytes == 0 {
            return Ok(());
        }
        let credit = retry_capacity(&self.retry, self.cancellation, || {
            std::future::ready(
                self.adapter
                    .event_budget
                    .clone()
                    .try_acquire_many_owned(
                        u32::try_from(events.bytes).expect("bounded event bytes"),
                    )
                    .map_err(|_| rsi_extension::RequestFailure::NotDispatched(Error::Capacity)),
            )
        })
        .await?;
        events.credits.push(credit);
        Ok(())
    }

    pub(super) async fn events(
        &self,
        body: Body,
        method: ReadMethod,
        payload: Value,
    ) -> Result<crate::events::Events> {
        let _reservation = self.reserve(&body).await?;
        let mut events = self
            .decode(
                body,
                rsi_agent_session_protocol::MAXIMUM_EXTENSION_JOURNAL_BYTES,
                method,
                payload,
            )
            .await?;
        self.retain_events(&mut events).await?;
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_extension::RequestFailure;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn event_reply_shape_is_closed_but_an_empty_inline_suffix_is_valid() {
        assert!(serde_json::from_value::<Events>(json!({"count":1,"ignored":true})).is_err());
        assert!(serde_json::from_value::<Events>(json!(vec![0; 4097])).is_err());
        assert!(
            matches!(serde_json::from_value::<Events>(json!([])).unwrap(), Events::Inline(events) if events.arrays.is_empty())
        );
    }

    #[test]
    fn aggregate_framing_allowance_does_not_widen_the_individual_page_counter() {
        let page = Body::Inline {
            value: json!([{"seq":0}]),
        };
        let remaining = rsi_agent_session_protocol::MAXIMUM_EXTENSION_JOURNAL_BYTES + 7;
        assert_eq!(event_page_bytes(&page, remaining), Ok(11));
        assert_eq!(event_page_bytes(&page, 10), Err(Error::Capacity));
    }

    #[tokio::test(start_paused = true)]
    async fn transient_unsent_read_capacity_retries_without_repeating_the_business_operation() {
        let calls = AtomicUsize::new(0);
        let retry = AtomicBool::new(true);
        let result = retry_capacity(&retry, &CancellationToken::new(), || {
            std::future::ready(if calls.fetch_add(1, Ordering::Relaxed) == 0 {
                Err(RequestFailure::NotDispatched(Error::Capacity))
            } else {
                Ok("retained result")
            })
        })
        .await;
        assert_eq!(result, Ok("retained result"));
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_pressure_spends_only_one_retry_across_all_operation_reads() {
        let calls = AtomicUsize::new(0);
        let retry = AtomicBool::new(true);
        let cancelled = CancellationToken::new();
        let mut rejected = || {
            calls.fetch_add(1, Ordering::Relaxed);
            std::future::ready(Err::<(), _>(RequestFailure::NotDispatched(Error::Capacity)))
        };
        assert_eq!(
            retry_capacity(&retry, &cancelled, &mut rejected).await,
            Err(Error::Capacity)
        );
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(
            retry_capacity(&retry, &cancelled, &mut rejected).await,
            Err(Error::Capacity)
        );
        assert_eq!(calls.load(Ordering::Relaxed), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn admitted_or_noncapacity_failures_are_never_repeated() {
        for failure in [
            RequestFailure::MayHaveExecuted(Error::Capacity),
            RequestFailure::MayHaveExecuted(Error::Disconnected),
            RequestFailure::NotDispatched(Error::Disconnected),
            RequestFailure::NotDispatched(Error::Protocol),
            RequestFailure::MayHaveExecuted(Error::Remote("refused".into())),
        ] {
            let calls = AtomicUsize::new(0);
            let retry = AtomicBool::new(true);
            let result = retry_capacity(&retry, &CancellationToken::new(), || {
                calls.fetch_add(1, Ordering::Relaxed);
                std::future::ready(Err::<(), _>(failure.clone()))
            })
            .await;
            assert_eq!(result, Err(failure.cause().clone()));
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            assert!(retry.load(Ordering::Relaxed));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn original_deadline_and_cancellation_stop_the_retry_wait() {
        let calls = AtomicUsize::new(0);
        let stop = CancellationToken::new();
        let retry = AtomicBool::new(true);
        let rejected = || {
            calls.fetch_add(1, Ordering::Relaxed);
            std::future::ready(Err::<(), _>(RequestFailure::NotDispatched(Error::Capacity)))
        };
        assert!(
            tokio::time::timeout(
                Duration::from_millis(5),
                retry_capacity(&retry, &stop, rejected)
            )
            .await
            .is_err()
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        let retry = AtomicBool::new(true);
        let cancel = async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            stop.cancel();
        };
        let (result, ()) = tokio::join!(retry_capacity(&retry, &stop, rejected), cancel);
        assert_eq!(result, Err(Error::Cancelled));
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(
            retry_capacity(&AtomicBool::new(true), &stop, rejected).await,
            Err(Error::Cancelled)
        );
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn read_budget_admission_recovers_without_leaking_credits() {
        let budget = rsi_api_protocol::ByteBudget::new(100).unwrap();
        let held = budget.reserve(100).unwrap();
        let retry = AtomicBool::new(true);
        let stop = CancellationToken::new();
        let admission = retry_capacity(&retry, &stop, || {
            std::future::ready(
                budget
                    .reserve(100)
                    .map_err(|_| RequestFailure::NotDispatched(Error::Capacity)),
            )
        });
        let release = async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            drop(held);
        };
        let (retained, ()) = tokio::join!(admission, release);
        assert!(retained.is_ok());
        assert_eq!(budget.used(), 100);
        drop(retained);
        assert_eq!(budget.used(), 0);
    }
}
