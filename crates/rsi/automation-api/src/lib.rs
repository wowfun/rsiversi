//! Finite Automation wire operations without native execution authority.
#![forbid(unsafe_code)]
use rsi_api_protocol::{
    ApiClient, ApiError, OperationAccess, OperationClass, OperationEffect, OperationId,
    OperationSpec, RequestEncoding, Result, call_json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Status,
    Diagnostics,
    Policy,
    SetPolicy {
        expected_revision: String,
        policy: Value,
    },
    List {
        after: String,
        watermark: Option<String>,
        limit: u8,
    },
    Get {
        id: String,
    },
    Artifact {
        id: String,
        ordinal: u8,
    },
    Cancel {
        id: String,
        request_id: String,
    },
    Resume {
        id: String,
        request_id: String,
        rule_revision: String,
    },
}
impl Request {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Diagnostics => "diagnostics",
            Self::Policy => "policy",
            Self::SetPolicy { .. } => "set_policy",
            Self::List { .. } => "list",
            Self::Get { .. } => "get",
            Self::Artifact { .. } => "artifact",
            Self::Cancel { .. } => "cancel",
            Self::Resume { .. } => "resume",
        }
    }
    pub fn spec(&self) -> OperationSpec {
        spec(self.name())
    }
    /// # Errors
    /// Rejects malformed values or values exceeding the owning protocol bounds.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::SetPolicy {
                expected_revision,
                policy,
            } => {
                decimal(expected_revision)?;
                if !policy.is_object() {
                    return Err(ApiError::Invalid("policy object required".into()));
                }
            }
            Self::List {
                after,
                watermark,
                limit,
            } => {
                decimal(after)?;
                if let Some(value) = watermark {
                    decimal(value)?;
                }
                if !(1..=50).contains(limit) {
                    return Err(ApiError::Invalid("page bound is 1..50".into()));
                }
            }
            Self::Get { id } | Self::Artifact { id, .. } => {
                if decimal(id)? == 0 {
                    return Err(ApiError::Invalid("attempt required".into()));
                }
            }
            Self::Cancel { id, request_id } | Self::Resume { id, request_id, .. } => {
                if decimal(id)? == 0
                    || request_id.is_empty()
                    || request_id.len() > 128
                    || !request_id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                {
                    return Err(ApiError::Invalid("invalid attempt mutation".into()));
                }
                if let Self::Resume { rule_revision, .. } = self {
                    decimal(rule_revision)?;
                }
            }
            _ => {}
        }
        if matches!(self,Self::Artifact{ordinal,..}if *ordinal>3) {
            return Err(ApiError::Invalid("artifact bound is four".into()));
        }
        Ok(())
    }
}
/// # Errors
/// Rejects noncanonical decimal text or unsigned-integer overflow.
pub fn decimal(value: &str) -> Result<u64> {
    let n = value
        .parse::<u64>()
        .map_err(|_| ApiError::Invalid("invalid automation coordinate".into()))?;
    if n.to_string() != value || n > i64::MAX as u64 {
        return Err(ApiError::Invalid(
            "noncanonical automation coordinate".into(),
        ));
    }
    Ok(n)
}
pub fn operations() -> Vec<OperationSpec> {
    [
        "status",
        "diagnostics",
        "policy",
        "set_policy",
        "list",
        "get",
        "artifact",
        "cancel",
        "resume",
    ]
    .map(spec)
    .into()
}
fn spec(name: &str) -> OperationSpec {
    OperationSpec {
        id: OperationId::new("automation", name, 1).expect("static automation operation"),
        access: OperationAccess::Authenticated,
        class: if matches!(name, "cancel" | "resume" | "set_policy") {
            OperationClass::Control
        } else {
            OperationClass::Data
        },
        effect: if matches!(name, "cancel" | "resume" | "set_policy") {
            OperationEffect::Mutation
        } else {
            OperationEffect::Read
        },
        encoding: RequestEncoding::Json,
        maximum_request_bytes: if name == "set_policy" {
            128 * 1024
        } else {
            16384
        },
        maximum_response_bytes: if matches!(name, "cancel" | "resume" | "set_policy") {
            128 * 1024
        } else {
            1024 * 1024
        },
    }
}
#[derive(Clone, Debug)]
pub struct Client {
    api: Arc<dyn ApiClient>,
}
#[derive(Deserialize)]
enum Never {}
impl Client {
    /// # Errors
    /// Rejects invalid resources or unavailable exact operation versions.
    pub fn new(api: Arc<dyn ApiClient>) -> Result<Self> {
        if operations()
            .iter()
            .any(|spec| !api.operations().contains(spec))
        {
            return Err(ApiError::Unavailable);
        }
        Ok(Self { api })
    }
    /// # Errors
    /// Rejects malformed requests or responses and propagates transport refusals.
    pub async fn call(&self, request: Request) -> Result<Value> {
        request.validate()?;
        match call_json::<_, Value, Never>(&*self.api, &request.spec(), &request).await? {
            Ok(value) => {
                validate_reply(&request, &value).map_err(|error| {
                    if request.spec().effect == OperationEffect::Mutation {
                        ApiError::OutcomeUnknown
                    } else {
                        error
                    }
                })?;
                Ok(value)
            }
            Err(never) => match never {},
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep one complete ownership operation or acceptance scenario together"
)]
fn validate_reply(request: &Request, value: &Value) -> Result<()> {
    let invalid = || ApiError::Invalid("invalid automation reply".into());
    let object = value.as_object().ok_or_else(invalid)?;
    let text = |key: &str, maximum: usize| -> Result<&str> {
        object
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| s.len() <= maximum)
            .ok_or_else(invalid)
    };
    let number = |key: &str| -> Result<u64> { decimal(text(key, 20)?) };
    let state = |v: &Value| -> Result<()> {
        if matches!(
            v.as_str(),
            Some(
                "queued"
                    | "running"
                    | "passed"
                    | "failed"
                    | "unavailable"
                    | "cancelled"
                    | "interrupted"
                    | "superseded"
                    | "capacity_rejected"
            )
        ) {
            Ok(())
        } else {
            Err(invalid())
        }
    };
    match request {
        Request::Status => {
            if !matches!(
                text("readiness", 32)?,
                "disabled" | "available" | "storage_unavailable" | "browser_unavailable"
            ) {
                return Err(invalid());
            }
        }
        Request::SetPolicy { .. } => {
            number("revision")?;
        }
        Request::List {
            after,
            watermark,
            limit,
        } => {
            let rows = object
                .get("entries")
                .and_then(Value::as_array)
                .filter(|r| r.len() <= usize::from(*limit))
                .ok_or_else(invalid)?;
            let cut = number("after")?;
            let high = number("watermark")?;
            let mut previous = decimal(after)?;
            if watermark
                .as_deref()
                .map(decimal)
                .transpose()?
                .is_some_and(|w| w != high)
                || cut < previous
                || cut > high
                || !object.get("more").is_some_and(Value::is_boolean)
            {
                return Err(invalid());
            }
            for row in rows {
                fields(
                    row,
                    &[
                        "id",
                        "task_id",
                        "created_ms",
                        "rule_revision",
                        "source",
                        "rule",
                        "url",
                        "environment",
                        "sha",
                        "state",
                        "exploration",
                        "verdict",
                        "session_id",
                    ],
                )?;
                let id = decimal(row["id"].as_str().ok_or_else(invalid)?)?;
                if id <= previous || id > cut {
                    return Err(invalid());
                }
                previous = id;
                for key in ["task_id", "created_ms", "rule_revision"] {
                    decimal(row[key].as_str().ok_or_else(invalid)?)?;
                }
                for (key, max) in [
                    ("source", 128),
                    ("rule", 128),
                    ("url", 4096),
                    ("environment", 128),
                    ("sha", 64),
                ] {
                    if row[key].as_str().is_none_or(|s| s.len() > max) {
                        return Err(invalid());
                    }
                }
                state(&row["state"])?;
                exploration(&row["exploration"])?;
                if !row["verdict"].is_null() {
                    outcome(&row["verdict"])?;
                }
                sha(&row["sha"], &[40, 64])?;
                session_id(&row["session_id"])?;
            }
            if object["more"] == true && cut == decimal(after)? {
                return Err(invalid());
            }
        }
        Request::Get { id } => {
            fields(
                value,
                &[
                    "id",
                    "task_id",
                    "created_ms",
                    "state",
                    "may_cancel",
                    "may_resume",
                    "exploration",
                    "session_id",
                    "current_rule_revision",
                    "report",
                    "deployment",
                    "rule",
                    "result",
                ],
            )?;
            if text("id", 20)? != id {
                return Err(invalid());
            }
            number("task_id")?;
            number("created_ms")?;
            state(&object["state"])?;
            for key in ["may_cancel", "may_resume"] {
                if !object.get(key).is_some_and(Value::is_boolean) {
                    return Err(invalid());
                }
            }
            if !matches!(
                object["exploration"].as_str(),
                Some(
                    "not_started"
                        | "starting"
                        | "running"
                        | "complete"
                        | "failed"
                        | "interrupted"
                        | "cancelled"
                )
            ) {
                return Err(invalid());
            }
            session_id(&object["session_id"])?;
            if let Some(s) = object["current_rule_revision"].as_str() {
                decimal(s)?;
            } else if !object["current_rule_revision"].is_null() {
                return Err(invalid());
            }
            nullable_text(&object["report"], 64 * 1024)?;
            deployment_reply(&object["deployment"])?;
            rule_reply(&object["rule"])?;
            if !object["result"].is_null() {
                check_reply(&object["result"])?;
            }
        }
        Request::Cancel { id, .. } => {
            fields(value, &["id", "state"])?;
            if text("id", 20)? != id {
                return Err(invalid());
            }
            state(&object["state"])?;
        }
        Request::Resume { .. } => {
            fields(value, &["id", "state"])?;
            if number("id")? == 0 {
                return Err(invalid());
            }
            state(&object["state"])?;
        }
        Request::Artifact { .. } => {
            use base64::{Engine, engine::general_purpose::STANDARD};
            let encoded = text("png", 700 * 1024)?;
            let bytes = STANDARD.decode(encoded).map_err(|_| invalid())?;
            if bytes.len() > 512 * 1024
                || !bytes.starts_with(b"\x89PNG\r\n\x1a\n")
                || STANDARD.encode(bytes) != encoded
            {
                return Err(invalid());
            }
        }
        Request::Policy => {
            number("revision")?;
            if !object.get("rules").is_some_and(Value::is_object)
                || !object.get("grants").is_some_and(Value::is_array)
            {
                return Err(invalid());
            }
        }
        Request::Diagnostics => {
            if !object
                .get("storage_available")
                .is_some_and(Value::is_boolean)
            {
                return Err(invalid());
            }
        }
    }
    Ok(())
}
fn invalid_reply() -> ApiError {
    ApiError::Invalid("invalid automation reply".into())
}
fn fields(value: &Value, required: &[&str]) -> Result<()> {
    let object = value.as_object().ok_or_else(invalid_reply)?;
    if object.len() != required.len() || required.iter().any(|key| !object.contains_key(*key)) {
        return Err(invalid_reply());
    }
    Ok(())
}
fn bounded_text(value: &Value, maximum: usize) -> Result<&str> {
    value
        .as_str()
        .filter(|text| text.len() <= maximum)
        .ok_or_else(invalid_reply)
}
fn nullable_text(value: &Value, maximum: usize) -> Result<()> {
    if !value.is_null() {
        bounded_text(value, maximum)?;
    }
    Ok(())
}
fn session_id(value: &Value) -> Result<()> {
    if !value.is_null() {
        let text = value.as_str().ok_or_else(invalid_reply)?;
        rsi_agent_session_protocol::SessionId::new(text).map_err(|_| invalid_reply())?;
    }
    Ok(())
}
fn unsigned(value: &Value, maximum: u64) -> Result<u64> {
    value
        .as_u64()
        .filter(|number| *number <= maximum)
        .ok_or_else(invalid_reply)
}
fn positive(value: &Value, maximum: u64) -> Result<()> {
    if unsigned(value, maximum)? == 0 {
        return Err(invalid_reply());
    }
    Ok(())
}
fn boolean(value: &Value) -> Result<()> {
    value.as_bool().map(|_| ()).ok_or_else(invalid_reply)
}
fn array(value: &Value, maximum: usize) -> Result<&[Value]> {
    value
        .as_array()
        .filter(|items| items.len() <= maximum)
        .map(Vec::as_slice)
        .ok_or_else(invalid_reply)
}
fn sha(value: &Value, lengths: &[usize]) -> Result<()> {
    let text = bounded_text(value, 64)?;
    if !lengths.contains(&text.len())
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
    {
        return Err(invalid_reply());
    }
    Ok(())
}
fn exploration(value: &Value) -> Result<()> {
    if !matches!(
        value.as_str(),
        Some(
            "not_started"
                | "starting"
                | "running"
                | "complete"
                | "failed"
                | "interrupted"
                | "cancelled"
        )
    ) {
        return Err(invalid_reply());
    }
    Ok(())
}
fn outcome(value: &Value) -> Result<()> {
    if !matches!(
        value.as_str(),
        Some(
            "pass"
                | "assertion_failed"
                | "target_unavailable"
                | "timeout"
                | "policy_blocked"
                | "infrastructure_failed"
                | "cancelled"
        )
    ) {
        return Err(invalid_reply());
    }
    Ok(())
}
fn deployment_reply(value: &Value) -> Result<()> {
    fields(
        value,
        &[
            "repository_id",
            "deployment_id",
            "status_id",
            "deployment_created_ms",
            "status_created_ms",
            "environment",
            "sha",
            "url",
        ],
    )?;
    for key in ["repository_id", "deployment_id", "status_id"] {
        positive(&value[key], i64::MAX as u64)?;
    }
    for key in ["deployment_created_ms", "status_created_ms"] {
        positive(&value[key], 253_402_300_799_999)?;
    }
    bounded_text(&value["environment"], 128)?;
    bounded_text(&value["url"], 4096)?;
    sha(&value["sha"], &[40, 64])
}
fn assertion(value: &Value) -> Result<()> {
    match value["kind"].as_str() {
        Some("text_visible") => {
            fields(value, &["kind", "text"])?;
            bounded_text(&value["text"], 1024)?;
        }
        Some("role_visible") => {
            fields(value, &["kind", "role", "name"])?;
            bounded_text(&value["role"], 64)?;
            bounded_text(&value["name"], 1024)?;
        }
        Some("final_url") => {
            fields(value, &["kind", "url"])?;
            bounded_text(&value["url"], 4096)?;
        }
        _ => return Err(invalid_reply()),
    }
    Ok(())
}
fn rule_reply(value: &Value) -> Result<()> {
    fields(
        value,
        &[
            "id",
            "revision",
            "enabled",
            "repository_id",
            "environment",
            "preview_host_suffix",
            "path_prefix",
            "dependency_hosts",
            "checks",
            "explore_on_failure",
            "authorized_catalog_digest",
            "model",
            "turn_budget",
            "max_rounds",
        ],
    )?;
    for key in ["id", "environment"] {
        bounded_text(&value[key], 128)?;
    }
    bounded_text(&value["preview_host_suffix"], 253)?;
    bounded_text(&value["path_prefix"], 1024)?;
    for key in ["revision", "repository_id"] {
        positive(&value[key], i64::MAX as u64)?;
    }
    for key in ["enabled", "explore_on_failure"] {
        boolean(&value[key])?;
    }
    for host in array(&value["dependency_hosts"], 16)? {
        bounded_text(host, 253)?;
    }
    sha(&value["authorized_catalog_digest"], &[64])?;
    let model: rsi_ai_protocol::ModelRef =
        serde_json::from_value(value["model"].clone()).map_err(|_| invalid_reply())?;
    model.validate().map_err(|_| invalid_reply())?;
    let budget: rsi_agent_session_protocol::TurnBudget =
        serde_json::from_value(value["turn_budget"].clone()).map_err(|_| invalid_reply())?;
    budget.validate().map_err(|_| invalid_reply())?;
    if budget.maximum_elapsed_ms() > 120_000
        || budget.maximum_provider_attempts() > 8
        || budget.maximum_tool_calls() > 16
        || budget.maximum_generated_records() > 256
        || budget.maximum_generated_record_bytes() > 1_048_576
    {
        return Err(invalid_reply());
    }
    positive(&value["max_rounds"], 2)?;
    fields(&value["checks"], &["entry_identity", "assertions"])?;
    bounded_text(&value["checks"]["entry_identity"], 1024)?;
    let predicates = array(&value["checks"]["assertions"], 16)?;
    if predicates.is_empty() {
        return Err(invalid_reply());
    }
    for predicate in predicates {
        assertion(predicate)?;
    }
    Ok(())
}
fn check_reply(value: &Value) -> Result<()> {
    fields(
        value,
        &[
            "outcome",
            "final_url",
            "assertions",
            "snapshot",
            "dialogs_dismissed",
            "evidence_error",
        ],
    )?;
    outcome(&value["outcome"])?;
    bounded_text(&value["final_url"], 4096)?;
    bounded_text(&value["snapshot"], 64 * 1024)?;
    nullable_text(&value["evidence_error"], 1024)?;
    unsigned(&value["dialogs_dismissed"], u64::from(u32::MAX))?;
    let predicates = array(&value["assertions"], 16)?;
    if value["outcome"] == "policy_blocked" && (!predicates.is_empty() || value["snapshot"] != "") {
        return Err(invalid_reply());
    }
    for predicate in predicates {
        fields(predicate, &["assertion", "passed", "detail"])?;
        assertion(&predicate["assertion"])?;
        boolean(&predicate["passed"])?;
        bounded_text(&predicate["detail"], 1024)?;
    }
    if (value["outcome"] == "pass"
        && (predicates.is_empty() || predicates.iter().any(|p| p["passed"] != true)))
        || (value["outcome"] == "assertion_failed"
            && !predicates.iter().any(|p| p["passed"] == false))
    {
        return Err(invalid_reply());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use rsi_api_protocol::{
        ApiMessage, ApiOutput, ByteBudget, ConnectionDescription, EndpointId, HostEpoch,
        RetainedBytes,
    };
    #[derive(Debug)]
    struct Replies {
        description: ConnectionDescription,
        specs: Vec<OperationSpec>,
        reply: Value,
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait::async_trait]
    impl ApiClient for Replies {
        fn description(&self) -> &ConnectionDescription {
            &self.description
        }
        fn operations(&self) -> &[OperationSpec] {
            &self.specs
        }
        fn input_budget(&self, _: OperationClass) -> ByteBudget {
            ByteBudget::default()
        }
        async fn call(&self, _: &OperationSpec, _: RetainedBytes) -> Result<ApiOutput> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ApiOutput::Reply(ApiMessage {
                json: ByteBudget::default().encode(&self.reply, 1024 * 1024)?,
                binary: None,
            }))
        }
    }
    fn replies(value: Value) -> Arc<Replies> {
        Arc::new(Replies {
            description: ConnectionDescription {
                wire_version: 1,
                endpoint_id: EndpointId::from_bytes([1; 16]),
                host_epoch: HostEpoch::from_bytes([2; 16]),
            },
            specs: operations(),
            reply: value,
            calls: std::sync::atomic::AtomicUsize::new(0),
        })
    }
    #[tokio::test]
    async fn malformed_mutation_replies_are_uncertain_without_retrying() {
        for request in [
            Request::Cancel {
                id: "1".into(),
                request_id: "cancel-once".into(),
            },
            Request::Resume {
                id: "1".into(),
                request_id: "resume-once".into(),
                rule_revision: "1".into(),
            },
            Request::SetPolicy {
                expected_revision: "0".into(),
                policy: serde_json::json!({}),
            },
        ] {
            let api = replies(serde_json::json!({}));
            assert_eq!(
                Client::new(api.clone()).unwrap().call(request).await,
                Err(ApiError::OutcomeUnknown)
            );
            assert_eq!(api.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        }
        let api = replies(serde_json::json!({}));
        assert!(matches!(
            Client::new(api.clone())
                .unwrap()
                .call(Request::Status)
                .await,
            Err(ApiError::Invalid(_))
        ));
        assert!(matches!(
            Client::new(api.clone())
                .unwrap()
                .call(Request::Cancel {
                    id: "0".into(),
                    request_id: "invalid".into()
                })
                .await,
            Err(ApiError::Invalid(_))
        ));
        assert_eq!(
            api.calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "invalid input must not dispatch"
        );
    }
    #[tokio::test]
    async fn mutation_receipts_with_unknown_fields_are_uncertain_without_retry() {
        for request in [
            Request::Cancel {
                id: "1".into(),
                request_id: "cancel".into(),
            },
            Request::Resume {
                id: "1".into(),
                request_id: "resume".into(),
                rule_revision: "1".into(),
            },
        ] {
            let api = replies(serde_json::json!({"id":"1","state":"cancelled","unverified":true}));
            assert_eq!(
                Client::new(api.clone()).unwrap().call(request).await,
                Err(ApiError::OutcomeUnknown)
            );
            assert_eq!(api.calls.load(std::sync::atomic::Ordering::Relaxed), 1);
        }
    }
    fn attempt_reply() -> Value {
        serde_json::json!({
            "id":"1", "task_id":"1", "created_ms":"1800000000000", "state":"failed", "may_cancel":false, "may_resume":true, "current_rule_revision":"1", "session_id":null, "report":null, "exploration":"not_started",
            "deployment":{"repository_id":7,"deployment_id":1,"status_id":1,"deployment_created_ms":1_800_000_000_000_u64,"status_created_ms":1_800_000_000_000_u64,"environment":"preview","sha":"a".repeat(40),"url":"https://preview.example/"},
            "rule":{"id":"preview","revision":1,"enabled":true,"repository_id":7,"environment":"preview","preview_host_suffix":"example","path_prefix":"/","dependency_hosts":[],"checks":{"entry_identity":"Preview","assertions":[{"kind":"text_visible","text":"Ready"}]},"explore_on_failure":true,"authorized_catalog_digest":"0".repeat(64),"model":{"deployment":"fixture","model":"fixture-model"},"turn_budget":{"maximum_elapsed_ms":120_000,"maximum_provider_attempts":8,"maximum_tool_calls":16,"maximum_generated_records":256,"maximum_generated_record_bytes":1_048_576},"max_rounds":2},
            "result":{"outcome":"assertion_failed","final_url":"https://preview.example/","assertions":[{"assertion":{"kind":"text_visible","text":"Ready"},"passed":false,"detail":"absent"}],"snapshot":"", "dialogs_dismissed":0,"evidence_error":null}
        })
    }
    #[test]
    fn get_requires_complete_bounded_nested_shapes_and_nullable_types() {
        let request = Request::Get { id: "1".into() };
        assert!(validate_reply(&request, &attempt_reply()).is_ok());
        for (path, forged) in [
            ("/rule", serde_json::json!({})),
            (
                "/rule/checks/assertions/0/text",
                serde_json::json!("x".repeat(1025)),
            ),
            ("/deployment/sha", serde_json::json!("z".repeat(40))),
            ("/deployment/status_id", serde_json::json!(0)),
            ("/result/assertions/0/passed", serde_json::json!("yes")),
            (
                "/result/assertions/0/detail",
                serde_json::json!("x".repeat(1025)),
            ),
            ("/result/final_url", serde_json::json!("x".repeat(4097))),
            ("/result/evidence_error", serde_json::json!([])),
            ("/result", serde_json::json!(false)),
            ("/report", serde_json::json!({})),
            ("/session_id", serde_json::json!(42)),
            ("/current_rule_revision", serde_json::json!(false)),
        ] {
            let mut value = attempt_reply();
            *value.pointer_mut(path).unwrap() = forged;
            assert!(validate_reply(&request, &value).is_err(), "{path}");
        }
    }
    #[test]
    fn list_bounds_visible_attempt_fields_without_rejecting_session_links() {
        let request = Request::List {
            after: "0".into(),
            watermark: None,
            limit: 50,
        };
        let page = serde_json::json!({"entries":[{
            "id":"1", "task_id":"1", "created_ms":"1800000000000", "rule_revision":"1",
            "source":"github", "rule":"preview", "url":"https://preview.example/",
            "environment":"preview", "sha":"a".repeat(40), "state":"failed",
            "exploration":"complete", "verdict":"assertion_failed",
            "session_id":rsi_agent_session_protocol::SessionId::new("fixture-session").unwrap()
        }],"after":"1","watermark":"1","more":false});
        assert!(validate_reply(&request, &page).is_ok());
        for (path, forged) in [
            ("/entries/0/exploration", serde_json::json!("unknown")),
            ("/entries/0/verdict", serde_json::json!({})),
            ("/entries/0/session_id", serde_json::json!([])),
            ("/entries/0/session_id", serde_json::json!("")),
            ("/entries/0/sha", serde_json::json!("z".repeat(40))),
            ("/entries/0/url", serde_json::json!("x".repeat(4097))),
        ] {
            let mut value = page.clone();
            *value.pointer_mut(path).unwrap() = forged;
            assert!(validate_reply(&request, &value).is_err(), "{path}");
        }
    }
    #[test]
    fn rejects_forged_coordinates_and_stalled_or_foreign_pages() {
        let request = Request::List {
            after: "4".into(),
            watermark: Some("9".into()),
            limit: 50,
        };
        assert!(
            validate_reply(
                &request,
                &serde_json::json!({"entries":[],"after":"5","watermark":"9","more":true})
            )
            .is_ok()
        );
        for value in [
            serde_json::json!({"entries":[],"after":"4","watermark":"9","more":true}),
            serde_json::json!({"entries":[],"after":"05","watermark":"9","more":false}),
            serde_json::json!({"entries":[],"after":"5","watermark":"8","more":false}),
        ] {
            assert!(validate_reply(&request, &value).is_err());
        }
        assert!(
            validate_reply(
                &Request::Status,
                &serde_json::json!({"readiness":"unconfined"})
            )
            .is_err()
        );
    }
}
