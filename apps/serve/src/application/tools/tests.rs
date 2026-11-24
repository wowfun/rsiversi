use super::*;
mod transport_probe;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};

#[test]
fn capture_deadlines_preserve_absence_fractional_and_long_values() {
    let row = |value: Value| {
        serde_json::from_value::<Row>(json!({
            "definition": {"name":"probe", "description":"probe", "input_schema":{}},
            "registration":"registration", "timeout_ms":value, "requires_full_access":false
        }))
    };
    assert_eq!(row(Value::Null).unwrap().deadline, None);
    assert_eq!(
        row(json!(0.25)).unwrap().deadline,
        Some(Duration::from_micros(250))
    );
    assert_eq!(
        row(json!(600_001.5)).unwrap().deadline,
        Some(Duration::from_micros(600_001_500))
    );
    for value in [
        json!(0),
        json!(-1),
        json!(1e-15),
        json!(1e300),
        json!("1000"),
        json!({}),
    ] {
        assert!(row(value).is_err());
    }
    assert!(serde_json::from_value::<Row>(json!({"definition":{"name":"probe", "description":"probe", "input_schema":{}}, "registration":"registration"})).is_err());
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep captured HTTP metadata and both prepared-call policy assertions in one end-to-end adapter test."
)]
async fn captured_full_access_policy_reaches_prepared_calls() {
    use rsi_agent_session_protocol::{
        AgentPresetId, FrozenAgentSettings, ModelSelection, SessionId,
    };
    use rsi_ai_protocol::ModelRef;
    use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
    use rsi_sandbox::SandboxMode;
    use rsi_tools_protocol::{ToolCall, ToolCatalogProviderContract};
    let runtime = Runtime::default();
    let tools = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "tools",
                "test",
                UpdateMode::Replayable,
                Arc::new(rsi_tools::ToolsFactory),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    let (socket, endpoint) = endpoint().await;
    let (_sender, current) = watch::channel(Some(endpoint));
    let source = Source {
        profile: "fixture".into(),
        replaced_preset_tools: None,
        catalogs: runtime
            .root()
            .lookup_local::<ToolCatalogProviderContract>()
            .unwrap(),
        endpoint: current,
        lifetime: CancellationToken::new(),
        executions: Arc::new(Semaphore::new(8)),
    };
    let header = SessionHeader::new_local(
        SessionId::new("session").unwrap(),
        1,
        "/workspace",
        AgentPresetId::new("agent").unwrap(),
        FrozenAgentSettings::new(
            "default",
            "system",
            ModelRef::new("deployment", "model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    let selection = ModelSelection {
        model: ModelRef::new("deployment", "model").unwrap(),
        reasoning_effort: None,
    };
    let peer = async {
        let mut input = BufReader::new(socket.accept().await.unwrap().0);
        let mut line = String::new();
        input.read_line(&mut line).await.unwrap();
        assert_eq!(line, "POST /v1/capture HTTP/1.1\r\n");
        let mut length = None;
        loop {
            line.clear();
            input.read_line(&mut line).await.unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length: ") {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
        let mut bytes = vec![0; length.unwrap()];
        input.read_exact(&mut bytes).await.unwrap();
        let request: Value = serde_json::from_slice(&bytes).unwrap();
        let rows: Vec<_> = [("ordinary", false), ("privileged", true)].into_iter().map(|(name, full)|
            json!({"definition":{"name":name,"description":"fixture","input_schema":{}},"registration":name,"requires_full_access":full,"timeout_ms":null})).collect();
        let body =
            json!({"generation":"worker","view":request["view"],"tools":rows,"instructions":""})
                .to_string();
        input
            .into_inner()
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    };
    let reserved = BTreeSet::new();
    let (captured, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            source.capture(&header, &selection, &reserved, CancellationToken::new()),
            peer
        )
    })
    .await
    .unwrap();
    let catalog = captured.unwrap();
    for (name, full) in [("ordinary", false), ("privileged", true)] {
        let call = catalog
            .runtime
            .prepare(
                "request",
                ToolCall {
                    id: name.into(),
                    name: name.into(),
                    arguments: json!({}),
                },
            )
            .unwrap();
        assert_eq!(call.requires_full_access(), full, "{name}");
    }
    drop(catalog);
    drop(source);
    assert!(tools.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

pub(super) async fn endpoint() -> (TcpListener, Arc<Endpoint>) {
    let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = socket.local_addr().unwrap().port();
    let endpoint = Arc::new(Endpoint {
        listener: Listener {
            protocol: 16,
            generation: "worker".into(),
            port,
        },
        key: SecretValue::new("fixture-key").unwrap(),
        client: reqwest::Client::builder().no_proxy().build().unwrap(),
        retired: CancellationToken::new(),
        views: Arc::new(Semaphore::new(256)),
        started: Default::default(),
    });
    (socket, endpoint)
}

pub(super) fn captured_view(
    endpoint: Arc<Endpoint>,
    current: watch::Receiver<Option<Arc<Endpoint>>>,
) -> View {
    View {
        id: "capture".into(),
        release_permit: Some(endpoint.views.clone().try_acquire_owned().unwrap()),
        session: rsi_agent_session_protocol::SessionId::new("session").unwrap(),
        endpoint,
        current,
        lifetime: CancellationToken::new(),
        executions: Arc::new(Semaphore::new(8)),
    }
}

#[tokio::test]
async fn rejected_preparation_reports_the_listener_diagnostic() {
    let (socket, endpoint) = endpoint().await;
    let (_publish, current) = watch::channel(Some(endpoint.clone()));
    let view = captured_view(endpoint, current);
    let request = view.callback::<Value>("prepare", &(), 128, CancellationToken::new());
    let peer = async {
        let mut stream = accept(&socket, "prepare", &Value::Null).await;
        let body = r#"{"error":"input Session changed"}"#;
        stream.write_all(format!("HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    };
    let (result, ()) = tokio::join!(request, peer);
    assert!(
        matches!(result, Err(AgentCompositionError::InvalidInput(message)) if message.contains("400") && message.contains("input Session changed"))
    );
}

#[tokio::test]
async fn capture_preparation_and_start_use_caller_deadlines_while_receipt_reads_time_out() {
    for operation in [
        "capture",
        "prepare",
        "stopping",
        "start",
        "outcome",
        "callback-outcome",
    ] {
        let receipt = matches!(operation, "outcome" | "callback-outcome");
        let (socket, endpoint) = endpoint().await;
        let pending =
            tokio::spawn(async move { endpoint.call::<Value>(operation, &json!({}), 128).await });
        let mut stream = tokio::time::timeout(
            Duration::from_secs(5),
            accept(&socket, operation, &json!({})),
        )
        .await
        .unwrap();
        // Establish real network I/O before pausing: idle simulated time otherwise
        // advances the request deadline while the OS is still connecting.
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(12)).await;
        tokio::task::yield_now().await;
        tokio::time::resume();
        if !receipt {
            assert!(!pending.is_finished());
            respond(&mut stream, r#"{"ok":true}"#).await;
        }
        let result = tokio::time::timeout(Duration::from_secs(5), pending)
            .await
            .unwrap()
            .unwrap();
        if receipt {
            assert!(matches!(result, Err(ToolError::OutcomeUnknown)));
        } else {
            assert_eq!(result.unwrap(), json!({"ok":true}));
        }
    }
}

#[tokio::test]
async fn lost_callback_replies_read_the_original_view_without_repeating_hooks() {
    let (socket, endpoint) = endpoint().await;
    let (_publish, current) = watch::channel(Some(endpoint.clone()));
    let view = captured_view(endpoint, current);
    let turn = rsi_agent_session_protocol::TurnId::new("turn").unwrap();
    let step = rsi_agent_session_protocol::StepId::new("step").unwrap();
    let context = RequestInputContext {
        turn_id: &turn,
        step_id: &step,
        fact_horizon: 3,
        claims: &[],
        previous: None,
        contributed: &[],
        trailing_facts: 0,
    };
    let payload = json!({"view":"capture", "session":"session", "input":context});
    let worker = async {
        for (callback, request, reply) in [
            ("prepare", payload, r#"{"kind":"reject"}"#),
            ("stopping", json!({"view":"capture"}), "null"),
        ] {
            drop(accept(&socket, callback, &request).await);
            let mut lookup = accept(
                &socket,
                "callback-outcome",
                &json!({"view":"capture", "callback":callback}),
            )
            .await;
            respond(&mut lookup, reply).await;
        }
    };
    let request = async {
        assert_eq!(
            view.prepare(&context, CancellationToken::new())
                .await
                .unwrap(),
            rsi_agent_session_protocol::StepInputDecision::reject()
        );
        view.turn_stopping(CancellationToken::new()).await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(worker, request);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn stopping_can_be_cancelled_while_reading_the_original_callback_result() {
    let (socket, endpoint) = endpoint().await;
    let (_publish, current) = watch::channel(Some(endpoint.clone()));
    let view = captured_view(endpoint, current);
    let stop = CancellationToken::new();
    let worker = async {
        drop(accept(&socket, "stopping", &json!({"view": "capture"})).await);
        let _reply = accept(
            &socket,
            "callback-outcome",
            &json!({"view": "capture", "callback": "stopping"}),
        )
        .await;
        stop.cancel();
    };
    let ((), result) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(worker, view.turn_stopping(stop.clone()))
    })
    .await
    .unwrap();
    assert!(matches!(result, Err(AgentCompositionError::ShuttingDown)));
}

async fn accept(listener: &TcpListener, operation: &str, payload: &Value) -> TcpStream {
    let mut input = BufReader::new(listener.accept().await.unwrap().0);
    let mut line = String::new();
    assert!(
        input.read_line(&mut line).await.unwrap() > 0,
        "request line EOF: {operation}"
    );
    assert_eq!(line, format!("POST /v1/{operation} HTTP/1.1\r\n"));
    let mut length = None;
    loop {
        line.clear();
        assert!(
            input.read_line(&mut line).await.unwrap() > 0,
            "request headers EOF: {operation}"
        );
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length: ") {
            length = Some(value.trim().parse::<usize>().unwrap());
        }
    }
    let mut bytes = vec![0; length.unwrap()];
    input.read_exact(&mut bytes).await.unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), *payload);
    input.into_inner()
}

#[tokio::test]
async fn lost_start_and_outcome_replies_do_not_reenter_the_tool_before_or_after_retirement() {
    let (old_socket, old) = endpoint().await;
    let (new_socket, new) = endpoint().await;
    let (publish, current) = watch::channel(Some(old.clone()));
    let view = captured_view(old.clone(), current);
    let receipt = json!({"session":"session","effect":"effect","registration":"registration","identity":{"call_id":"call"}});
    let start = json!({"view":"capture","args":{}});
    let workers = async {
        drop(accept(&old_socket, "start", &start).await);
        drop(accept(&old_socket, "outcome", &receipt).await);
        let lost = accept(&old_socket, "outcome", &receipt).await;
        old.retired.cancel();
        publish.send_replace(Some(new));
        drop(lost);
        let mut stream = accept(&new_socket, "outcome", &receipt).await;
        let reply = r#"{"state":"settled","result":{"value":"one execution","content":[],"is_error":false}}"#;
        respond(&mut stream, reply).await;
    };
    let verify = async {
        let result = view
            .start_and_observe(&start, &receipt, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.value, "one execution");
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(workers, verify)
    })
    .await
    .unwrap();
}

async fn respond(stream: &mut TcpStream, reply: &str) {
    stream
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn dropped_view_release_survives_lost_requests_and_replies() {
    let (listener, endpoint) = endpoint().await;
    let (_publish, current) = watch::channel(Some(endpoint.clone()));
    let remaining = endpoint.views.clone().try_acquire_many_owned(255).unwrap();
    drop(captured_view(endpoint.clone(), current));
    assert!(endpoint.views.clone().try_acquire_owned().is_err());
    let expected = json!({"view":"capture"});
    // Neither an unacknowledged request nor a lost reply proves delivery.
    let mut refused = accept(&listener, "release", &expected).await;
    refused
        .write_all(
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await
        .unwrap();
    drop(refused);
    assert!(endpoint.views.clone().try_acquire_owned().is_err());
    let retried = tokio::time::timeout(
        Duration::from_secs(2),
        accept(&listener, "release", &expected),
    )
    .await
    .expect("a lost release must be retried while the generation lives");
    drop(retried);
    let mut acknowledged = tokio::time::timeout(
        Duration::from_secs(2),
        accept(&listener, "release", &expected),
    )
    .await
    .expect("a lost reply must also be retried");
    respond(&mut acknowledged, "{}").await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while Arc::strong_count(&endpoint) > 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("acknowledged release must retire its task");
    assert_eq!(endpoint.views.available_permits(), 1);
    drop(remaining);
}

#[tokio::test]
async fn definite_release_refusal_retires_delivery() {
    let (listener, endpoint) = endpoint().await;
    let ((), ()) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(endpoint.release_view("unknown"), async {
            let mut stream = accept(&listener, "release", &json!({"view":"unknown"})).await;
            stream
                .write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        })
    })
    .await
    .expect("a definite refusal must end delivery without retiring the generation");
    assert!(!endpoint.retired.is_cancelled());
}

#[tokio::test]
async fn dropped_view_release_stops_with_its_original_generation() {
    let (listener, endpoint) = endpoint().await;
    let release = endpoint.release_view("capture");
    let retire = async {
        let held = accept(&listener, "release", &json!({"view":"capture"})).await;
        endpoint.retired.cancel();
        held
    };
    let ((), _held) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(release, retire)
    })
    .await
    .expect("retirement interrupts a stalled release request");
}

#[tokio::test]
async fn settled_start_delivers_the_result_without_an_outcome_query() {
    let (listener, endpoint) = endpoint().await;
    let (_publish, current) = watch::channel(Some(endpoint.clone()));
    let view = captured_view(endpoint, current);
    let input = json!({"effect":"effect"});
    let value = "result".repeat(100);
    let worker = async {
        let mut stream = accept(&listener, "start", &input).await;
        let reply =
            json!({"state":"settled","result":{"value":value,"content":[],"is_error":false}});
        respond(&mut stream, &reply.to_string()).await;
    };
    let stop = CancellationToken::new();
    let ((), result) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(worker, view.start_and_observe(&input, &input, &stop))
    })
    .await
    .unwrap();
    assert_eq!(result.unwrap().value, value);
}

