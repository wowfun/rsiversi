use super::{Ordering, Owner};

pub(super) fn handle(
    owner: &Owner,
    label: &str,
    request: tauri::http::Request<Vec<u8>>,
    responder: tauri::UriSchemeResponder,
) {
    if request
        .uri()
        .path_and_query()
        .is_some_and(|value| value.as_str().len() > 2048)
        || request.body().len() > 17 * 1024 * 1024
    {
        respond(responder, Err("Native request exceeds its limit".into()));
        return;
    }
    if label != "main" || request.uri().host() != Some("localhost") {
        respond(responder, Err("Invalid native document".into()));
        return;
    }
    let origin = request
        .headers()
        .get("origin")
        .and_then(|value| value.to_str().ok());
    if origin.is_some_and(|value| value != "rsi://localhost") {
        respond(responder, Err("Untrusted native origin".into()));
        return;
    }
    let guard = owner.lock().expect("desktop owner poisoned");
    let Some(bridge) = guard.as_ref().cloned() else {
        respond(responder, Err("Native application is closed".into()));
        return;
    };
    let path = request.uri().path().to_owned();
    if path.starts_with("/_") && path != "/_frame" && request.method() != tauri::http::Method::POST
    {
        respond(responder, Err("Native inputs require POST".into()));
        return;
    }
    if path == "/_ack" {
        respond(
            responder,
            bridge
                .ack(request.body())
                .map(|()| (Vec::new(), "application/json")),
        );
        return;
    }
    if path == "/_close_cancel" {
        bridge.cancel_document_close();
        respond(responder, Ok((Vec::new(), "text/plain")));
        return;
    }
    if path == "/_disconnect" {
        let Ok(permit) = bridge.control_slot.clone().try_acquire_owned() else {
            respond(
                responder,
                Err("Native lifecycle operation is already pending".into()),
            );
            return;
        };
        bridge.lifetime.request_stop();
        tauri::async_runtime::spawn(async move {
            let _permit = permit;
            bridge.lifetime.stopped().await;
            respond(responder, Ok((serde_json::json!({"active_requests":bridge.tasks.len(),"pending_timers":0,"active_alarms":0}).to_string().into_bytes(), "application/json")));
        });
        return;
    }
    if path == "/_failed" {
        bridge.failed.store(true, Ordering::Release);
        bridge.lifetime.request_stop();
        respond(responder, Ok((Vec::new(), "text/plain")));
        return;
    }
    let permit = {
        let slots = if path == "/_frame" {
            bridge.frame_slot.clone()
        } else {
            bridge.slots.clone()
        };
        if let Ok(permit) = slots.try_acquire_owned() {
            permit
        } else {
            respond(responder, Err("Native input is busy or closed".into()));
            return;
        }
    };
    let tasks = bridge.tasks.clone();
    tauri::async_runtime::spawn(tasks.track_future(async move {
                let _permit = permit;
                let result = tokio::select! { biased;
                    () = bridge.stop.cancelled() => Err("Native application is closed".into()),
                    result = async {
                        if path == "/_frame" {
                            let base = request.uri().query().filter(|value| !value.is_empty()).map(str::to_owned);
                            bridge.frame(base).await.map(|bytes| (bytes, "application/json"))
                        } else if let Some(method) = path.strip_prefix("/_call/") {
                            bridge.call(method, request.body()).await.map(|bytes| (bytes, "application/octet-stream"))
                        } else if request.method() == tauri::http::Method::GET { bridge.asset(&path) }
                        else { Err("Unknown native route".into()) }
                    } => result,
                };
                respond(responder, result);
            }));
}
fn respond(responder: tauri::UriSchemeResponder, result: Result<(Vec<u8>, &'static str), String>) {
    let (status, bytes, mime) = match result {
        Ok((bytes, mime)) => (200, bytes, mime),
        Err(error) => (409, error.into_bytes(), "text/plain; charset=utf-8"),
    };
    responder.respond(tauri::http::Response::builder().status(status)
        .header("Content-Type", mime).header("Cache-Control", "no-store")
        .header("Content-Security-Policy", "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; connect-src 'self' ipc: http://ipc.localhost; img-src 'self' blob:; font-src 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; form-action 'none'")
        .body(bytes).expect("constant native headers"));
}
