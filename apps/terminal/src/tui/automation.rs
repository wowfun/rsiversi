use super::{Action, Client, Menu, Update, error};
use rsi_automation_api::Request;
use serde_json::Value;
pub(super) fn list() -> Request {
    Request::List {
        after: "0".into(),
        watermark: None,
        limit: 50,
    }
}
pub(super) async fn home_read(
    client: &rsi_automation_api::Client,
    request: Request,
) -> rsi_api_protocol::Result<Value> {
    let mutation = is_mutation(&request);
    let mut value = client.call(request).await?;
    if mutation {
        let receipt = value.clone();
        if let Some(id) = value["id"].as_str() {
            match client.call(Request::Get { id: id.into() }).await {
                Ok(attempt) => value = attempt,
                Err(error) => value["read_error"] = Value::String(error.to_string()),
            }
        }
        value["mutation_receipt"] = receipt;
    }
    // Status is presentation data: a failed status read must not hide a control receipt.
    let status = client.call(Request::Status).await.unwrap_or(Value::Null);
    value["browser_readiness"] = status["readiness"].clone();
    Ok(value)
}
pub(super) fn is_mutation(request: &Request) -> bool {
    matches!(
        request,
        Request::SetPolicy { .. } | Request::Cancel { .. } | Request::Resume { .. }
    )
}
pub(super) struct Pending {
    future: futures_util::future::BoxFuture<'static, super::Result<Value>>,
    mutation: bool,
    visible: bool,
}
impl Pending {
    pub(super) fn start(
        slot: &mut Option<Self>,
        client: rsi_automation_api::Client,
        request: Request,
    ) -> bool {
        if slot.is_some() {
            return false;
        }
        let mutation = is_mutation(&request);
        Self::install(
            slot,
            mutation,
            Box::pin(async move { home_read(&client, request).await.map_err(error) }),
        )
    }
    fn install(
        slot: &mut Option<Self>,
        mutation: bool,
        future: futures_util::future::BoxFuture<'static, super::Result<Value>>,
    ) -> bool {
        if slot.is_some() {
            return false;
        }
        *slot = Some(Self {
            future,
            mutation,
            visible: true,
        });
        true
    }
    pub(super) fn retaining(slot: Option<&Self>) -> bool {
        slot.is_some_and(|pending| pending.mutation)
    }
    pub(super) fn dismiss(slot: &mut Option<Self>) {
        if let Some(pending) = slot {
            if pending.mutation {
                pending.visible = false;
            } else {
                *slot = None;
            }
        }
    }
    pub(super) async fn next(slot: &mut Option<Self>) -> (bool, super::Result<Value>) {
        match slot {
            Some(pending) => (pending.visible, pending.future.as_mut().await),
            None => std::future::pending().await,
        }
    }
}
fn request_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS request entropy");
    hex::encode(bytes)
}
impl Client {
    pub(super) fn automation_request(&mut self, request: Request) {
        if self.automation_pending {
            self.state
                .notice("Deployment operation is still awaiting its result");
            return;
        }
        let Some(client) = self.automation.clone() else {
            self.state
                .notice("Automation is unavailable on this connection");
            return;
        };
        if self.pending_requests() >= 10 {
            self.state
                .notice("Client request queue is busy; deployment read was not started");
            return;
        }
        self.state.open_detail("Reading deployment owner…".into());
        let stop = self.state.detail_stop.clone();
        let mutation = is_mutation(&request);
        self.automation_pending = self.spawn_as(super::WorkKind::Automation, async move {
            let result = if mutation { home_read(&client, request).await.map_err(error) }
            else { tokio::select! { biased; () = stop.cancelled() => Ok(Value::Null), value = home_read(&client, request) => value.map_err(error) } };
            Ok(Update::Automation(stop, result))
        });
    }
    pub(super) fn show_automation(&mut self, value: &Value) {
        let items = actions(value);
        self.state.open_detail(format!(
            "Deployment checks · Browser {}\n\n{}",
            value["browser_readiness"].as_str().unwrap_or("unavailable"),
            super::transcript::json_window(value)
        ));
        self.state.detail_actions = Some(Menu {
            title: "Deployment operations".into(),
            items,
            selected: 0,
        });
    }
}