#[tokio::test]
async fn cancellation_during_start_observes_the_committed_result() {
    let (listener, endpoint) = endpoint().await;
    let (_publish, current) = watch::channel(Some(endpoint.clone()));
    let view = captured_view(endpoint, current);
    let stop = CancellationToken::new();
    let request = json!({"view":"captured"});
    let identity = json!({"effect":"running"});
    let worker = async {
        let start = accept(&listener, "start", &request).await;
        stop.cancel();
        let mut cancel = accept(&listener, "cancel", &identity).await;
        respond(&mut cancel, r#"{"accepted":true}"#).await;
        let mut query = accept(&listener, "outcome", &identity).await;
        respond(
            &mut query,
            r#"{"state":"settled","result":{"value":"stopped","content":[],"is_error":true}}"#,
        )
        .await;
        drop(start);
    };
    let ((), result) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(worker, view.start_and_observe(&request, &identity, &stop))
    })
    .await
    .unwrap();
    assert_eq!(result.unwrap().value, "stopped");
}

#[tokio::test]
async fn unreadable_receipt_is_unknown_after_an_acknowledged_start() {
    let (listener, endpoint) = endpoint().await;
    let (_publish, current) = watch::channel(Some(endpoint.clone()));
    let view = captured_view(endpoint, current);
    let request = json!({"effect":"effect"});
    let worker = async {
        respond(
            &mut accept(&listener, "start", &request).await,
            r#"{"state":"pending"}"#,
        )
        .await;
        respond(
            &mut accept(&listener, "outcome", &request).await,
            "invalid JSON",
        )
        .await;
    };
    let verify = async {
        assert!(matches!(
            view.start_and_observe(&request, &request, &CancellationToken::new())
                .await,
            Err(ToolError::OutcomeUnknown)
        ));
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(worker, verify)
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cancellation_interrupts_a_stalled_read_and_an_unconfirmed_body_remains_unknown() {
    let (listener, endpoint) = endpoint().await;
    let (_publish, current) = watch::channel(Some(endpoint.clone()));
    let view = captured_view(endpoint, current);
    let start = json!({"view":"capture"});
    let receipt = json!({"effect":"effect"});
    let stop = CancellationToken::new();
    let worker = async {
        respond(
            &mut accept(&listener, "start", &start).await,
            r#"{"state":"pending"}"#,
        )
        .await;
        let held = accept(&listener, "outcome", &receipt).await;
        stop.cancel();
        respond(
            &mut accept(&listener, "cancel", &receipt).await,
            r#"{"accepted":true}"#,
        )
        .await;
        let pending = accept(&listener, "outcome", &receipt).await;
        (held, pending)
    };
    let check = async {
        assert!(matches!(
            view.start_and_observe(&start, &receipt, &stop).await,
            Err(ToolError::OutcomeUnknown)
        ));
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(worker, check)
    })
    .await
    .unwrap();
}
