use super::{Bridge, Ordering, Owner};
use std::{future::Future, sync::Arc};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};

#[derive(Debug)]
pub(super) enum NativeError {
    Busy,
    Closed,
    Invalid(String),
    Failed(String),
}
impl NativeError {
    fn encode(&self) -> Vec<u8> {
        let (code, message, not_admitted, retryable) = match self {
            Self::Busy => ("busy", "Native input is busy", true, true),
            Self::Closed => ("closed", "Native application is closed", true, false),
            Self::Invalid(message) => ("invalid", message.as_str(), true, false),
            Self::Failed(message) => ("failed", message.as_str(), false, false),
        };
        serde_json::to_vec(&serde_json::json!({"code":code,"message":message,"notAdmitted":not_admitted,"retryable":retryable}))
            .expect("native failure fields serialize")
    }
}
impl From<TryAcquireError> for NativeError {
    fn from(error: TryAcquireError) -> Self {
        match error {
            TryAcquireError::NoPermits => Self::Busy,
            TryAcquireError::Closed => Self::Closed,
        }
    }
}
#[derive(Debug)]
pub(super) struct Admission {
    ordinary: Arc<Semaphore>,
    reads: Arc<Semaphore>,
    writes: Arc<Semaphore>,
    frames: Arc<Semaphore>,
    lifecycle: Arc<Semaphore>,
}
impl Default for Admission {
    fn default() -> Self {
        Self {
            ordinary: Arc::new(Semaphore::new(8)),
            reads: Arc::new(Semaphore::new(32)),
            writes: Arc::new(Semaphore::new(8)),
            frames: Arc::new(Semaphore::new(1)),
            lifecycle: Arc::new(Semaphore::new(1)),
        }
    }
}
#[derive(Clone, Copy)]
enum Lane {
    Ordinary,
    Read,
    Write,
    Frame,
    Lifecycle,
}

fn classify(path: &str, body: &[u8]) -> Lane {
    #[derive(serde::Deserialize)]
    struct Intent<'a> {
        #[serde(borrow)]
        request: Kind<'a>,
    }
    #[derive(serde::Deserialize)]
    struct Kind<'a> {
        #[serde(rename = "type", borrow)]
        kind: std::borrow::Cow<'a, str>,
    }
    match path {
        "/_frame" => Lane::Frame,
        "/_call/connect" | "/_disconnect" => Lane::Lifecycle,
        "/_call/terminal" if body.len() <= 512 * 1024 => {
            match serde_json::from_slice::<Intent<'_>>(body).map(|input| input.request.kind) {
                Ok(kind) if kind == "read" => Lane::Read,
                Ok(kind) if kind == "write" => Lane::Write,
                _ => Lane::Ordinary,
            }
        }
        _ => Lane::Ordinary,
    }
}

impl Admission {
    fn acquire(&self, lane: Lane) -> Result<OwnedSemaphorePermit, NativeError> {
        let slots = match lane {
            Lane::Ordinary => &self.ordinary,
            Lane::Read => &self.reads,
            Lane::Write => &self.writes,
            Lane::Frame => &self.frames,
            Lane::Lifecycle => &self.lifecycle,
        };
        slots.clone().try_acquire_owned().map_err(Into::into)
    }
    pub(super) fn close(&self) {
        for slots in [
            &self.ordinary,
            &self.reads,
            &self.writes,
            &self.frames,
            &self.lifecycle,
        ] {
            slots.close();
        }
    }
}

fn validate_request(
    label: &str,
    request: &tauri::http::Request<Vec<u8>>,
) -> Result<(), NativeError> {
    if request
        .uri()
        .path_and_query()
        .is_some_and(|value| value.as_str().len() > 2048)
        || request.body().len() > 17 * 1024 * 1024
    {
        return Err(NativeError::Invalid(
            "Native request exceeds its limit".into(),
        ));
    }
    if label != "main" || request.uri().host() != Some("localhost") {
        return Err(NativeError::Invalid("Invalid native document".into()));
    }
    let origin = request
        .headers()
        .get("origin")
        .and_then(|value| value.to_str().ok());
    if origin.is_some_and(|value| value != "rsi://localhost") {
        return Err(NativeError::Invalid("Untrusted native origin".into()));
    }
    Ok(())
}

