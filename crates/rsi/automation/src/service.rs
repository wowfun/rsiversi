use crate::now;
use crate::{AdmissionError, Attempt, Deployment, Ledger, PolicyOwner};
use async_trait::async_trait;
use rsi_browser::NativeRuntime;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngressSource {
    pub id: String,
    pub credential: rsi_credentials_protocol::CredentialRef,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    Disabled,
    Available,
    StorageUnavailable,
    BrowserUnavailable,
}
/// Product integration creates a protected native Goal; browser data never becomes Human input.
#[async_trait]
pub trait Explorer: fmt::Debug + Send + Sync + 'static {
    async fn explore(
        &self,
        source: &str,
        attempt: &Attempt,
        browser: Arc<dyn rsi_browser::PreviewBrowser>,
        stop: CancellationToken,
    ) -> Result<(String, String), String>;
}
#[derive(Debug)]
pub struct AutomationService {
    pub ledger: Arc<Ledger>,
    pub policy: Arc<PolicyOwner>,
    pub browser: Option<Arc<NativeRuntime>>,
    sources: BTreeMap<String, rsi_credentials_protocol::SecretValue>,
    explorer: Option<Arc<dyn Explorer>>,
    pub(crate) tasks: TaskTracker,
    pub(crate) stop: CancellationToken,
    active: Mutex<BTreeMap<u64, CancellationToken>>,
}
struct AttemptReservation {
    owner: Arc<AutomationService>,
    id: u64,
    stop: CancellationToken,
    converged: bool,
}
impl Drop for AttemptReservation {
    fn drop(&mut self) {
        self.stop.cancel();
        self.owner.active().remove(&self.id);
        if !self.converged {
            self.owner.ledger.fence();
        }
    }
}
impl AutomationService {
    fn active(&self) -> MutexGuard<'_, BTreeMap<u64, CancellationToken>> {
        self.active.lock().unwrap_or_else(|poisoned| {
            self.ledger.fence();
            poisoned.into_inner()
        })
    }

    pub fn new(
        ledger: Arc<Ledger>,
        policy: Arc<PolicyOwner>,
        browser: Option<Arc<NativeRuntime>>,
        sources: BTreeMap<String, rsi_credentials_protocol::SecretValue>,
        explorer: Option<Arc<dyn Explorer>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            ledger,
            policy,
            browser,
            sources,
            explorer,
            tasks: TaskTracker::new(),
            stop: CancellationToken::new(),
            active: Mutex::new(BTreeMap::new()),
        })
    }
    pub fn readiness(&self) -> Readiness {
        if self.ledger.available() {
            match &self.browser {
                None => Readiness::Disabled,
                Some(browser) if browser.is_verified() => Readiness::Available,
                Some(_) => Readiness::BrowserUnavailable,
            }
        } else {
            Readiness::StorageUnavailable
        }
    }
    /// # Panics
    /// Panics if no Tokio runtime is entered.
    pub fn start(self: &Arc<Self>) {
        let owner = self.clone();
        self.tasks.spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            let mut last_retention = 0;
            loop {
                tokio::select! {
                    () = owner.stop.cancelled() => break,
                    _ = interval.tick() => {
                        let policy = owner.policy.clone();
                        let _ = owner.tasks.spawn_blocking(move || policy.flush_intake()).await;
                        let instant = now();
                        if instant.saturating_sub(last_retention) >= 60_000 {
                            let _ = owner.ledger.run(move |ledger| ledger.retain(instant)).await;
                            last_retention = instant;
                        }
                        if !matches!(owner.readiness(), Readiness::Available)
                            || owner.active().len() >= 2
                        {
                            continue;
                        }
                        let _ = owner.claim_and_dispatch(instant).await;
                    }
                }
            }
        });
    }
    async fn claim_and_dispatch(
        self: &Arc<Self>,
        instant: u64,
    ) -> Result<Option<u64>, AdmissionError> {
        let owner = self.clone();
        let runtime = tokio::runtime::Handle::current();
        self.ledger
            .run(move |ledger| owner.dispatch_claim(ledger, instant, &runtime))
            .await
    }
    fn dispatch_claim(
        self: &Arc<Self>,
        ledger: &Ledger,
        instant: u64,
        runtime: &tokio::runtime::Handle,
    ) -> Result<Option<u64>, AdmissionError> {
        self.dispatch_with(runtime, || ledger.claim(instant))
    }
    fn dispatch_with(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Handle,
        claim: impl FnOnce() -> Result<Option<Attempt>, AdmissionError>,
    ) -> Result<Option<u64>, AdmissionError> {
        let _dispatch = {
            let active = self.active();
            if self.stop.is_cancelled() || active.len() >= 2 {
                return Ok(None);
            }
            self.tasks.token()
        };
        let Some(attempt) = claim()? else {
            return Ok(None);
        };
        let id = attempt.id;
        let stop = self.stop.child_token();
        self.active().insert(id, stop.clone());
        let reservation = AttemptReservation {
            owner: self.clone(),
            id,
            stop,
            converged: false,
        };
        let owner = self.clone();
        self.tasks.spawn_on(
            async move {
                owner.run(attempt, reservation).await;
            },
            runtime,
        );
        Ok(Some(id))
    }
    async fn run(self: Arc<Self>, attempt: Attempt, mut reservation: AttemptReservation) {
        let stop = reservation.stop.clone();
        let deadline = stop.clone();
        let timer = self.tasks.spawn(attempt_deadline(deadline));
        let result = self.execute(&attempt, stop.clone()).await;
        if let Err(error) = result {
            let id = attempt.id;
            let cancelled = stop.is_cancelled();
            if self
                .ledger
                .run(move |ledger| ledger.fail_execution(id, cancelled, &error))
                .await
                .is_err()
            {
                self.ledger.fence();
            }
        }
        stop.cancel();
        let _ = timer.await;
        stop.cancel();
        reservation.converged = true;
    }
    async fn execute(&self, attempt: &Attempt, stop: CancellationToken) -> Result<(), String> {
        let source = tokio::select! { biased; () = stop.cancelled() => return Err("attempt cancelled".into()), source = self.source_for(attempt) => source? };
        let policy = self.policy.snapshot().map_err(|e| e.to_string())?;
        let rule = policy
            .rule(&source, &attempt.rule.id)
            .ok_or("rule removed")?;
        if !rule.enabled {
            return Err("rule disabled".into());
        }
        let browser = self.browser.as_ref().ok_or("browser unavailable")?;
        if stop.is_cancelled() {
            return Err("attempt cancelled".into());
        }
        let scope = browser
            .open(
                attempt.rule.policy(&attempt.deployment.url)?,
                &format!("attempt-{}-check", attempt.id),
            )
            .await
            .map_err(|error| error.to_string())?;
        let result = tokio::select! {()=stop.cancelled()=>Err("attempt cancelled".into()),result=scope.check(attempt.rule.checks.clone())=>result};
        let cleanup = scope.close().await;
        drop(scope);
        let id = attempt.id;
        let settled = self.settle_check(id, result, cleanup).await?;
        if self
            .ledger
            .run(move |ledger| ledger.eligible(id))
            .await
            .map_err(|e| e.to_string())?
            && self
                .policy
                .snapshot()
                .map_err(|e| e.to_string())?
                .rule(&source, &attempt.rule.id)
                .is_some_and(|r| r.enabled)
        {
            if self.explorer.is_none() {
                return Err("exploration integration unavailable".into());
            }
            if stop.is_cancelled() {
                return Err("attempt cancelled".into());
            }
            let scope = Arc::new(
                browser
                    .open_exploration(
                        attempt.rule.policy(&attempt.deployment.url)?,
                        &format!("attempt-{}-explore", attempt.id),
                    )
                    .await
                    .map_err(|error| error.to_string())?,
            );
            self.explore_failed(settled.id, scope, stop.clone()).await?;
        }
        Ok(())
    }
    async fn settle_check(
        &self,
        id: u64,
        result: Result<(rsi_browser::CheckResult, Vec<Vec<u8>>), String>,
        cleanup: Result<(), String>,
    ) -> Result<Attempt, String> {
        let (mut result, artifacts) = result.map_err(|error| match &cleanup {
            Ok(()) => error,
            Err(cleanup) => format!("{error}; preview cleanup: {cleanup}"),
        })?;
        if let Err(error) = &cleanup {
            let diagnostic = format!(
                "{}preview cleanup: {error}",
                result
                    .evidence_error
                    .as_ref()
                    .map_or(String::new(), |prior| format!("{prior}; "))
            );
            result.evidence_error = Some(diagnostic.chars().take(256).collect());
        }
        let settled = self
            .ledger
            .run(move |ledger| ledger.settle(id, result, artifacts, now()))
            .await
            .map_err(|e| e.to_string())?;
        cleanup?;
        Ok(settled)
    }
    /// Consumes an independently admitted failure and a product-owned private
    /// preview port. Serialized identities and page content grant no authority.
    /// # Errors
    /// Fails when fresh authority, investigation execution, retirement or durable settlement fails.
    pub async fn explore_failed(
        &self,
        id: u64,
        browser: Arc<dyn rsi_browser::PreviewBrowser>,
        stop: CancellationToken,
    ) -> Result<(String, String), String> {
        let result = async {
            if !self
                .ledger
                .run(move |ledger| ledger.eligible(id))
                .await
                .map_err(|e| e.to_string())?
                || stop.is_cancelled()
            {
                return Err("attempt has no fresh exploration authority".into());
            }
            let attempt = self
                .ledger
                .run(move |ledger| ledger.get(id))
                .await
                .map_err(|e| e.to_string())?;
            let source = self.source_for(&attempt).await?;
            if !self
                .policy
                .snapshot()
                .map_err(|e| e.to_string())?
                .rule(&source, &attempt.rule.id)
                .is_some_and(|r| r.enabled)
            {
                return Err("standing rule unavailable".into());
            }
            self.explorer
                .as_ref()
                .ok_or("exploration integration unavailable")?
                .explore(&source, &attempt, browser.clone(), stop.clone())
                .await
        }
        .await;
        let cleanup = browser.close().await;
        let result = match (result, cleanup) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(error), Err(cleanup)) => Err(format!("{error}; preview cleanup: {cleanup}")),
        };
        match result {
            Ok((session, report)) => {
                let saved_session = session.clone();
                let saved_report = report.clone();
                if let Err(error) = self
                    .ledger
                    .run(move |ledger| {
                        ledger.record_exploration(id, saved_session, Some(saved_report))
                    })
                    .await
                {
                    let message = error.to_string();
                    let cancelled = stop.is_cancelled();
                    let saved = message.clone();
                    self.ledger
                        .run(move |ledger| ledger.fail_execution(id, cancelled, &saved))
                        .await
                        .map_err(|e| format!("{message}; failure settlement: {e}"))?;
                    return Err(message);
                }
                Ok((session, report))
            }
            Err(error) => {
                let cancelled = stop.is_cancelled();
                let saved = error.clone();
                self.ledger
                    .run(move |ledger| ledger.fail_execution(id, cancelled, &saved))
                    .await
                    .map_err(|e| format!("{error}; failure settlement: {e}"))?;
                Err(error)
            }
        }
    }
    /// # Errors
    /// Fails when the authoritative attempt source cannot be read.
    pub async fn source_for(&self, attempt: &Attempt) -> Result<String, String> {
        let task = attempt.task_id;
        self.ledger
            .run(move |ledger| ledger.source(task))
            .await
            .map_err(|e| e.to_string())
    }
    /// # Errors
    /// Rejects identity conflicts, capacity or failed and uncertain durable cancellation.
    /// # Panics
    /// Panics if no Tokio runtime is entered.
    pub async fn cancel(
        self: &Arc<Self>,
        id: u64,
        request: &str,
    ) -> Result<Attempt, AdmissionError> {
        let owner = self.clone();
        let request = request.to_owned();
        self.ledger
            .run(move |ledger| {
                let result = ledger.cancel(id, &request)?;
                if let Some(token) = owner.active().get(&id) {
                    token.cancel();
                }
                Ok(result)
            })
            .await
    }
    /// # Panics
    /// Panics if no Tokio runtime is entered.
    pub async fn revoke_disabled(&self) {
        let policy = self.policy.snapshot().ok();
        let active = self
            .active()
            .iter()
            .map(|(id, token)| (*id, token.clone()))
            .collect::<Vec<_>>();
        for (id, token) in active {
            if let Ok((a, source)) = self
                .ledger
                .run(move |ledger| {
                    let a = ledger.get(id)?;
                    let source = ledger.source(a.task_id)?;
                    Ok((a, source))
                })
                .await
                && policy
                    .as_ref()
                    .and_then(|policy| policy.rule(&source, &a.rule.id))
                    .is_none_or(|rule| !rule.enabled)
            {
                token.cancel();
            }
        }
    }
    /// # Errors
    /// Rejects invalid authentication, inconsistent events or failed atomic rule admission.
    pub async fn accept(
        self: &Arc<Self>,
        source: &str,
        delivery: &str,
        event: &str,
        signature: &str,
        body: &[u8],
    ) -> Result<Vec<crate::Receipt>, AdmissionError> {
        if self.stop.is_cancelled() || !self.ledger.available() {
            return Err(AdmissionError::Unavailable);
        }
        if body.len() > 256 * 1024 {
            return Err(AdmissionError::Capacity);
        }
        crate::protocol::identity(delivery).map_err(AdmissionError::Invalid)?;
        let secret = self.sources.get(source).ok_or(AdmissionError::NotFound)?;
        let signature = signature
            .strip_prefix("sha256=")
            .and_then(|s| hex::decode(s).ok())
            .filter(|s| s.len() == 32)
            .ok_or(AdmissionError::Unauthorized)?;
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.expose_secret().as_bytes());
        ring::hmac::verify(&key, body, &signature).map_err(|_| AdmissionError::Unauthorized)?;
        let value: Value = serde_json::from_slice(body)
            .map_err(|_| AdmissionError::Invalid("invalid webhook JSON".into()))?;
        if event != "deployment_status" {
            if value.get("deployment_status").is_some() {
                return Err(AdmissionError::Invalid(
                    "deployment body has mismatched event header".into(),
                ));
            }
            return Ok(vec![]);
        }
        let state = value
            .get("deployment_status")
            .and_then(|status| status.get("state"))
            .and_then(Value::as_str)
            .filter(|state| {
                matches!(
                    *state,
                    "error"
                        | "failure"
                        | "inactive"
                        | "pending"
                        | "queued"
                        | "in_progress"
                        | "success"
                )
            })
            .ok_or_else(|| AdmissionError::Invalid("invalid deployment_status state".into()))?;
        if state != "success" {
            return Ok(vec![]);
        }
        let deployment = deployment(&value)?;
        let policy = self.policy.snapshot()?;
        let rules = policy
            .rules
            .get(source)
            .into_iter()
            .flatten()
            .filter(|r| {
                r.enabled
                    && r.repository_id == deployment.repository_id
                    && r.environment == deployment.environment
            })
            .cloned()
            .collect::<Vec<_>>();
        if rules.is_empty() {
            return Ok(vec![]);
        }
        self.ledger
            .admit_rules(
                source.into(),
                delivery.into(),
                hex::encode(Sha256::digest(body)),
                rules,
                deployment,
                now(),
            )
            .await
    }
    /// # Errors
    /// Rejects non-loopback endpoints or failure to bind the listener.
    pub async fn listen(
        self: &Arc<Self>,
        address: std::net::SocketAddr,
    ) -> Result<std::net::SocketAddr, String> {
        if !address.ip().is_loopback() {
            return Err(
                "webhook listener requires loopback and an operator HTTPS reverse proxy".into(),
            );
        }
        self.listen_with_slots(address, Arc::new(tokio::sync::Semaphore::new(32)))
            .await
    }
    async fn listen_with_slots(
        self: &Arc<Self>,
        address: std::net::SocketAddr,
        slots: Arc<tokio::sync::Semaphore>,
    ) -> Result<std::net::SocketAddr, String> {
        let listener = TcpListener::bind(address)
            .await
            .map_err(|e| e.to_string())?;
        let actual = listener.local_addr().map_err(|e| e.to_string())?;
        let owner = self.clone();
        self.tasks.spawn(async move {
            loop {
                let Some((mut stream, _)) = accept_with_retry(&owner.stop, || listener.accept()).await else { break; };
                let Ok(permit)=slots.clone().try_acquire_owned() else {
                    owner.policy.reject(None,None,crate::intake::Reason::Busy);
                    // Inline bounded delivery creates no unbounded rejection tasks.
                    tokio::select! { biased;
                        () = owner.stop.cancelled() => break,
                        _ = tokio::time::timeout(Duration::from_millis(500), reply(&mut stream,"503 Service Unavailable",json!({"error":"admission_unconfirmed","reason":"busy"}))) => {},
                    }
                    continue;
                };
                let child=owner.clone();
                owner.tasks.spawn(async move { let _permit=permit; serve(child,stream).await; });
            }
        });
        Ok(actual)
    }
    /// # Panics
    /// Panics if no Tokio runtime is entered.
    pub async fn close(&self) {
        self.stop.cancel();
        for token in self.active().values() {
            token.cancel();
        }
        self.tasks.close();
        self.tasks.wait().await;
        self.ledger.drain().await;
        let policy = self.policy.clone();
        let _ = tokio::task::spawn_blocking(move || policy.flush_intake()).await;
    }
}
async fn accept_with_retry<T, F: std::future::Future<Output = std::io::Result<T>>>(
    stop: &CancellationToken,
    mut accept: impl FnMut() -> F,
) -> Option<T> {
    loop {
        let result = tokio::select! { biased; () = stop.cancelled() => return None, result = accept() => result };
        if let Ok(value) = result {
            return Some(value);
        }
        tokio::select! { biased; () = stop.cancelled() => return None, () = tokio::time::sleep(Duration::from_millis(100)) => {} }
    }
}

