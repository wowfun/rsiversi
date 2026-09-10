use crate::{TOKEN, spec};
use futures_util::{FutureExt, StreamExt};
use rsi_api_browser_client::{
    BrowserClient, BrowserClientConfig, BrowserClientFactory, BrowserResourceSnapshot,
};
use rsi_api_protocol::*;
use rsi_credentials_protocol::SecretValue;
use rsi_meta::{Execution, ResolvedFactory, Runtime, RuntimeLimits, UpdateMode};
use std::{sync::Arc, time::Duration};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn diagnostic(message: &str);
    #[wasm_bindgen(js_namespace = globalThis)]
    fn bootstrap_state() -> u32;
    #[wasm_bindgen(js_namespace = globalThis)]
    fn release_bootstrap();
}

#[wasm_bindgen]
pub async fn run_bootstrap_cancel_probe(_secure: bool) -> std::result::Result<String, JsValue> {
    std::panic::set_hook(Box::new(|info| diagnostic(&info.to_string())));
    let execution = Execution::browser().unwrap();
    let runtime = Runtime::with_execution(RuntimeLimits::default(), execution.clone()).unwrap();
    let root = runtime.root();
    let starting = execution.spawn(async move {
        root.apply(ResolvedFactory::linked("client", "test", UpdateMode::Replayable, Arc::new(BrowserClientFactory)),
            serde_json::json!({"endpoint_id":"02".repeat(16),"allow_loopback_http":true})).await
    });
    wait_bootstrap(&execution, 1).await;
    let mut stopping = Box::pin(execution.spawn(async move { runtime.shutdown().await }));
    wait_bootstrap(&execution, 2).await;
    assert!(futures_util::poll!(stopping.as_mut()).is_pending(), "clean shutdown was reported while Fetch still owned an unsettled platform promise");
    release_bootstrap();
    assert!(stopping.await.unwrap().is_clean());
    let _ = starting.await.unwrap();
    assert_eq!(bootstrap_state(), 3);
    let resources = rsi_meta_execution::browser_resource_snapshot();
    assert_eq!(resources.pending_timers, 0);
    assert_eq!(resources.active_alarms, 0);
    Ok(serde_json::json!({"cancelled_bootstrap":"passed","pending_timers":0,"active_alarms":0}).to_string())
}
async fn wait_bootstrap(execution: &Execution, state: u32) {
    execution.deadline_after(Duration::from_secs(5)).timeout(async {
        while bootstrap_state() != state { execution.sleep(Duration::from_millis(1)).await; }
    }).await.unwrap();
}

#[wasm_bindgen]
pub async fn run_pool_probe(secure: bool) -> std::result::Result<String, JsValue> {
    std::panic::set_hook(Box::new(|info| diagnostic(&info.to_string())));
    let execution = Execution::browser().unwrap();
    let config = BrowserClientConfig {
        endpoint_id: EndpointId::from_bytes([2; 16]),
        allow_loopback_http: !secure,
    };
    BrowserClient::login(execution.clone(), &config, SecretValue::new(TOKEN).unwrap())
        .await
        .unwrap();
    let client = BrowserClient::connect(execution.clone(), config.clone())
        .await
        .unwrap();
    let mut streams = Vec::new();
    let count = if secure { 8 } else { 6 };
    for _ in 0..count {
        streams.push(call(&client, "idle").await.unwrap());
    }
    let control = execution
        .deadline_after(Duration::from_secs(3))
        .timeout(call(&client, "stats"))
        .await;
    let completed = matches!(control, Ok(Ok(_)));
    client.close().await.unwrap();
    assert_eq!(
        client.resource_snapshot(),
        BrowserResourceSnapshot::default()
    );
    drop(streams);
    let observer = BrowserClient::connect(execution.clone(), config.clone())
        .await
        .unwrap();
    until(&observer, &execution, "idle", 0).await;
    observer.close().await.unwrap();
    BrowserClient::logout(execution, &config).await.unwrap();
    Ok(
        serde_json::json!({"subscriptions":count, "control_completed":completed, "active_requests":0})
            .to_string(),
    )
}

async fn call(client: &dyn ApiClient, name: &str) -> Result<ApiOutput> {
    let operation = spec(name);
    client
        .call(
            &operation,
            client.input_budget(operation.class).copy(b"{}")?,
        )
        .await
}

