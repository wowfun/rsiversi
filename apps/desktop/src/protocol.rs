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
    export_control: Arc<Semaphore>,
}
impl Default for Admission {
    fn default() -> Self {
        Self {
            ordinary: Arc::new(Semaphore::new(8)),
            reads: Arc::new(Semaphore::new(32)),
            writes: Arc::new(Semaphore::new(8)),
            frames: Arc::new(Semaphore::new(1)),
            lifecycle: Arc::new(Semaphore::new(1)),
            export_control: Arc::new(Semaphore::new(8)),
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
    ExportControl,
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
        "/_call/export_cancel" => Lane::ExportControl,
        "/_call/directory_input"
            if body.len() <= 128 * 1024
                && serde_json::from_slice::<serde_json::Value>(body)
                    .ok()
                    .is_some_and(|value| value["action"] == "cancel") =>
        {
            Lane::ExportControl
        }
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
    pub(super) async fn join_frame(
        &self,
    ) -> Result<OwnedSemaphorePermit, tokio::sync::AcquireError> {
        self.frames.clone().acquire_owned().await
    }
    fn acquire(&self, lane: Lane) -> Result<OwnedSemaphorePermit, NativeError> {
        let slots = match lane {
            Lane::Ordinary => &self.ordinary,
            Lane::Read => &self.reads,
            Lane::Write => &self.writes,
            Lane::Frame => &self.frames,
            Lane::Lifecycle => &self.lifecycle,
            Lane::ExportControl => &self.export_control,
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
            &self.export_control,
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
    if label != "main"
        || request.uri().scheme_str() != Some("rsi")
        || request
            .uri()
            .authority()
            .map(tauri::http::uri::Authority::as_str)
            != Some("localhost")
    {
        return Err(NativeError::Invalid("Invalid native document".into()));
    }
    let private = request.uri().path().starts_with("/_");
    let mut origins = request.headers().get_all("origin").iter();
    let origin = origins.next();
    if origin.is_some_and(|value| value.to_str().ok() != Some(crate::document::ORIGIN))
        || (private && origin.is_none() && !admits_document_referer(request))
        || origins.next().is_some()
    {
        return Err(NativeError::Invalid("Untrusted native origin".into()));
    }
    if private && request.method() != tauri::http::Method::POST {
        return Err(NativeError::Invalid("Native inputs require POST".into()));
    }
    Ok(())
}

fn admits_document_referer(request: &tauri::http::Request<Vec<u8>>) -> bool {
    #[cfg(target_os = "linux")]
    {
        let mut referers = request.headers().get_all("referer").iter();
        referers
            .next()
            .is_some_and(|value| value.as_bytes() == b"rsi://localhost/")
            && referers.next().is_none()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = request;
        false
    }
}

pub(super) fn handle(
    owner: &Owner,
    label: &str,
    request: tauri::http::Request<Vec<u8>>,
    responder: tauri::UriSchemeResponder,
) {
    let fallback = error_policy(request.uri().path());
    let respond = move |responder, result| respond_with_policy(responder, result, &fallback);
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
        match bridge.asset(&path) {
            Ok((bytes, mime, policy)) => respond_with_policy(responder, Ok((bytes, mime)), &policy),
            Err(error) => respond(responder, Err(error)),
        }
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
    let response = tasks.track_future(respond_admitted(
        permit,
        execute(bridge, request, path),
        move |result| respond(responder, result),
    ));
    // Retirement must observe registration before this lock is released; the
    // tracked future already owns its lifetime while scheduling happens outside.
    drop(guard);
    tauri::async_runtime::spawn(response);
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

fn error_policy(path: &str) -> rsi_api_http::DocumentPolicy {
    match path {
        "/preview-local.html" => rsi_api_http::DocumentPolicy::SandboxLocal,
        "/preview-online.html" => rsi_api_http::DocumentPolicy::SandboxHttps,
        _ => rsi_api_http::DocumentPolicy::default(),
    }
}
fn respond_with_policy(
    responder: tauri::UriSchemeResponder,
    result: Result<(Vec<u8>, &'static str), NativeError>,
    policy: &rsi_api_http::DocumentPolicy,
) {
    responder.respond(response(result, policy));
}
fn response(
    result: Result<(Vec<u8>, &'static str), NativeError>,
    policy: &rsi_api_http::DocumentPolicy,
) -> tauri::http::Response<Vec<u8>> {
    let csp = policy
        .content_security_policy(crate::document::ORIGIN)
        .expect("closed product asset policy");
    let (status, bytes, mime) = match result {
        Ok((bytes, mime)) => (200, bytes, mime),
        Err(error) => (409, error.encode(), "application/json"),
    };
    tauri::http::Response::builder()
        .status(status)
        .header("Content-Type", mime)
        .header("Cache-Control", "no-store")
        .header("Content-Security-Policy", csp)
        .header("Referrer-Policy", policy.referrer_policy())
        .header("X-Content-Type-Options", "nosniff")
        .body(bytes)
        .expect("constant native headers")
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

#[cfg(all(test, windows))]
#[path = "protocol_probe.rs"]
mod live_probe;

#[cfg(test)]
mod tests {
    use super::{Admission, NativeError, classify, respond_admitted, run_operation};
    use std::sync::Arc;

    #[test]
    fn native_requests_use_normalized_uri_and_actual_document_origin() {
        let request = |uri: &str, origin: &str| {
            tauri::http::Request::builder()
                .method(tauri::http::Method::POST)
                .uri(uri)
                .header("origin", origin)
                .body(Vec::new())
                .unwrap()
        };
        let admitted = request("rsi://localhost/_connect", crate::document::ORIGIN);
        assert!(super::validate_request("main", &admitted).is_ok());
        assert!(super::validate_request("preview", &admitted).is_err());
        for uri in [
            "rsi://localhost:8888/_connect",
            "rsi://user@localhost/_connect",
            "http://rsi.localhost/_connect",
            "https://localhost/_connect",
            "rsi://localhost.evil/_connect",
        ] {
            assert!(
                super::validate_request("main", &request(uri, crate::document::ORIGIN)).is_err()
            );
        }
        for origin in [
            "null",
            "https://rsi.localhost",
            "http://localhost",
            "http://rsi.localhost.evil",
        ] {
            assert!(
                super::validate_request("main", &request("rsi://localhost/_connect", origin))
                    .is_err()
            );
        }
    }

    #[test]
    fn private_requests_require_post_and_an_exact_well_formed_origin() {
        for path in ["/_frame", "/_call/connect", "/_ack"] {
            let uri = format!("rsi://localhost{path}");
            let request = tauri::http::Request::builder()
                .method("POST")
                .uri(&uri)
                .body(Vec::new())
                .unwrap();
            assert!(super::validate_request("main", &request).is_err());
            let request = tauri::http::Request::builder()
                .method("POST")
                .uri(&uri)
                .header(
                    "origin",
                    tauri::http::HeaderValue::from_bytes(b"\xff").unwrap(),
                )
                .body(Vec::new())
                .unwrap();
            assert!(super::validate_request("main", &request).is_err());
            let request = tauri::http::Request::builder()
                .method("GET")
                .uri(&uri)
                .header("origin", crate::document::ORIGIN)
                .body(Vec::new())
                .unwrap();
            assert!(super::validate_request("main", &request).is_err());
        }
        let asset = tauri::http::Request::builder()
            .uri("rsi://localhost/")
            .body(Vec::new())
            .unwrap();
        assert!(super::validate_request("main", &asset).is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn webkit_same_origin_posts_require_an_exact_unique_root_referer() {
        let request = |referer: &str| {
            tauri::http::Request::builder()
                .method("POST")
                .uri("rsi://localhost/_call/connect")
                .header("referer", referer)
                .body(Vec::new())
                .unwrap()
        };
        let root = request("rsi://localhost/");
        assert!(super::validate_request("main", &root).is_ok());
        assert!(super::validate_request("preview", &root).is_err());
        for referer in [
            "null",
            "rsi://localhost/preview-local.html",
            "rsi://localhost/preview-online.html",
            "rsi://localhost.evil/",
            "rsi://user@localhost/",
            "rsi://localhost:8888/",
            "https://localhost/",
        ] {
            assert!(super::validate_request("main", &request(referer)).is_err());
        }
        let mut duplicate = request("rsi://localhost/");
        duplicate.headers_mut().append(
            "referer",
            tauri::http::HeaderValue::from_static("rsi://localhost/"),
        );
        assert!(super::validate_request("main", &duplicate).is_err());
        let mut foreign_origin = request("rsi://localhost/");
        foreign_origin.headers_mut().insert(
            "origin",
            tauri::http::HeaderValue::from_static("https://example.invalid"),
        );
        assert!(super::validate_request("main", &foreign_origin).is_err());
    }

    #[test]
    fn document_response_csp_uses_the_platform_origin() {
        let response = super::response(
            Ok((Vec::new(), "text/html")),
            &rsi_api_http::DocumentPolicy::SandboxLocal,
        );
        let csp = response.headers()["Content-Security-Policy"]
            .to_str()
            .unwrap();
        assert!(csp.contains(crate::document::ORIGIN));
        #[cfg(windows)]
        assert!(!csp.contains("rsi://localhost"));
        #[cfg(not(windows))]
        assert!(!csp.contains("http://rsi.localhost"));
    }

    #[test]
    fn duplicate_origin_headers_are_rejected_in_any_order_even_when_equal() {
        for path in ["/", "/_frame"] {
            for origins in [
                [crate::document::ORIGIN, "null"],
                ["null", crate::document::ORIGIN],
                [crate::document::ORIGIN, crate::document::ORIGIN],
            ] {
                let request = tauri::http::Request::builder()
                    .method("POST")
                    .uri(format!("rsi://localhost{path}"))
                    .header("origin", origins[0])
                    .header("origin", origins[1])
                    .body(Vec::new())
                    .unwrap();
                assert!(super::validate_request("main", &request).is_err());
            }
        }
    }

    #[tokio::test]
    async fn reconnect_joins_actual_frame_work_before_new_observer_admission() {
        let admission = Arc::new(Admission::default());
        let old = admission.acquire(super::Lane::Frame).unwrap();
        let reconnect = admission.clone();
        let waiting = tokio::spawn(async move { reconnect.join_frame().await.unwrap() });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        drop(old);
        let reconnect = waiting.await.unwrap();
        assert!(matches!(
            admission.acquire(super::Lane::Frame),
            Err(NativeError::Busy)
        ));
        drop(reconnect);
        assert!(admission.acquire(super::Lane::Frame).is_ok());
    }

    #[test]
    fn rejected_preview_requests_keep_opaque_sandbox_headers() {
        for path in ["/preview-local.html", "/preview-online.html"] {
            let response = super::response(Err(NativeError::Closed), &super::error_policy(path));
            assert_eq!(response.status(), 409);
            assert_eq!(response.headers()["Content-Type"], "application/json");
            assert_eq!(response.headers()["X-Content-Type-Options"], "nosniff");
            assert_eq!(response.headers()["Referrer-Policy"], "no-referrer");
            assert!(
                response.headers()["Content-Security-Policy"]
                    .to_str()
                    .unwrap()
                    .starts_with("sandbox allow-scripts;")
            );
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(response.body()).unwrap()["code"],
                "closed"
            );
        }
    }
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
            ("/_call/export_cancel", "{}", 8),
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
        assert_eq!(permits.len(), 58);
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
