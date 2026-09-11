use crate::{TOKEN, malformed_specs};
use futures_util::StreamExt;
use rsi_api_browser_client::{BrowserClient, BrowserClientConfig, BrowserResourceSnapshot};
use rsi_api_protocol::*;
use rsi_credentials_protocol::SecretValue;
use rsi_meta::Execution;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn diagnostic(message: &str);
}

#[wasm_bindgen]
pub async fn run_caller_fault_probe(_: bool) -> std::result::Result<String, JsValue> {
    std::panic::set_hook(Box::new(|info| diagnostic(&info.to_string())));
    let execution = Execution::browser().unwrap();
    let result = BrowserClient::connect(
        execution,
        BrowserClientConfig {
            endpoint_id: EndpointId::from_bytes([2; 16]),
            allow_loopback_http: true,
        },
    )
    .await;
    assert!(
        matches!(result, Err(ApiError::Invalid(_) | ApiError::Unauthorized)),
        "malformed caller was admitted: {result:?}"
    );
    let resources = rsi_meta_execution::browser_resource_snapshot();
    assert_eq!(resources.pending_timers, 0);
    assert_eq!(resources.active_alarms, 0);
    Ok(serde_json::json!({"status":"rejected","pending_timers":0,"active_alarms":0}).to_string())
}

#[wasm_bindgen]
pub async fn run_malformed_probe(_: bool) -> std::result::Result<String, JsValue> {
    std::panic::set_hook(Box::new(|info| diagnostic(&info.to_string())));
    let execution = Execution::browser().unwrap();
    let config = BrowserClientConfig {
        endpoint_id: EndpointId::from_bytes([2; 16]),
        allow_loopback_http: true,
    };
    let client = BrowserClient::connect(execution.clone(), config.clone())
        .await
        .unwrap();
    let mut cases = 0;
    for operation in malformed_specs()
        .into_iter()
        .filter(|operation| operation.id.domain() == "fault")
    {
        let result = client
            .call(
                &operation,
                client.input_budget(operation.class).copy(b"{}").unwrap(),
            )
            .await;
        if operation.class == OperationClass::Subscription {
            let ApiOutput::Stream(mut stream) = result.unwrap() else {
                panic!("stream")
            };
            let mut failed = false;
            while let Some(item) = stream.next().await {
                if item.is_err() {
                    failed = true;
                    break;
                }
            }
            assert!(failed, "accepted malformed stream {}", operation.id.name());
        } else if operation.effect == OperationEffect::Mutation {
            assert!(
                matches!(result, Err(ApiError::OutcomeUnknown)),
                "{}: {result:?}",
                operation.id.name()
            );
        } else {
            assert!(
                matches!(
                    result,
                    Err(ApiError::Invalid(_) | ApiError::Backend(_) | ApiError::ShuttingDown)
                ),
                "{}: {result:?}",
                operation.id.name()
            );
        }
        cases += 1;
    }
    client.close().await.unwrap();
    assert_eq!(
        client.resource_snapshot(),
        BrowserResourceSnapshot::default()
    );
    let login = BrowserClient::login(execution, &config, SecretValue::new(TOKEN).unwrap()).await;
    assert!(
        matches!(login, Err(ApiError::OutcomeUnknown)),
        "lost cookie exchange: {login:?}"
    );
    let timers = rsi_meta_execution::browser_resource_snapshot();
    assert_eq!(timers.pending_timers, 0);
    assert_eq!(timers.active_alarms, 0);
    Ok(serde_json::json!({"status":"passed","malformed_cases":cases,"lost_login":"outcome_unknown",
        "active_requests":0,"pending_timers":0,"active_alarms":0}).to_string())
}