#[wasm_bindgen]
pub async fn run_shared_pool_probe(secure: bool) -> std::result::Result<String, JsValue> {
    std::panic::set_hook(Box::new(|info| diagnostic(&info.to_string())));
    assert!(secure);
    let execution = Execution::browser().unwrap();
    let config = BrowserClientConfig {
        endpoint_id: EndpointId::from_bytes([2; 16]),
        allow_loopback_http: false,
    };
    let client = BrowserClient::connect(execution.clone(), config)
        .await
        .unwrap();
    let mut streams = Vec::new();
    for _ in 0..4 {
        streams.push(call(&client, "idle").await.unwrap());
    }
    let ApiOutput::Reply(reply) = call(&client, "pool-barrier").await.unwrap() else {
        panic!("barrier reply")
    };
    let population: serde_json::Value = serde_json::from_slice(reply.json.as_bytes()).unwrap();
    assert_eq!(population["concurrent_subscriptions"], 8);
    drop(streams);
    until(&client, &execution, "idle", 0).await;
    client.close().await.unwrap();
    assert_eq!(
        client.resource_snapshot(),
        BrowserResourceSnapshot::default()
    );
    let timers = rsi_meta_execution::browser_resource_snapshot();
    assert_eq!(timers.pending_timers, 0);
    assert_eq!(timers.active_alarms, 0);
    Ok(
        serde_json::json!({"status":"passed","own_subscriptions":4,"shared_subscriptions":8,
        "active_requests":0,"pending_timers":0,"active_alarms":0})
        .to_string(),
    )
}
async fn stats(client: &dyn ApiClient) -> serde_json::Value {
    let ApiOutput::Reply(reply) = call(client, "stats").await.unwrap() else {
        panic!("stats reply")
    };
    serde_json::from_slice(reply.json.as_bytes()).unwrap()
}
async fn until(client: &dyn ApiClient, execution: &Execution, key: &str, expected: u64) {
    let mut last = serde_json::Value::Null;
    execution
        .deadline_after(Duration::from_secs(10))
        .timeout(async {
            loop {
                last = stats(client).await;
                if last[key] == expected {
                    break;
                }
                execution.sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("waiting for {key}={expected}; last counters: {last}"));
}

#[wasm_bindgen]
pub async fn run_probe(secure: bool) -> std::result::Result<String, JsValue> {
    std::panic::set_hook(Box::new(|info| diagnostic(&info.to_string())));
    let execution = Execution::browser().map_err(|error| JsValue::from_str(&error.to_string()))?;
    let config = BrowserClientConfig {
        endpoint_id: EndpointId::from_bytes([2; 16]),
        allow_loopback_http: !secure,
    };
    BrowserClient::logout(execution.clone(), &config)
        .await
        .unwrap();
    assert!(matches!(
        BrowserClient::connect(execution.clone(), config.clone()).await,
        Err(ApiError::Unauthorized)
    ));
    assert!(matches!(
        BrowserClient::connect(
            execution.clone(),
            BrowserClientConfig {
                allow_loopback_http: secure,
                ..config.clone()
            }
        )
        .await,
        Err(ApiError::Invalid(_))
    ));
    BrowserClient::login(execution.clone(), &config, SecretValue::new(TOKEN).unwrap())
        .await
        .unwrap();
    assert!(matches!(
        BrowserClient::login(execution.clone(), &config, SecretValue::new(TOKEN).unwrap()).await,
        Err(ApiError::Unauthorized)
    ));
    let client = BrowserClient::connect(execution.clone(), config.clone())
        .await
        .unwrap();
    let observer = BrowserClient::connect(execution.clone(), config.clone())
        .await
        .unwrap();
    assert_eq!(client.description().endpoint_id, config.endpoint_id);
    assert!(!format!("{client:?}").contains(TOKEN));
    let operation = spec("binary");
    let bytes: Vec<u8> = (0..512 * 1024).map(|index| (index % 256) as u8).collect();
    let ApiOutput::Reply(reply) = client
        .call(
            &operation,
            client.input_budget(operation.class).copy(&bytes).unwrap(),
        )
        .await
        .unwrap()
    else {
        panic!("binary reply")
    };
    assert_eq!(reply.json.as_bytes(), b"18446744073709551615");
    assert_eq!(reply.binary.unwrap().as_bytes(), bytes);
    let current = stats(&client).await;
    let ApiOutput::Stream(mut events) = call(&client, "events")
        .await
        .unwrap_or_else(|error| panic!("events: {error}; {current}"))
    else {
        panic!("events")
    };
    for _ in 0..2 {
        assert_eq!(
            events.next().await.unwrap().unwrap().json.as_bytes(),
            b"18446744073709551615"
        );
    }
    assert!(events.next().await.is_none());
    assert!(matches!(
        call(&client, "reject").await,
        Err(ApiError::Capacity)
    ));
    let Err(ApiError::Domain(error)) = call(&client, "domain").await else {
        panic!("domain rejection")
    };
    assert_eq!(error.as_bytes(), b"{\"kind\":\"fixture\"}");

    let before = stats(&observer).await;
    let operation = spec("mutate");
    for index in 0..3 {
        let mut waiter = Box::pin(client.call(
            &operation,
            client.input_budget(operation.class).copy(b"{}").unwrap(),
        ));
        assert!(waiter.as_mut().now_or_never().is_none());
        until(
            &observer,
            &execution,
            "started",
            before["started"].as_u64().unwrap() + index + 1,
        )
        .await;
        drop(waiter);
    }
    let mut uncertain = Box::pin(client.call(
        &operation,
        client.input_budget(operation.class).copy(b"{}").unwrap(),
    ));
    assert!(uncertain.as_mut().now_or_never().is_none());
    until(
        &observer,
        &execution,
        "started",
        before["started"].as_u64().unwrap() + 4,
    )
    .await;
    assert!(matches!(
        call(&client, "mutate").await,
        Err(ApiError::Capacity)
    ));
    client.close().await.unwrap();
    assert!(matches!(uncertain.await, Err(ApiError::OutcomeUnknown)));
    assert_eq!(
        client.resource_snapshot(),
        BrowserResourceSnapshot::default()
    );
    call(&observer, "release").await.unwrap();
    until(
        &observer,
        &execution,
        "completed",
        before["completed"].as_u64().unwrap() + 4,
    )
    .await;
    assert_eq!(
        stats(&observer).await["started"],
        before["started"].as_u64().unwrap() + 4
    );

    let operation = spec("read-gate");
    let mut read = Box::pin(observer.call(
        &operation,
        observer.input_budget(operation.class).copy(b"{}").unwrap(),
    ));
    assert!(read.as_mut().now_or_never().is_none());
    until(
        &observer,
        &execution,
        "reads",
        before["reads"].as_u64().unwrap() + 1,
    )
    .await;
    drop(read);
    execution
        .deadline_after(Duration::from_secs(5))
        .timeout(async {
            while observer.resource_snapshot().active_requests != 0 {
                execution.sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    assert_eq!(
        observer.resource_snapshot(),
        BrowserResourceSnapshot::default()
    );
    until(
        &observer,
        &execution,
        "dropped",
        before["dropped"].as_u64().unwrap() + 1,
    )
    .await;
    let unpolled = call(&observer, "idle").await.unwrap();
    assert!(matches!(unpolled, ApiOutput::Stream(_)));
    observer.close().await.unwrap();
    assert_eq!(
        observer.resource_snapshot(),
        BrowserResourceSnapshot::default()
    );
    assert!(matches!(
        call(&observer, "stats").await,
        Err(ApiError::ShuttingDown)
    ));
    drop(unpolled);

    let runtime = Runtime::with_execution(RuntimeLimits::default(), execution.clone()).unwrap();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "fetch-client",
                "test",
                UpdateMode::Replayable,
                Arc::new(BrowserClientFactory),
            ),
            serde_json::to_value(&config).unwrap(),
        )
        .await
        .unwrap();
    let escaped = runtime.root().lookup_local::<ApiClientContract>().unwrap();
    let unpolled = call(escaped.as_ref(), "idle").await.unwrap();
    fiber.dispose().await;
    assert!(runtime.root().lookup_local::<ApiClientContract>().is_none());
    assert!(matches!(
        call(escaped.as_ref(), "stats").await,
        Err(ApiError::ShuttingDown)
    ));
    drop(unpolled);
    assert!(runtime.shutdown().await.is_clean());

    BrowserClient::logout(execution.clone(), &config)
        .await
        .unwrap();
    assert!(matches!(
        BrowserClient::connect(execution, config).await,
        Err(ApiError::Unauthorized)
    ));
    let timers = rsi_meta_execution::browser_resource_snapshot();
    assert_eq!(timers.pending_timers, 0);
    assert_eq!(timers.active_alarms, 0);
    Ok(serde_json::json!({ "status": "passed", "pending_timers": 0, "active_alarms": 0,
        "active_requests": observer.resource_snapshot().active_requests,
        "remote_read_cancelled": true,
        "cases": ["cookie login/logout and mixed authority", "explicit loopback origin", "512 KiB raw binary and exact u64",
            "explicit-end SSE", "known and domain rejection", "abandoned mutations and no replay", "four data slots preserve controls",
            "read drop aborts Fetch", "unpolled stream cleanup", "ordinary plugin withdrawal"] }).to_string())
}