async fn attempt_deadline(stop: CancellationToken) {
    tokio::select! {
        () = stop.cancelled() => {},
        () = tokio::time::sleep(Duration::from_mins(10)) => stop.cancel(),
    }
}

fn deployment(v: &Value) -> Result<Deployment, AdmissionError> {
    let number = |v: &Value| {
        v.as_u64()
            .filter(|n| *n > 0 && i64::try_from(*n).is_ok())
            .ok_or_else(|| AdmissionError::Invalid("invalid deployment identity".into()))
    };
    let text = |v: &Value| {
        v.as_str()
            .map(str::to_owned)
            .ok_or_else(|| AdmissionError::Invalid("missing deployment field".into()))
    };
    let timestamp = |v: &Value| -> Result<u64, AdmissionError> {
        let text = text(v)?;
        let date =
            time::OffsetDateTime::parse(&text, &time::format_description::well_known::Rfc3339)
                .map_err(|_| AdmissionError::Invalid("invalid deployment timestamp".into()))?;
        u64::try_from(date.unix_timestamp_nanos() / 1_000_000)
            .map_err(|_| AdmissionError::Invalid("invalid deployment timestamp".into()))
    };
    let d = &v["deployment"];
    let s = &v["deployment_status"];
    let deployment = Deployment {
        repository_id: number(&v["repository"]["id"])?,
        deployment_id: number(&d["id"])?,
        status_id: number(&s["id"])?,
        deployment_created_ms: timestamp(&d["created_at"])?,
        status_created_ms: timestamp(&s["created_at"])?,
        environment: text(s.get("environment").unwrap_or(&d["environment"]))?,
        sha: text(&d["sha"])?,
        url: text(&s["environment_url"])?,
    };
    deployment.validate().map_err(AdmissionError::Invalid)?;
    Ok(deployment)
}
async fn serve(owner: Arc<AutomationService>, mut stream: tokio::net::TcpStream) {
    let parsed = tokio::select! { biased; ()=owner.stop.cancelled()=>return, parsed=tokio::time::timeout(Duration::from_secs(7), read_request(&mut stream))=>parsed };
    let request = match parsed {
        Ok(Ok(request)) => request,
        failure => {
            let (status, reason) = match failure {
                Err(_) => ("408 Request Timeout", crate::intake::Reason::Timeout),
                Ok(Err(true)) => ("413 Content Too Large", crate::intake::Reason::Oversized),
                _ => ("400 Bad Request", crate::intake::Reason::Malformed),
            };
            owner.policy.reject(None, None, reason);
            let _ = tokio::time::timeout(
                Duration::from_millis(500),
                reply(&mut stream, status, json!({"error":"invalid_request"})),
            )
            .await;
            return;
        }
    };
    let (source, delivery, event, signature, body) = request;
    // Once durable admission starts, its blocking owner settles even if this waiter times out.
    let accepted = tokio::time::timeout(
        Duration::from_secs(1),
        owner.accept(&source, &delivery, &event, &signature, &body),
    )
    .await;
    let (status, value, reason) = match accepted {
        Ok(Ok(receipts)) if receipts.is_empty() => ("204 No Content", Value::Null, None),
        Ok(Ok(receipts)) => ("202 Accepted", json!({"receipts":receipts}), None),
        Err(_) => (
            "503 Service Unavailable",
            json!({"error":"admission_unconfirmed"}),
            Some(crate::intake::Reason::StorageUnavailable),
        ),
        Ok(Err(
            AdmissionError::Unavailable | AdmissionError::OutcomeUnknown | AdmissionError::Corrupt,
        )) => (
            "503 Service Unavailable",
            json!({"error":"storage_unavailable"}),
            Some(crate::intake::Reason::StorageUnavailable),
        ),
        Ok(Err(AdmissionError::Conflict)) => (
            "409 Conflict",
            json!({"error":"identity_conflict"}),
            Some(crate::intake::Reason::Conflict),
        ),
        Ok(Err(AdmissionError::Capacity)) => (
            "503 Service Unavailable",
            json!({"error":"capacity"}),
            Some(crate::intake::Reason::Capacity),
        ),
        Ok(Err(AdmissionError::Unauthorized)) => (
            "401 Unauthorized",
            json!({"error":"unauthorized"}),
            Some(crate::intake::Reason::Unauthorized),
        ),
        _ => (
            "422 Unprocessable Content",
            json!({"error":"invalid_event"}),
            Some(crate::intake::Reason::InvalidEvent),
        ),
    };
    if let Some(reason) = reason {
        owner.policy.reject(Some(&source), Some(&delivery), reason);
    }
    let _ = tokio::time::timeout(
        Duration::from_millis(500),
        reply(&mut stream, status, value),
    )
    .await;
}
async fn read_request(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
) -> Result<(String, String, String, String, Vec<u8>), bool> {
    let mut bytes = vec![];
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let available = (16384usize.saturating_sub(bytes.len())).min(chunk.len());
        if available == 0 {
            return Err(true);
        }
        let read = stream
            .read(&mut chunk[..available])
            .await
            .map_err(|_| false)?;
        if read == 0 {
            return Err(false);
        }
        let search_from = bytes.len().saturating_sub(3);
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(index) = bytes[search_from..]
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
        {
            break search_from + index + 4;
        }
    };
    let headers = std::str::from_utf8(&bytes[..header_end]).map_err(|_| false)?;
    let mut lines = headers.split("\r\n");
    let parts = lines.next().ok_or(false)?.split(' ').collect::<Vec<_>>();
    if parts.len() != 3 || parts[0] != "POST" || parts[2] != "HTTP/1.1" {
        return Err(false);
    }
    let source = parts[1].strip_prefix("/github/").ok_or(false)?;
    crate::protocol::identity(source).map_err(|_| false)?;
    let mut fields = BTreeMap::new();
    for line in lines.filter(|l| !l.is_empty()) {
        let (name, value) = line.split_once(':').ok_or(false)?;
        if name.is_empty()
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || value.chars().any(|c| c.is_control() && c != '\t')
            || fields
                .insert(name.to_ascii_lowercase(), value.trim())
                .is_some()
        {
            return Err(false);
        }
    }
    if fields.contains_key("transfer-encoding") {
        return Err(false);
    }
    let encoded_length = fields.get("content-length").ok_or(false)?;
    if encoded_length.is_empty() || !encoded_length.bytes().all(|b| b.is_ascii_digit()) {
        return Err(false);
    }
    let length: usize = encoded_length.parse().map_err(|_| false)?;
    if length > 256 * 1024 {
        return Err(true);
    }
    let mut body = vec![0u8; length];
    let buffered = (bytes.len() - header_end).min(length);
    body[..buffered].copy_from_slice(&bytes[header_end..header_end + buffered]);
    stream
        .read_exact(&mut body[buffered..])
        .await
        .map_err(|_| false)?;
    Ok((
        source.into(),
        fields
            .get("x-github-delivery")
            .copied()
            .unwrap_or("")
            .into(),
        fields.get("x-github-event").copied().unwrap_or("").into(),
        fields
            .get("x-hub-signature-256")
            .copied()
            .unwrap_or("")
            .into(),
        body,
    ))
}
async fn reply(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    value: Value,
) -> Result<(), String> {
    let body = if status.starts_with("204") {
        vec![]
    } else {
        serde_json::to_vec(&value).map_err(|e| e.to_string())?
    };
    stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.map_err(|e|e.to_string())?;
    stream.write_all(&body).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn claim_io_releases_bookkeeping_while_dispatch_remains_tracked_through_close() {
        let directory = crate::test_directory();
        let ledger = Ledger::open(&directory.path().join("ledger"), now()).unwrap();
        let policy = Arc::new(PolicyOwner::open(directory.path().join("policy")).unwrap());
        let receipt = ledger
            .admit(
                "source".into(),
                "close-during-claim".into(),
                "b".repeat(64),
                crate::store::tests::rule(),
                crate::store::tests::deployment(1, now()),
                now(),
            )
            .await
            .unwrap();
        let owner = AutomationService::new(ledger.clone(), policy, None, BTreeMap::new(), None);
        let (entered, entering) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let runtime = tokio::runtime::Handle::current();
        let dispatch = std::thread::spawn({
            let owner = owner.clone();
            let ledger = ledger.clone();
            move || {
                owner.dispatch_with(&runtime, || {
                    entered.send(()).unwrap();
                    released.recv().unwrap();
                    ledger.claim(now())
                })
            }
        });
        entering.await.unwrap();
        let available = owner.active.try_lock().is_ok();
        if !available {
            release.send(()).unwrap();
            dispatch.join().unwrap().unwrap();
            owner.close().await;
            assert!(
                available,
                "durable claim must not hold the active-attempt mutex"
            );
            return;
        }
        let mut closing = Box::pin(owner.close());
        assert!(futures_util::poll!(&mut closing).is_pending());
        assert!(owner.stop.is_cancelled());
        assert!(
            !owner.tasks.is_empty(),
            "claim-to-worker handoff must retain task ownership"
        );
        release.send(()).unwrap();
        assert_eq!(dispatch.join().unwrap().unwrap(), Some(receipt.attempt_id));
        closing.await;
        assert!(owner.active().is_empty());
        assert_eq!(
            ledger.get(receipt.attempt_id).unwrap().state,
            crate::AttemptState::Cancelled
        );
        assert!(ledger.available());
    }

    #[tokio::test]
    async fn converged_reservation_releases_bookkeeping_without_fencing_a_healthy_ledger() {
        let directory = crate::test_directory();
        let ledger = Ledger::open(&directory.path().join("ledger"), now()).unwrap();
        let policy = Arc::new(PolicyOwner::open(directory.path().join("policy")).unwrap());
        let owner = AutomationService::new(ledger.clone(), policy, None, BTreeMap::new(), None);
        let token = owner.stop.child_token();
        owner.active().insert(1, token.clone());
        drop(AttemptReservation {
            owner: owner.clone(),
            id: 1,
            stop: token.clone(),
            converged: true,
        });
        assert!(token.is_cancelled());
        assert!(owner.active().is_empty());
        assert!(ledger.available());
        owner.close().await;
    }

    #[tokio::test]
    async fn poisoned_attempt_bookkeeping_does_not_panic_during_teardown() {
        for converged in [false, true] {
            let directory = crate::test_directory();
            let ledger = Ledger::open(&directory.path().join("ledger"), now()).unwrap();
            let policy = Arc::new(PolicyOwner::open(directory.path().join("policy")).unwrap());
            let owner = AutomationService::new(ledger.clone(), policy, None, BTreeMap::new(), None);
            let token = owner.stop.child_token();
            owner.active.lock().unwrap().insert(1, token.clone());
            let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _active = owner.active.lock().unwrap();
                panic!("bookkeeping interrupted");
            }));
            assert!(poisoned.is_err());
            let reservation = AttemptReservation {
                owner: owner.clone(),
                id: 1,
                stop: token.clone(),
                converged,
            };
            let dropped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                drop(reservation);
            }));
            assert!(
                dropped.is_ok(),
                "reservation cleanup panicked on a poisoned mutex"
            );
            assert!(token.is_cancelled());
            assert!(
                owner
                    .active
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_empty()
            );
            assert!(!ledger.available());

            let unwinding = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _reservation = AttemptReservation {
                    owner: owner.clone(),
                    id: 2,
                    stop: owner.stop.child_token(),
                    converged: false,
                };
                panic!("worker interrupted");
            }));
            assert_eq!(
                unwinding.unwrap_err().downcast_ref::<&str>(),
                Some(&"worker interrupted")
            );
            owner.close().await;
        }
    }

    #[test]
    fn cancellation_before_the_first_worker_poll_reaches_the_published_token() {
        let admission = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let worker = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let directory = crate::test_directory();
        let ledger = Ledger::open(&directory.path().join("ledger"), now()).unwrap();
        let policy = Arc::new(PolicyOwner::open(directory.path().join("policy")).unwrap());
        let receipt = admission
            .block_on(ledger.admit(
                "source".into(),
                "cancel-before-poll".into(),
                "b".repeat(64),
                crate::store::tests::rule(),
                crate::store::tests::deployment(1, now()),
                now(),
            ))
            .unwrap();
        let owner = AutomationService::new(ledger.clone(), policy, None, BTreeMap::new(), None);
        assert_eq!(
            owner
                .dispatch_claim(&ledger, now(), worker.handle())
                .unwrap(),
            Some(receipt.attempt_id)
        );
        let token = owner.active.lock().unwrap()[&receipt.attempt_id].clone();
        assert!(!token.is_cancelled());
        admission
            .block_on(owner.cancel(receipt.attempt_id, "cancel"))
            .unwrap();
        assert!(token.is_cancelled());
        worker.block_on(owner.close());
        assert!(owner.active.lock().unwrap().is_empty());
        assert_eq!(
            ledger.get(receipt.attempt_id).unwrap().state,
            crate::AttemptState::Cancelled
        );
    }

    #[tokio::test]
    async fn stopped_scheduler_does_not_claim_and_abnormal_reservation_destruction_fences() {
        let directory = crate::test_directory();
        let ledger = Ledger::open(&directory.path().join("ledger"), now()).unwrap();
        let policy = Arc::new(PolicyOwner::open(directory.path().join("policy")).unwrap());
        let owner = AutomationService::new(ledger.clone(), policy, None, BTreeMap::new(), None);
        owner.stop.cancel();
        assert_eq!(owner.claim_and_dispatch(now()).await.unwrap(), None);
        let token = CancellationToken::new();
        owner.active.lock().unwrap().insert(1, token.clone());
        drop(AttemptReservation {
            owner: owner.clone(),
            id: 1,
            stop: token.clone(),
            converged: false,
        });
        assert!(token.is_cancelled());
        assert!(owner.active.lock().unwrap().is_empty());
        assert!(!ledger.available());
        owner.close().await;
    }
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn accept_error_retries_and_retirement_interrupts_backoff() {
        for cancel in [false, true] {
            let stop = CancellationToken::new();
            let calls = std::cell::Cell::new(0);
            let mut accepting = Box::pin(accept_with_retry(&stop, || {
                let count = calls.get();
                calls.set(count + 1);
                std::future::ready(if count == 0 {
                    Err(std::io::Error::from(std::io::ErrorKind::ConnectionAborted))
                } else {
                    Ok(7)
                })
            }));
            assert!(futures_util::poll!(&mut accepting).is_pending());
            assert_eq!(calls.get(), 1);
            if cancel {
                stop.cancel();
                assert_eq!(accepting.await, None);
                assert_eq!(calls.get(), 1);
            } else {
                tokio::time::advance(Duration::from_millis(100)).await;
                assert_eq!(accepting.await, Some(7));
                assert_eq!(calls.get(), 2);
            }
        }
    }
    #[tokio::test]
    async fn failed_durable_failure_settlement_fences_admission_and_releases_live_entry() {
        let directory = crate::test_directory();
        let ledger = Ledger::open(&directory.path().join("ledger"), now()).unwrap();
        let receipt = ledger
            .admit(
                "source".into(),
                "failure".into(),
                "b".repeat(64),
                crate::store::tests::rule(),
                crate::store::tests::deployment(1, now()),
                now(),
            )
            .await
            .unwrap();
        let attempt = ledger.claim(now()).unwrap().unwrap();
        crate::store::tests::exhaust_metadata(&ledger);
        let policy = Arc::new(PolicyOwner::open(directory.path().join("policy")).unwrap());
        let owner = AutomationService::new(ledger.clone(), policy, None, BTreeMap::new(), None);
        let stop = owner.stop.child_token();
        owner
            .active
            .lock()
            .unwrap()
            .insert(attempt.id, stop.clone());
        let reservation = AttemptReservation {
            owner: owner.clone(),
            id: attempt.id,
            stop,
            converged: false,
        };
        owner.clone().run(attempt, reservation).await;
        assert!(
            !ledger.available(),
            "failure settlement must not leave a healthy running ledger"
        );
        assert!(matches!(owner.readiness(), Readiness::StorageUnavailable));
        assert!(owner.active.lock().unwrap().is_empty());
        assert!(matches!(
            owner
                .accept("source", "another", "deployment_status", "invalid", b"{}")
                .await,
            Err(AdmissionError::Unavailable)
        ));
        assert!(receipt.attempt_id > 0);
        owner.close().await;
    }
    #[tokio::test(start_paused = true)]
    async fn execution_deadline_stops_at_ten_minutes_and_retires_early_on_cancel() {
        let stop = CancellationToken::new();
        let mut deadline = Box::pin(attempt_deadline(stop.clone()));
        assert!(futures_util::poll!(&mut deadline).is_pending());
        tokio::time::advance(Duration::from_secs(599)).await;
        assert!(futures_util::poll!(&mut deadline).is_pending());
        assert!(!stop.is_cancelled());
        tokio::time::advance(Duration::from_secs(1)).await;
        deadline.await;
        assert!(stop.is_cancelled());
        let stop = CancellationToken::new();
        let mut deadline = Box::pin(attempt_deadline(stop.clone()));
        assert!(futures_util::poll!(&mut deadline).is_pending());
        stop.cancel();
        deadline.await;
    }
    #[tokio::test]
    async fn precheck_failure_retains_its_specific_diagnostic() {
        let directory = crate::test_directory();
        let ledger = Ledger::open(&directory.path().join("ledger"), now()).unwrap();
        let policy = Arc::new(PolicyOwner::open(directory.path().join("policy")).unwrap());
        let rule = crate::store::tests::rule();
        policy
            .update(
                0,
                crate::Policy {
                    rules: BTreeMap::from([("source".into(), vec![rule.clone()])]),
                    ..Default::default()
                },
            )
            .unwrap();
        let receipt = ledger
            .admit(
                "source".into(),
                "precheck".into(),
                "b".repeat(64),
                rule,
                crate::Deployment {
                    repository_id: 7,
                    deployment_id: 1,
                    status_id: 1,
                    deployment_created_ms: now(),
                    status_created_ms: now(),
                    environment: "preview".into(),
                    sha: "a".repeat(40),
                    url: "https://deployment-1.example.invalid/".into(),
                },
                now(),
            )
            .await
            .unwrap();
        let owner = AutomationService::new(ledger.clone(), policy, None, BTreeMap::new(), None);
        owner.claim_and_dispatch(now()).await.unwrap();
        owner.tasks.close();
        owner.tasks.wait().await;
        let attempt = ledger.get(receipt.attempt_id).unwrap();
        assert_eq!(attempt.state, crate::AttemptState::Unavailable);
        assert_eq!(
            attempt.result.unwrap().evidence_error.as_deref(),
            Some("browser unavailable")
        );
        owner.close().await;
    }
    #[tokio::test]
    async fn complete_checker_result_survives_cleanup_failure_before_execution_error() {
        let directory = crate::test_directory();
        let ledger = Ledger::open(&directory.path().join("ledger"), now()).unwrap();
        let instant = now();
        let receipt = ledger
            .admit(
                "source".into(),
                "cleanup".into(),
                "a".repeat(64),
                crate::store::tests::rule(),
                crate::store::tests::deployment(1, instant),
                instant,
            )
            .await
            .unwrap();
        let attempt = ledger.claim(instant).unwrap().unwrap();
        let policy = Arc::new(PolicyOwner::open(directory.path().join("policy")).unwrap());
        let owner = AutomationService::new(ledger.clone(), policy, None, BTreeMap::new(), None);
        let fixed = rsi_browser::CheckResult {
            outcome: rsi_browser::CheckOutcome::AssertionFailed,
            final_url: attempt.deployment.url,
            assertions: vec![rsi_browser::AssertionResult {
                assertion: rsi_browser::Assertion::TextVisible {
                    text: "Ready".into(),
                },
                passed: false,
                detail: "absent".into(),
            }],
            snapshot: "observed page".into(),
            dialogs_dismissed: 0,
            evidence_error: None,
        };
        let png = hex::decode("89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d49444154789c63f8cfc0f01f00050001ff89993d1d0000000049454e44ae426082").unwrap();
        let error = owner
            .settle_check(
                attempt.id,
                Ok((fixed, vec![png])),
                Err("cleanup failed".into()),
            )
            .await
            .unwrap_err();
        ledger.fail_execution(attempt.id, false, &error).unwrap();
        let saved = ledger.get(attempt.id).unwrap();
        assert_eq!(saved.state, crate::AttemptState::Failed);
        assert_eq!(saved.exploration, crate::ExplorationState::Failed);
        let result = saved.result.unwrap();
        assert_eq!(result.outcome, rsi_browser::CheckOutcome::AssertionFailed);
        assert_eq!(result.snapshot, "observed page");
        assert!(
            ledger
                .artifact(receipt.attempt_id, 0)
                .unwrap()
                .starts_with(b"\x89PNG\r\n\x1a\n")
        );
        assert_eq!(
            result.evidence_error.as_deref(),
            Some("preview cleanup: cleanup failed")
        );
        assert!(!ledger.eligible(attempt.id).unwrap());
        owner.close().await;
    }
    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "Keep one complete ownership operation or acceptance scenario together"
    )]
    async fn listener_classifies_rejection_and_signed_non_work_without_execution() {
        let tmp = crate::test_directory();
        let ledger = Ledger::open(&tmp.path().join("ledger"), now()).unwrap();
        let policy = Arc::new(PolicyOwner::open(tmp.path().join("policy")).unwrap());
        let owner = AutomationService::new(
            ledger.clone(),
            policy.clone(),
            None,
            BTreeMap::from([(
                "source".into(),
                rsi_credentials_protocol::SecretValue::new("fixture-secret").unwrap(),
            )]),
            None,
        );
        let address = owner.listen("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let send = |request: String| async move {
            let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
            socket.write_all(request.as_bytes()).await.unwrap();
            let mut output = String::new();
            socket.read_to_string(&mut output).await.unwrap();
            output
        };
        assert!(
            send(
                "POST /github/source HTTP/1.1\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n"
                    .into()
            )
            .await
            .starts_with("HTTP/1.1 400")
        );
        assert!(
            send("POST /github/source HTTP/1.1\r\nContent-Length: +0\r\n\r\n".into())
                .await
                .starts_with("HTTP/1.1 400")
        );
        assert!(
            send("POST /github/source HTTP/1.1\r\nContent-Length: 262145\r\n\r\n".into())
                .await
                .starts_with("HTTP/1.1 413")
        );
        assert!(send("POST /github/source HTTP/1.1\r\nContent-Length: 0\r\nX-GitHub-Delivery: bad-signature\r\n\r\n".into()).await.starts_with("HTTP/1.1 401"));
        let body = "{}";
        let signature = hex::encode(
            ring::hmac::sign(
                &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, b"fixture-secret"),
                body.as_bytes(),
            )
            .as_ref(),
        );
        for (event, status) in [("ping", 204), ("deployment_status", 422)] {
            let reply=send(format!("POST /github/source HTTP/1.1\r\nContent-Length: 2\r\nX-GitHub-Delivery: valid-ignored\r\nX-GitHub-Event: {event}\r\nX-Hub-Signature-256: sha256={signature}\r\n\r\n{body}")).await;
            assert!(reply.starts_with(&format!("HTTP/1.1 {status}")), "{reply}");
        }
        assert_eq!(
            policy.intake_diagnostics()["rejections"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        assert!(ledger.list(0, None, 50).unwrap().0.is_empty());
        let signed = |delivery: &str, body: &str| {
            let signature = hex::encode(
                ring::hmac::sign(
                    &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, b"fixture-secret"),
                    body.as_bytes(),
                )
                .as_ref(),
            );
            format!(
                "POST /github/source HTTP/1.1\r\nContent-Length: {}\r\nX-GitHub-Delivery: {delivery}\r\nX-GitHub-Event: deployment_status\r\nX-Hub-Signature-256: sha256={signature}\r\n\r\n{body}",
                body.len()
            )
        };
        let invalid = r#"{"deployment_status":{"state":"success"}}"#;
        assert!(
            send(signed("invalid-event", invalid))
                .await
                .starts_with("HTTP/1.1 422")
        );
        policy
            .update(
                0,
                crate::Policy {
                    rules: BTreeMap::from([("source".into(), vec![crate::store::tests::rule()])]),
                    ..Default::default()
                },
            )
            .unwrap();
        let timestamp = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        let mut event = json!({"repository":{"id":7},"deployment":{"id":1,"environment":"preview","sha":"a".repeat(40),"created_at":timestamp},"deployment_status":{"id":1,"state":"success","environment":"preview","environment_url":"https://deployment-1.example.invalid/","created_at":timestamp}});
        assert!(
            send(signed("same-delivery", &event.to_string()))
                .await
                .starts_with("HTTP/1.1 202")
        );
        event["deployment"]["sha"] = json!("b".repeat(40));
        assert!(
            send(signed("same-delivery", &event.to_string()))
                .await
                .starts_with("HTTP/1.1 409")
        );
        owner.ledger.drain().await;
        assert!(
            send(signed("unavailable", &event.to_string()))
                .await
                .starts_with("HTTP/1.1 503")
        );
        owner.close().await;
    }
    #[tokio::test]
    async fn saturated_listener_returns_the_bounded_busy_json() {
        let tmp = crate::test_directory();
        let ledger = Ledger::open(&tmp.path().join("ledger"), now()).unwrap();
        let policy = Arc::new(PolicyOwner::open(tmp.path().join("policy")).unwrap());
        let owner = AutomationService::new(ledger, policy.clone(), None, BTreeMap::new(), None);
        let slots = Arc::new(tokio::sync::Semaphore::new(32));
        let held = slots.clone().acquire_many_owned(32).await.unwrap();
        let address = owner
            .listen_with_slots("127.0.0.1:0".parse().unwrap(), slots)
            .await
            .unwrap();
        let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let mut response = String::new();
        tokio::time::timeout(Duration::from_secs(2), socket.read_to_string(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(response.starts_with("HTTP/1.1 503"));
        let body = response.split_once("\r\n\r\n").unwrap().1;
        assert_eq!(
            serde_json::from_str::<Value>(body).unwrap(),
            json!({"error":"admission_unconfirmed","reason":"busy"})
        );
        assert_eq!(
            policy.intake_diagnostics()["rejections"][0]["reason"],
            "busy"
        );
        drop(held);
        owner.close().await;
    }
    #[derive(Debug)]
    struct TestPreview {
        closes: std::sync::atomic::AtomicUsize,
        close_fails: bool,
    }
    #[async_trait]
    impl rsi_browser::PreviewBrowser for TestPreview {
        async fn navigate(&self, _: &str) -> Result<String, String> {
            Ok(String::new())
        }
        async fn observe(&self) -> Result<String, String> {
            Ok(String::new())
        }
        async fn close(&self) -> Result<(), String> {
            self.closes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.close_fails {
                Err("cleanup failed".into())
            } else {
                Ok(())
            }
        }
    }
    #[derive(Debug)]
    struct TestExplorer {
        ledger: Arc<Ledger>,
        fails: bool,
        cancel: bool,
    }
    #[async_trait]
    impl Explorer for TestExplorer {
        async fn explore(
            &self,
            _: &str,
            attempt: &Attempt,
            _: Arc<dyn rsi_browser::PreviewBrowser>,
            stop: CancellationToken,
        ) -> Result<(String, String), String> {
            let id = attempt.id;
            self.ledger
                .run(move |ledger| {
                    ledger.record_exploration(id, "preview-session".into(), None)?;
                    ledger.exploration_state(id, crate::ExplorationState::Running, None)
                })
                .await
                .unwrap();
            if self.cancel {
                stop.cancel();
            }
            if self.fails || self.cancel {
                Err("explorer failed".into())
            } else {
                Ok(("preview-session".into(), "report".into()))
            }
        }
    }
    #[tokio::test]
    async fn exploration_errors_and_cleanup_failures_settle_before_returning() {
        for (fails, close_fails, cancel) in [
            (true, false, false),
            (false, true, false),
            (true, true, false),
            (true, false, true),
        ] {
            let tmp = crate::test_directory();
            let ledger = Ledger::open(&tmp.path().join("ledger"), now()).unwrap();
            let policy = Arc::new(PolicyOwner::open(tmp.path().join("policy")).unwrap());
            let rule = crate::store::tests::rule();
            policy
                .update(
                    0,
                    crate::Policy {
                        rules: BTreeMap::from([("source".into(), vec![rule.clone()])]),
                        ..Default::default()
                    },
                )
                .unwrap();
            let receipt = ledger
                .admit(
                    "source".into(),
                    "failure".into(),
                    "b".repeat(64),
                    rule,
                    crate::store::tests::deployment(1, now()),
                    now(),
                )
                .await
                .unwrap();
            ledger.claim(now()).unwrap().unwrap();
            let verdict = crate::store::tests::failed();
            ledger
                .settle(receipt.attempt_id, verdict.clone(), vec![], now())
                .unwrap();
            let explorer = Arc::new(TestExplorer {
                ledger: ledger.clone(),
                fails,
                cancel,
            });
            let owner = AutomationService::new(
                ledger.clone(),
                policy,
                None,
                BTreeMap::new(),
                Some(explorer),
            );
            let browser = Arc::new(TestPreview {
                closes: 0.into(),
                close_fails,
            });
            let error = owner
                .explore_failed(
                    receipt.attempt_id,
                    browser.clone(),
                    CancellationToken::new(),
                )
                .await
                .unwrap_err();
            assert_eq!(browser.closes.load(std::sync::atomic::Ordering::SeqCst), 1);
            let a = ledger.get(receipt.attempt_id).unwrap();
            assert_eq!(a.state, crate::AttemptState::Failed);
            assert_eq!(
                serde_json::to_value(a.result.unwrap()).unwrap(),
                serde_json::to_value(verdict).unwrap()
            );
            assert_eq!(
                a.exploration,
                if cancel {
                    crate::ExplorationState::Cancelled
                } else {
                    crate::ExplorationState::Failed
                }
            );
            assert!(a.report.unwrap().contains(if close_fails {
                "cleanup failed"
            } else {
                "explorer failed"
            }));
            if fails && close_fails {
                assert!(error.contains("explorer failed") && error.contains("cleanup failed"));
            }
            owner.close().await;
        }
    }
    #[tokio::test]
    async fn header_chunks_preserve_coalesced_body_and_split_terminators() {
        for split in [false, true] {
            let (mut input, mut output) = tokio::io::duplex(128);
            let header = b"POST /github/source HTTP/1.1\r\nContent-Length: 4\r\n\r\n";
            if split {
                input.write_all(&header[..header.len() - 1]).await.unwrap();
                let mut parsed = Box::pin(read_request(&mut output));
                assert!(futures_util::poll!(&mut parsed).is_pending());
                input.write_all(b"\nbody").await.unwrap();
                assert_eq!(parsed.await.unwrap().4, b"body");
            } else {
                let mut request = header.to_vec();
                request.extend_from_slice(b"body");
                input.write_all(&request).await.unwrap();
                assert_eq!(read_request(&mut output).await.unwrap().4, b"body");
            }
        }
        let (mut input, mut output) = tokio::io::duplex(32768);
        input.write_all(&vec![b'x'; 16384]).await.unwrap();
        assert_eq!(read_request(&mut output).await, Err(true));
        let (mut input, mut output) = tokio::io::duplex(128);
        input
            .write_all(b"POST /github/source HTTP/1.1\r\nContent-Length: 4\r\n\r\nshort")
            .await
            .unwrap();
        // The declared body, not any pipelined suffix, belongs to this closed response.
        assert_eq!(read_request(&mut output).await.unwrap().4, b"shor");
        let (mut input, mut output) = tokio::io::duplex(128);
        input
            .write_all(b"POST /github/source HTTP/1.1\r\nContent-Length: 4\r\n\r\nabc")
            .await
            .unwrap();
        input.shutdown().await.unwrap();
        assert_eq!(read_request(&mut output).await, Err(false));
    }
}
