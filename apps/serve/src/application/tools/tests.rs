use super::*;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};

pub(super) async fn endpoint() -> (TcpListener, Arc<Endpoint>) {
    let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = socket.local_addr().unwrap().port();
    let endpoint = Arc::new(Endpoint {
        listener: Listener {
            protocol: 14,
            generation: "worker".into(),
            port,
        },
        key: SecretValue::new("fixture-key").unwrap(),
        client: reqwest::Client::builder().no_proxy().build().unwrap(),
        retired: CancellationToken::new(),
    });
    (socket, endpoint)
}

pub(super) fn captured_view(
    endpoint: Arc<Endpoint>,
    current: watch::Receiver<Option<Arc<Endpoint>>>,
) -> View {
    View {
        id: "capture".into(),
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