pub(super) fn handle(
    owner: &Owner,
    label: &str,
    request: tauri::http::Request<Vec<u8>>,
    responder: tauri::UriSchemeResponder,
) {
    if let Err(error) = validate_request(label, &request) {
        respond(responder, Err(error));
        return;
    }
    let path = request.uri().path().to_owned();
    let lane = classify(&path, request.body());
    // Retirement takes this same lock before closing/waiting on the tracker.
    // Keep registration inside that fence; classification needs no owner lock.
    let guard = owner.lock().expect("desktop owner poisoned");
    let Some(bridge) = guard.as_ref().cloned() else {
        drop(guard);
        respond(responder, Err(NativeError::Closed));
        return;
    };
    if path.starts_with("/_") && path != "/_frame" && request.method() != tauri::http::Method::POST
    {
        drop(guard);
        respond(
            responder,
            Err(NativeError::Invalid("Native inputs require POST".into())),
        );
        return;
    }
    if path == "/_ack" {
        drop(guard);
        respond(
            responder,
            bridge
                .ack(request.body())
                .map(|()| (Vec::new(), "application/json"))
                .map_err(NativeError::Failed),
        );
        return;
    }
    if path == "/_close_cancel" {
        drop(guard);
        bridge.cancel_document_close();
        respond(responder, Ok((Vec::new(), "text/plain")));
        return;
    }
    if path == "/_disconnect" {
        let permit = match bridge.admission.acquire(lane) {
            Ok(permit) => permit,
            Err(error) => {
                drop(guard);
                respond(responder, Err(error));
                return;
            }
        };
        bridge.lifetime.request_stop();
        tauri::async_runtime::spawn(respond_admitted(
            permit,
            async move {
                bridge.lifetime.stopped().await;
                Ok((serde_json::json!({"active_requests":bridge.tasks.len(),"pending_timers":0,"active_alarms":0}).to_string().into_bytes(), "application/json"))
            },
            move |result| respond(responder, result),
        ));
        return;
    }
    if path == "/_failed" {
        drop(guard);
        bridge.failed.store(true, Ordering::Release);
        bridge.lifetime.request_stop();
        respond(responder, Ok((Vec::new(), "text/plain")));
        return;
    }
    if !path.starts_with("/_") && request.method() == tauri::http::Method::GET {
        drop(guard);
        respond(responder, bridge.asset(&path));
        return;
    }
    let permit = match bridge.admission.acquire(lane) {
        Ok(permit) => permit,
        Err(error) => {
            drop(guard);
            respond(responder, Err(error));
            return;
        }
    };
    let tasks = bridge.tasks.clone();
    tauri::async_runtime::spawn(tasks.track_future(respond_admitted(
        permit,
        execute(bridge, request, path),
        move |result| respond(responder, result),
    )));
}

async fn execute(
    bridge: Arc<Bridge>,
    request: tauri::http::Request<Vec<u8>>,
    path: String,
) -> Result<(Vec<u8>, &'static str), NativeError> {
    run_operation(&bridge.stop, async {
        if path == "/_frame" {
            let base = request
                .uri()
                .query()
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            bridge
                .frame(base)
                .await
                .map(|bytes| (bytes, "application/json"))
        } else if let Some(method) = path.strip_prefix("/_call/") {
            bridge
                .call(method, request.body())
                .await
                .map(|bytes| (bytes, "application/octet-stream"))
        } else {
            Err("Unknown native route".into())
        }
    })
    .await
}

async fn run_operation<T>(
    stop: &tokio_util::sync::CancellationToken,
    operation: impl Future<Output = Result<T, String>>,
) -> Result<T, NativeError> {
    let mut started = false;
    tokio::select! { biased;
        () = stop.cancelled() => Err(if started {
            NativeError::Failed("Native application is closed".into())
        } else {
            NativeError::Closed
        }),
        result = async { started = true; operation.await } => result.map_err(NativeError::Failed),
    }
}

