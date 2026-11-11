//! A live operation lends its captured, read-only history to the process.
use async_trait::async_trait;
use rsi_agent_composition_protocol::ContributionFactReader;
use rsi_extension::{Error, Handler, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

const FRAGMENT_BYTES: usize = 128 * 1024;
mod facts;
pub use facts::project_history_page;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    session_id: String,
    operation_id: String,
    after_seq: u64,
    through_seq: Option<u64>,
    offset: usize,
}
#[derive(Debug)]
struct Page {
    after: u64,
    requested_end: u64,
    through: u64,
    offset: usize,
    json: String,
    _permit: OwnedSemaphorePermit,
}
#[derive(Debug)]
struct Authority {
    reader: Arc<dyn ContributionFactReader>,
    horizon: u64,
    cancellation: CancellationToken,
    page: tokio::sync::Mutex<Option<Page>>,
    previous: Option<String>,
}
#[derive(Debug)]
pub(crate) struct HistoryRouter {
    active: Mutex<BTreeMap<(String, String), Arc<Authority>>>,
    pages: Arc<Semaphore>,
}
impl Default for HistoryRouter {
    fn default() -> Self {
        Self {
            active: Mutex::default(),
            pages: Arc::new(Semaphore::new(2)),
        }
    }
}
#[derive(Debug)]
pub(crate) struct HistoryScope {
    router: Arc<HistoryRouter>,
    key: (String, String),
    cancellation: CancellationToken,
}
impl Drop for HistoryScope {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.router
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.key);
    }
}
impl HistoryRouter {
    pub(crate) fn enter_reader(
        self: &Arc<Self>,
        session: &str,
        operation: &str,
        reader: Arc<dyn ContributionFactReader>,
        horizon: u64,
        cancellation: &CancellationToken,
    ) -> Result<HistoryScope> {
        self.enter_step_reader(session, operation, reader, horizon, cancellation, None)
    }

