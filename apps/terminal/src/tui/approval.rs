use super::{Action, Client, Menu, Update, WorkKind, error};
use rsi_approval_protocol::ApprovalRequest;
use std::fmt::Write as _;

impl Client {
    pub(super) fn inspect_approval(&mut self, request: ApprovalRequest) {
        if self.approval_busy {
            self.state.notice("The approval decision is still pending");
            return;
        }
        self.state.open_detail("Reading current approval…".into());
        let handle = self.handle.clone();
        self.spawn_detail(async move {
            let requests = handle.pending_approvals().await.map_err(error)?;
            Ok(Update::Approval(requests.into_iter().find(|current| {
                current.id == request.id && current.subject == request.subject
            })))
        });
    }
    pub(super) fn show_approval(&mut self, request: Option<ApprovalRequest>) {
        let Some(request) = request else {
            self.state.escape();
            self.state.info("Approval is no longer pending");
            return;
        };
        self.state.open_detail(review(&request));
        self.state.reviewed_approval = Some(request.clone());
        self.state.detail_actions = Some(Menu {
            title: request.action.clone(),
            selected: 0,
            items: vec![
                (
                    "Deny".into(),
                    Action::Decide(
                        request.clone(),
                        rsi_approval_protocol::ApprovalDecision::Deny,
                    ),
                ),
                (
                    "Allow once".into(),
                    Action::Decide(request, rsi_approval_protocol::ApprovalDecision::AllowOnce),
                ),
            ],
        });
        self.state
            .info("Review the prepared request; Enter opens Deny / Allow once");
    }
    pub(super) fn decide_approval(
        &mut self,
        request: ApprovalRequest,
        decision: rsi_approval_protocol::ApprovalDecision,
    ) {
        if self.approval_busy || self.state.reviewed_approval.as_ref() != Some(&request) {
            self.state
                .notice("Read the current pending approval before deciding");
            return;
        }
        let handle = self.handle.clone();
        if self.spawn_as(WorkKind::Read, async move {
            let owner = rsi_agent_session_protocol::SessionId::new(request.subject.session_id())
                .map_err(error)?;
            let result = handle.answer_approval(&owner, &request.id, decision).await;
            Ok(Update::ApprovalDecided(request, result))
        }) {
            self.approval_busy = true;
            self.state.reviewed_approval = None;
            self.state.menu = None;
            self.state.detail_actions = None;
            self.state.info("Sending approval decision…");
        }
    }
    pub(super) fn approval_decided(
        &mut self,
        request: ApprovalRequest,
        result: rsi_session_protocol::Result<bool>,
        current: bool,
    ) {
        self.approval_busy = false;
        match result {
            Ok(accepted) => {
                if current {
                    self.state.escape();
                }
                self.state.info(if accepted {
                    "Approval decision received"
                } else {
                    "Approval is no longer pending"
                });
            }
            Err(problem) => {
                if current {
                    self.state.detail_actions = Some(Menu {
                        title: "Check approval status".into(),
                        selected: 0,
                        items: vec![("Refresh pending request".into(), Action::Approval(request))],
                    });
                }
                self.state.notice(format!(
                    "{problem}. Refresh pending state before deciding again."
                ));
            }
        }
    }
}

pub(super) fn review(request: &ApprovalRequest) -> String {
    let mut text = format!(
        "{}\n\n{}\n",
        request.action,
        friendly_reason(&request.reason)
    );
    if let Some(review) = &request.review {
        let location = review.execution.as_ref().map_or_else(
            || "Not supplied".into(),
            |execution| match execution.binding().location() {
                rsi_agent_session_protocol::ExecutionLocation::Local => "Service machine".into(),
                rsi_agent_session_protocol::ExecutionLocation::Ssh { target } => {
                    format!("SSH target {}", target.as_str())
                }
            },
        );
        let _ = write!(
            text,
            "\nExecution location: {location}\nWorking directory: {}\nRequested sandbox policy: {}\n",
            review.cwd, review.sandbox
        );
        if let Some(execution) = &review.execution {
            let _ = writeln!(text, "Workspace: {}", execution.workspace());
        }
        text.push_str("\nPrepared arguments\n");
        text.push_str(&rsi_terminal_ui::transcript::json_window(&review.arguments));
        text.push('\n');
    } else {
        text.push_str("\nPrepared effect details: Not supplied\n");
    }
    text.push_str("\nRaw request\n");
    text.push_str(&rsi_terminal_ui::transcript::json_window(request));
    text
}

pub(super) fn friendly_reason(reason: &str) -> &str {
    if reason
        .strip_prefix("Agent turn ")
        .and_then(|rest| rest.strip_suffix(" requested this Tool effect"))
        .is_some_and(|turn| !turn.is_empty() && !turn.chars().any(char::is_whitespace))
    {
        "The assistant requested this tool action."
    } else {
        reason
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_reason_is_friendly_without_rewriting_custom_evidence() {
        assert_eq!(
            friendly_reason("Agent turn t-1 requested this Tool effect"),
            "The assistant requested this tool action."
        );
        for reason in [
            "Requested build",
            "Agent turn  requested this Tool effect",
            "Agent turn a b requested this Tool effect",
        ] {
            assert_eq!(friendly_reason(reason), reason);
        }
    }
    #[test]
    fn prepared_arguments_remain_complete_and_policy_is_not_enforcement_evidence() {
        let value = serde_json::json!({"subject":{"session_id":"s","turn_id":"t","effect_id":"e"},"id":"request","action":"Run command","reason":"Requested build","review":{"execution":null,"arguments":{"command":format!("{}TAIL", "中".repeat(30_000))},"cwd":"/workspace","sandbox":"workspace-write","request_sha256":"a".repeat(64)}});
        let request: ApprovalRequest = serde_json::from_value(value).unwrap();
        let text = review(&request);
        assert!(text.contains("Requested sandbox policy: workspace-write"));
        assert!(text.contains("Execution location: Not supplied"));
        assert!(text.split("Raw request").next().unwrap().contains("TAIL"));
    }
    #[test]
    fn oversized_unicode_arguments_leave_the_approval_renderable() {
        let value = serde_json::json!({"subject":{"session_id":"s","turn_id":"t","effect_id":"e"},"id":"request","action":"Write file","reason":"Review this write","review":{"execution":null,"arguments":{"content":"界".repeat(1_000_000)},"cwd":"/workspace","sandbox":"workspace-write","request_sha256":"a".repeat(64)}});
        let request: ApprovalRequest = serde_json::from_value(value).unwrap();
        let text = review(&request);
        assert!(
            text.len() < 1024 * 1024,
            "approval must fit the scene detail budget"
        );
        assert!(text.contains("JSON display window truncated"));
        assert!(text.contains("Raw request"));
    }
}