fn respond(
    responder: tauri::UriSchemeResponder,
    result: Result<(Vec<u8>, &'static str), NativeError>,
) {
    let (status, bytes, mime) = match result {
        Ok((bytes, mime)) => (200, bytes, mime),
        Err(error) => (409, error.encode(), "application/json"),
    };
    responder.respond(tauri::http::Response::builder().status(status)
        .header("Content-Type", mime).header("Cache-Control", "no-store")
        .header("Content-Security-Policy", "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; connect-src 'self' ipc: http://ipc.localhost; img-src 'self' blob:; font-src 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; form-action 'none'")
        .body(bytes).expect("constant native headers"));
}

async fn respond_admitted<T>(
    permit: OwnedSemaphorePermit,
    operation: impl Future<Output = T>,
    handoff: impl FnOnce(T),
) {
    let result = operation.await;
    drop(permit);
    handoff(result);
}

#[cfg(test)]
mod tests {
    use super::{Admission, NativeError, classify, respond_admitted, run_operation};
    use std::sync::Arc;
    use tokio::sync::{Semaphore, oneshot};

    #[test]
    fn mixed_native_lanes_reject_only_their_own_excess_work() {
        let admission = Admission::default();
        let mut permits = Vec::new();
        for (path, body, capacity) in [
            ("/_call/command", "{}", 8),
            ("/_call/terminal", r#"{"request":{"type":"read"}}"#, 32),
            ("/_call/terminal", r#"{"request":{"type":"write"}}"#, 8),
            ("/_frame", "", 1),
            ("/_call/connect", "", 1),
        ] {
            for _ in 0..capacity {
                permits.push(admission.acquire(classify(path, body.as_bytes())).unwrap());
            }
            let error = admission
                .acquire(classify(path, body.as_bytes()))
                .unwrap_err();
            let wire: serde_json::Value = serde_json::from_slice(&error.encode()).unwrap();
            assert_eq!(wire["code"], "busy");
            assert_eq!(wire["notAdmitted"], true);
            assert_eq!(wire["retryable"], true);
        }
        assert_eq!(permits.len(), 50);
        assert!(matches!(
            admission.acquire(classify("/_disconnect", b"")),
            Err(NativeError::Busy)
        ));
        drop(permits);
        admission.close();
        assert!(matches!(
            admission.acquire(classify(
                "/_call/terminal",
                br#"{"request":{"type":"write"}}"#
            )),
            Err(NativeError::Closed)
        ));
    }

    #[test]
    fn escaped_valid_io_kinds_share_the_same_native_lanes() {
        let admission = Admission::default();
        let _ordinary = (0..8)
            .map(|_| {
                admission
                    .acquire(classify("/_call/command", b"{}"))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        for kind in [r"\u0072ead", r"\u0077rite"] {
            let body = format!(r#"{{"request":{{"type":"{kind}"}}}}"#);
            assert!(
                admission
                    .acquire(classify("/_call/terminal", body.as_bytes()))
                    .is_ok()
            );
        }
    }

    #[test]
    fn malformed_and_non_io_terminal_intents_use_ordinary_admission() {
        let admission = Admission::default();
        let _permits = (0..8)
            .map(|_| {
                admission
                    .acquire(classify("/_call/command", b"{}"))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        for body in [
            "malformed",
            "{}",
            r#"{"request":{"type":"resize"}}"#,
            r#"{"request":{"type":"unknown"}}"#,
        ] {
            assert!(matches!(
                admission.acquire(classify("/_call/terminal", body.as_bytes())),
                Err(NativeError::Busy)
            ));
        }
        let oversized = format!(
            r#"{{"request":{{"type":"read"}},"padding":"{}"}}"#,
            "x".repeat(512 * 1024)
        );
        assert!(matches!(
            admission.acquire(classify("/_call/terminal", oversized.as_bytes())),
            Err(NativeError::Busy)
        ));
        assert!(
            admission
                .acquire(classify(
                    "/_call/terminal",
                    br#"{"request":{"type":"read"}}"#
                ))
                .is_ok()
        );
    }

    #[test]
    fn only_pre_dispatch_busy_authorizes_retry() {
        for (error, admitted) in [
            (NativeError::Closed, false),
            (NativeError::Invalid("invalid".into()), false),
            (NativeError::Failed("busy".into()), true),
        ] {
            let wire: serde_json::Value = serde_json::from_slice(&error.encode()).unwrap();
            assert_eq!(wire["notAdmitted"], !admitted);
            assert_eq!(wire["retryable"], false);
        }
    }

    #[tokio::test]
    async fn stop_before_the_first_operation_poll_is_a_permanent_rejection() {
        let stop = tokio_util::sync::CancellationToken::new();
        stop.cancel();
        let result = run_operation::<()>(&stop, async {
            panic!("a closed bridge must not dispatch the operation");
        })
        .await
        .unwrap_err();
        let wire: serde_json::Value = serde_json::from_slice(&result.encode()).unwrap();
        assert_eq!(wire["code"], "closed");
        assert_eq!(wire["notAdmitted"], true);
        assert_eq!(wire["retryable"], false);
    }

    #[tokio::test]
    async fn stop_after_the_first_operation_poll_cannot_authorize_replay() {
        let stop = tokio_util::sync::CancellationToken::new();
        let (entered, mut entry) = oneshot::channel();
        let mut request = Box::pin(run_operation(&stop, async {
            entered.send(()).unwrap();
            std::future::pending::<Result<(), String>>().await
        }));
        assert!(futures_util::poll!(request.as_mut()).is_pending());
        entry.try_recv().unwrap();
        stop.cancel();
        let wire: serde_json::Value =
            serde_json::from_slice(&request.await.unwrap_err().encode()).unwrap();
        assert_eq!(wire["code"], "failed");
        assert_eq!(wire["notAdmitted"], false);
        assert_eq!(wire["retryable"], false);
    }

    #[tokio::test]
    async fn completed_request_releases_admission_before_response_handoff() {
        for outcome in [Ok(()), Err("request failed")] {
            let slots = Arc::new(Semaphore::new(1));
            let permit = slots.clone().try_acquire_owned().unwrap();
            let (finish, operation) = oneshot::channel();
            let next = slots.clone();
            let task = tokio::spawn(respond_admitted(
                permit,
                async move { operation.await.unwrap() },
                move |result| {
                    assert_eq!(result, outcome);
                    // WebKit can issue its next request while the previous
                    // response callback is still running on the native thread.
                    let _next_request = next
                        .try_acquire_owned()
                        .expect("response already delivered");
                },
            ));
            assert!(slots.try_acquire().is_err(), "pending work owns admission");
            finish.send(outcome).unwrap();
            task.await.unwrap();
            assert_eq!(slots.available_permits(), 1);
        }
    }
}