    pub(crate) fn enter_step_reader(
        self: &Arc<Self>,
        session: &str,
        operation: &str,
        reader: Arc<dyn ContributionFactReader>,
        horizon: u64,
        cancellation: &CancellationToken,
        previous: Option<String>,
    ) -> Result<HistoryScope> {
        let key = (session.to_owned(), operation.to_owned());
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if active.len() >= 64 || active.contains_key(&key) {
            return Err(Error::Capacity);
        }
        let cancellation = cancellation.child_token();
        active.insert(
            key.clone(),
            Arc::new(Authority {
                reader,
                previous,
                horizon,
                cancellation: cancellation.clone(),
                page: tokio::sync::Mutex::new(None),
            }),
        );
        Ok(HistoryScope {
            router: self.clone(),
            key,
            cancellation,
        })
    }
    async fn read(&self, authority: &Authority, request: &Request) -> Result<Value> {
        let end = request
            .through_seq
            .unwrap_or_else(|| request.after_seq.saturating_add(1).min(authority.horizon));
        if end < request.after_seq || end > authority.horizon {
            return Err(Error::Protocol);
        }
        let mut slot = authority.page.lock().await;
        if slot.is_none() {
            if request.offset != 0 {
                return Err(Error::Protocol);
            }
            let permit = self
                .pages
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| Error::Disconnected)?;
            let limit = usize::try_from(end - request.after_seq)
                .unwrap_or(usize::MAX)
                .clamp(1, rsi_agent_session_protocol::MAXIMUM_FACTS_PER_READ);
            let page = authority
                .reader
                .read(request.after_seq, limit)
                .await
                .map_err(|error| Error::Remote(error.to_string()))?;
            if page.through_seq > authority.horizon
                || page.through_seq < request.after_seq
                || (page.through_seq == request.after_seq && request.after_seq != authority.horizon)
                || page.facts.len() > limit
            {
                return Err(Error::Protocol);
            }
            let (through, json) = project_history_page(
                page.facts.iter().map(Arc::as_ref),
                page.through_seq.min(end),
                rsi_agent_session_protocol::MAXIMUM_SESSION_FACT_BYTES + 2,
            )?;
            *slot = Some(Page {
                after: request.after_seq,
                requested_end: end,
                through,
                offset: 0,
                json,
                _permit: permit,
            });
        }
        let page = slot.as_mut().ok_or(Error::Protocol)?;
        if request.after_seq != page.after
            || end != page.requested_end
            || request.offset != page.offset
        {
            return Err(Error::Protocol);
        }
        let mut end = (page.offset + FRAGMENT_BYTES).min(page.json.len());
        while !page.json.is_char_boundary(end) {
            end -= 1;
        }
        let response = json!({ "through_seq": page.through, "total_bytes": page.json.len(),
            "offset": page.offset, "next_offset": end, "json": &page.json[page.offset..end] });
        page.offset = end;
        if end == page.json.len() {
            *slot = None;
        }
        Ok(response)
    }

    fn authority(&self, key: &(String, String)) -> Result<Arc<Authority>> {
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .cloned()
            .ok_or_else(|| Error::Remote("history has no active operation authority".into()))
    }
}
#[async_trait]
impl Handler for HistoryRouter {
    async fn call(
        &self,
        method: &str,
        value: Value,
        cancellation: CancellationToken,
    ) -> Result<Value> {
        if method == "agent.step-decision.read" {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Fragment {
                session_id: String,
                operation_id: String,
                offset: usize,
            }
            let part: Fragment = serde_json::from_value(value).map_err(|_| Error::Protocol)?;
            let authority = self.authority(&(part.session_id, part.operation_id))?;
            if authority.cancellation.is_cancelled() || cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let previous = authority.previous.as_deref().ok_or(Error::Protocol)?;
            let remaining = previous
                .get(part.offset..)
                .filter(|s| !s.is_empty())
                .ok_or(Error::Protocol)?;
            return Ok(Value::String(
                remaining[..remaining.floor_char_boundary(FRAGMENT_BYTES.min(remaining.len()))]
                    .to_owned(),
            ));
        }
        if method != "agent.history.read" {
            return Err(Error::Protocol);
        }
        let request: Request = serde_json::from_value(value).map_err(|_| Error::Protocol)?;
        let authority =
            self.authority(&(request.session_id.clone(), request.operation_id.clone()))?;
        tokio::select! {
            biased;
            () = authority.cancellation.cancelled() => Err(Error::Cancelled),
            () = cancellation.cancelled() => Err(Error::Cancelled),
            result = self.read(&authority, &request) => result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_composition_protocol::{ContributionFactPage, ContributionResult};
    use rsi_agent_session_protocol::{
        AgentMessageContent, ContributionId, InputMessageSource, SessionFact, SessionFactBody,
        StepId, TurnId,
    };

    #[derive(Debug)]
    struct Reader(Vec<Arc<SessionFact>>);
    #[async_trait]
    impl ContributionFactReader for Reader {
        async fn read(&self, after: u64, limit: usize) -> ContributionResult<ContributionFactPage> {
            assert_eq!(after, 0);
            assert!(matches!(limit, 1 | 3));
            Ok(ContributionFactPage {
                facts: self.0.iter().take(limit).cloned().collect(),
                through_seq: 3,
            })
        }
    }
    #[tokio::test]
    async fn history_page_normalizes_attempts_and_preserves_timing_unicode_and_scope() {
        let events = vec![
            (
                0,
                json!({"type":"content_started","index":0,"content":{"type":"text"}}),
            ),
            (
                7,
                json!({"type":"content_delta","index":0,"delta":{"type":"text","value":"中文".repeat(35_000)}}),
            ),
            (11, json!({"type":"content_finished","index":0})),
            (13, json!({"type":"finished","reason":"stop","replay":null})),
        ];
        let attempt = rsi_agent_session_protocol::ModelAttemptLog::from_events(
            events
                .iter()
                .map(|(time, event)| (*time, serde_json::from_value(event.clone()).unwrap())),
            true,
        )
        .unwrap();
        let fact = Arc::new(
            SessionFact::new(
                1,
                10,
                SessionFactBody::ModelAttemptSettled {
                    turn_id: TurnId::new("turn").unwrap(),
                    effect_id: rsi_agent_session_protocol::EffectId::new("model").unwrap(),
                    attempt,
                    purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                },
            )
            .unwrap(),
        );
        let router = Arc::new(HistoryRouter::default());
        let cancel = CancellationToken::new();
        let scope = router
            .enter_reader(
                "session",
                "operation",
                Arc::new(Reader(vec![fact])),
                3,
                &cancel,
            )
            .unwrap();
        let mut request =
            json!({"session_id":"session","operation_id":"operation","after_seq":0,"offset":0});
        let mut encoded = String::new();
        loop {
            let page = router
                .call(
                    "agent.history.read",
                    request.clone(),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
            assert_eq!(page["through_seq"], 1);
            encoded.push_str(page["json"].as_str().unwrap());
            request["offset"] = page["next_offset"].clone();
            if page["next_offset"] == page["total_bytes"] {
                break;
            }
            assert!(
                router
                    .call(
                        "agent.history.read",
                        {
                            let mut changed = request.clone();
                            changed["through_seq"] = json!(2);
                            changed
                        },
                        CancellationToken::new()
                    )
                    .await
                    .is_err()
            );
        }
        let projection: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(
            projection,
            json!([{ "seq":1,"timestamp_ms":10,"type":"model_attempt_settled","turn_id":"turn","effect_id":"model","purpose":"conversation","attempt":{"retain_context":true,"events":events} }])
        );
        assert_eq!(router.pages.available_permits(), 2);
        drop(scope);
        request["offset"] = json!(0);
        assert!(
            router
                .call("agent.history.read", request, CancellationToken::new())
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn preceding_decision_is_fragmented_on_char_boundaries_and_revoked_with_its_step() {
        let previous = "界🚀".repeat(110_000);
        let router = Arc::new(HistoryRouter::default());
        let cancel = CancellationToken::new();
        let scope = router
            .enter_step_reader(
                "session",
                "step",
                Arc::new(Reader(vec![])),
                0,
                &cancel,
                Some(previous.clone()),
            )
            .unwrap();
        let read = |offset| {
            router.call(
                "agent.step-decision.read",
                json!({"session_id":"session","operation_id":"step","offset":offset}),
                CancellationToken::new(),
            )
        };
        let mut encoded = String::new();
        while encoded.len() < previous.len() {
            let value = read(encoded.len()).await.unwrap();
            let part = value.as_str().unwrap();
            assert!(!part.is_empty() && part.len() <= FRAGMENT_BYTES);
            encoded.push_str(part);
        }
        assert_eq!(encoded, previous);
        assert!(read(1).await.is_err());
        drop(scope);
        assert!(read(0).await.is_err());
    }

    #[tokio::test]
    async fn history_fragments_preserve_unicode_cursor_and_withdraw_authority() {
        let fact = Arc::new(
            SessionFact::new(
                2,
                1,
                SessionFactBody::InputMessageClaimed {
                    turn_id: TurnId::new("turn").unwrap(),
                    step_id: StepId::new("step").unwrap(),
                    source: InputMessageSource::PluginContext {
                        tool: None,
                        contribution_id: ContributionId::new("fixture").unwrap(),
                    },
                    content: vec![AgentMessageContent::Text {
                        text: "中文".repeat(60_000),
                    }],
                },
            )
            .unwrap(),
        );
        let router = Arc::new(HistoryRouter::default());
        let cancellation = CancellationToken::new();
        let key = ("session".into(), "operation".into());
        let start = Arc::new(
            SessionFact::new(
                1,
                1,
                SessionFactBody::StepStarted {
                    turn_id: TurnId::new("turn").unwrap(),
                    step_id: StepId::new("step").unwrap(),
                },
            )
            .unwrap(),
        );
        let authority = Arc::new(Authority {
            reader: Arc::new(Reader(vec![start.clone(), fact.clone()])),
            previous: None,
            horizon: 3,
            cancellation: cancellation.clone(),
            page: tokio::sync::Mutex::new(None),
        });
        router
            .active
            .lock()
            .unwrap()
            .insert(key.clone(), authority.clone());
        let scope = HistoryScope {
            router: router.clone(),
            key,
            cancellation,
        };
        let mut request = json!({"session_id":"foreign","operation_id":"operation","after_seq":0,"through_seq":3,"offset":0});
        assert!(
            router
                .call(
                    "agent.history.read",
                    request.clone(),
                    CancellationToken::new()
                )
                .await
                .is_err()
        );
        request["session_id"] = json!("session");
        let mut json = String::new();
        loop {
            let page = router
                .call(
                    "agent.history.read",
                    request.clone(),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
            assert_eq!(page["through_seq"], 3); // A claim-invisible Fact advanced the scan cursor.
            json.push_str(page["json"].as_str().unwrap());
            request["offset"] = page["next_offset"].clone();
            if page["next_offset"] == page["total_bytes"] {
                break;
            }
            assert_eq!(router.pages.available_permits(), 1);
        }
        let restored: Vec<SessionFact> = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored,
            vec![start.as_ref().clone(), fact.as_ref().clone()]
        );
        assert_eq!(router.pages.available_permits(), 2);
        drop(scope);
        assert!(authority.cancellation.is_cancelled());
        assert!(
            router
                .call("agent.history.read", request, CancellationToken::new())
                .await
                .is_err()
        );
    }
}