pub(super) fn actions(value: &Value) -> Vec<(String, Action)> {
    let mut items = vec![(
        "Refresh deployment checks".into(),
        Action::Automation(Box::new(list())),
    )];
    if let Some(rows) = value["entries"].as_array() {
        for row in rows {
            if let Some(id) = row["id"].as_str() {
                items.push((
                    format!(
                        "Attempt {id} · {} · {}",
                        row["environment"].as_str().unwrap_or(""),
                        row["state"].as_str().unwrap_or("")
                    ),
                    Action::Automation(Box::new(Request::Get { id: id.into() })),
                ));
            }
        }
        if value["more"] == true
            && let (Some(after), Some(watermark)) =
                (value["after"].as_str(), value["watermark"].as_str())
        {
            items.push((
                "Next page".into(),
                Action::Automation(Box::new(Request::List {
                    after: after.into(),
                    watermark: Some(watermark.into()),
                    limit: 50,
                })),
            ));
        }
    } else if let Some(id) = value["id"].as_str() {
        if value.get("may_cancel").is_none() {
            items.push((
                "Read resulting attempt".into(),
                Action::Automation(Box::new(Request::Get { id: id.into() })),
            ));
        }
        if value["may_cancel"] == true {
            items.push((
                "Cancel attempt".into(),
                Action::Automation(Box::new(Request::Cancel {
                    id: id.into(),
                    request_id: request_id(),
                })),
            ));
        }
        if value["may_resume"] == true
            && let Some(revision) = value["current_rule_revision"].as_str()
        {
            items.push((
                "Create new attempt".into(),
                Action::Automation(Box::new(Request::Resume {
                    id: id.into(),
                    request_id: request_id(),
                    rule_revision: revision.into(),
                })),
            ));
        }
        if let Some(session) = value["session_id"].as_str()
            && let Ok(session) = rsi_agent_session_protocol::SessionId::new(session)
        {
            items.push((
                "Open protected investigation".into(),
                Action::Attach(session),
            ));
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn dismissal_cancels_reads_but_retains_one_mutation_without_reopening() {
        let (release, entered) = tokio::sync::oneshot::channel();
        let mut slot = None;
        assert!(Pending::install(
            &mut slot,
            false,
            Box::pin(async move {
                entered.await.unwrap();
                Ok(Value::Null)
            })
        ));
        Pending::dismiss(&mut slot);
        assert!(slot.is_none());
        assert!(
            release.send(()).is_err(),
            "dismissal retained a read waiter"
        );
        let (release, receipt) = tokio::sync::oneshot::channel();
        assert!(Pending::install(
            &mut slot,
            true,
            Box::pin(async move { Ok(receipt.await.unwrap()) })
        ));
        {
            let mut waiting = Box::pin(Pending::next(&mut slot));
            assert!(futures_util::poll!(&mut waiting).is_pending());
        }
        assert!(!Pending::install(
            &mut slot,
            true,
            Box::pin(async { panic!("replacement polled") })
        ));
        Pending::dismiss(&mut slot);
        assert!(Pending::retaining(slot.as_ref()));
        release
            .send(serde_json::json!({"id":"2","state":"queued"}))
            .unwrap();
        let (visible, result) = Pending::next(&mut slot).await;
        assert!(!visible);
        assert_eq!(result.unwrap()["id"], "2");
        let receipt = serde_json::json!({"id":"2","state":"queued","read_error":"unavailable"});
        assert!(actions(&receipt).iter().any(|(_, action)| matches!(action, Action::Automation(request) if matches!(request.as_ref(), Request::Get { id } if id == "2"))));
    }
    #[test]
    fn unavailable_authority_is_not_offered_and_resume_ids_are_preallocated() {
        let denied = serde_json::json!({"id":"1","may_cancel":false,"may_resume":false});
        assert_eq!(actions(&denied).len(), 1);
        let allowed = serde_json::json!({"id":"1","may_cancel":false,"may_resume":true,"current_rule_revision":"2"});
        let actions = actions(&allowed);
        assert_eq!(actions.len(), 2);
        let Action::Automation(request) = &actions[1].1 else {
            panic!("expected operation")
        };
        request.validate().unwrap();
        let Request::Resume { request_id, .. } = request.as_ref() else {
            panic!("expected resume")
        };
        assert_eq!(request_id.len(), 32);
    }
}
