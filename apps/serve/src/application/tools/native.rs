use super::{MAX_RESPONSE, ToolError, ToolOutput, ToolResult, View};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use rsi_tools_protocol::{
    AroundTool, PendingToolApproval, Result, ToolBody, ToolCall, ToolContent, ToolContextMessage,
    ToolDecision, ToolGate, ToolMiddleware, ToolOutputDeclaration,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    WebSocketStream, client_async_with_config,
    tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig},
};
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Ask {
        reason: String,
    },
    Denied {
        reason: String,
        #[serde(default)]
        additional_contexts: Vec<ToolContextMessage>,
    },
    Next,
    Unchanged,
    Rendered {
        content: Vec<ToolContent>,
        additional_contexts: Vec<ToolContextMessage>,
    },
    Settled {
        result: ToolOutput,
    },
}

type Socket = WebSocketStream<TcpStream>;

async fn receive(socket: &mut Socket) -> Result<Reply> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(&text).map_err(|_| ToolError::OutcomeUnknown);
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            _ => return Err(ToolError::OutcomeUnknown),
        }
    }
}

fn settled(output: ToolOutput, declaration: Option<&ToolOutputDeclaration>) -> Result<ToolResult> {
    if !output.is_error
        && let Some(declaration) = declaration
    {
        declaration.validate_value(&output.value)?;
    }
    output.into_result()
}

async fn send(socket: &mut Socket, value: &impl Serialize) -> Result<()> {
    let data = serde_json::to_string(value).map_err(|_| ToolError::OutcomeUnknown)?;
    socket
        .send(Message::text(data))
        .await
        .map_err(|_| ToolError::OutcomeUnknown)
}

impl View {
    async fn native_socket(&self) -> Result<Socket> {
        let port = self.endpoint.listener.port;
        let mut request = format!("ws://127.0.0.1:{port}/v1/native")
            .into_client_request()
            .map_err(|_| ToolError::OutcomeUnknown)?;
        request.headers_mut().insert(
            "authorization",
            self.endpoint
                .key
                .expose_secret()
                .parse()
                .map_err(|_| ToolError::OutcomeUnknown)?,
        );
        let stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .map_err(|_| ToolError::OutcomeUnknown)?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_RESPONSE))
            .max_frame_size(Some(MAX_RESPONSE));
        client_async_with_config(request, stream, Some(config))
            .await
            .map(|(s, _)| s)
            .map_err(|_| ToolError::OutcomeUnknown)
    }

    async fn stopped(&self, stop: &CancellationToken) -> ToolError {
        tokio::select! {
            () = stop.cancelled() => ToolError::Cancelled,
            () = self.lifetime.cancelled() => ToolError::OutcomeUnknown,
            () = self.endpoint.retired.cancelled() => ToolError::OutcomeUnknown,
        }
    }
}

#[async_trait]
impl ToolMiddleware for View {
    async fn prepare(
        self: Arc<Self>,
        call: &ToolCall,
        output: Option<&ToolOutputDeclaration>,
        stop: &CancellationToken,
    ) -> Result<ToolGate> {
        #[derive(Serialize)]
        struct Invocation<'a> {
            view: &'a str,
            session: &'a str,
            name: &'a str,
            args: &'a serde_json::Value,
            call_id: &'a str,
            output_schema: Option<&'a serde_json::Value>,
        }
        let begin = async {
            let mut socket = self.native_socket().await?;
            send(
                &mut socket,
                &Invocation {
                    view: &self.id,
                    session: self.session.as_str(),
                    name: &call.name,
                    args: &call.arguments,
                    call_id: &call.id,
                    output_schema: output.map(ToolOutputDeclaration::schema),
                },
            )
            .await?;
            let first = receive(&mut socket).await?;
            Ok::<_, ToolError>((socket, first))
        };
        let (socket, first) = tokio::select! {
            biased; error = self.stopped(stop) => return Err(error), r = begin => r?,
        };
        let invocation = NativeInvocation {
            view: self,
            socket,
            output: output.cloned(),
        };
        match first {
            Reply::Ask { reason } if !reason.trim().is_empty() => Ok(ToolGate::Ask {
                reason,
                pending: Box::new(invocation),
            }),
            reply => invocation.decision(reply).map(ToolGate::Decided),
        }
    }
}

#[derive(Debug)]
struct NativeInvocation {
    view: Arc<View>,
    socket: Socket,
    output: Option<ToolOutputDeclaration>,
}

impl NativeInvocation {
    async fn wait_withdrawal(&mut self) {
        tokio::select! {
            _ = receive(&mut self.socket) => {},
            () = self.view.lifetime.cancelled() => {},
            () = self.view.endpoint.retired.cancelled() => {},
        }
    }

    fn decision(self, reply: Reply) -> Result<ToolDecision> {
        match reply {
            Reply::Next => Ok(ToolDecision::Ready(Box::new(self))),
            Reply::Denied {
                reason,
                additional_contexts,
            } => {
                ToolContextMessage::validate_batch(&additional_contexts)?;
                Ok(ToolDecision::Denied {
                    reason,
                    additional_contexts,
                })
            }
            Reply::Settled { result } if !result.is_error => Ok(ToolDecision::Ready(Box::new(
                Answered(settled(result, self.output.as_ref())?),
            ))),
            _ => Err(ToolError::OutcomeUnknown),
        }
    }
}

#[async_trait]
impl PendingToolApproval for NativeInvocation {
    async fn withdrawn(&mut self) {
        self.wait_withdrawal().await;
    }

    async fn answer(
        mut self: Box<Self>,
        allow: bool,
        stop: &CancellationToken,
    ) -> Result<ToolDecision> {
        let answer = async {
            send(&mut self.socket, &serde_json::json!({ "allow": allow })).await?;
            receive(&mut self.socket).await
        };
        let reply = tokio::select! {
            biased;
            error = self.view.stopped(stop) => return Err(error),
            reply = answer => reply?,
        };
        self.decision(reply)
    }
}
#[derive(Debug)]
struct Answered(ToolResult);
#[async_trait]
impl AroundTool for Answered {
    async fn execute(self: Box<Self>, _: ToolBody<'_>) -> Result<ToolResult> {
        Ok(self.0)
    }
}
#[async_trait]
impl AroundTool for NativeInvocation {
    async fn withdrawn(&mut self) {
        self.wait_withdrawal().await;
    }

    async fn execute(self: Box<Self>, body: ToolBody<'_>) -> Result<ToolResult> {
        let stop = body.execution().cancellation.clone();
        self.view
            .resolve_native(self.socket, body.run(), &stop, self.output.as_ref())
            .await
    }
}

impl View {
    async fn resolve_native(
        &self,
        mut socket: Socket,
        run: impl Future<Output = Result<ToolResult>>,
        stop: &CancellationToken,
        declaration: Option<&ToolOutputDeclaration>,
    ) -> Result<ToolResult> {
        tokio::pin!(run);
        let mut entered = false;
        let result = tokio::select! {
            biased;
            error = self.stopped(stop) => Err(error),
            _ = receive(&mut socket) => Err(ToolError::OutcomeUnknown),
            result = std::future::poll_fn(|cx| {
                entered = true;
                run.as_mut().poll(cx)
            }) => Ok(result),
        };
        let mut original = match result {
            Ok(result) => result?,
            Err(error) => {
                if entered {
                    stop.cancel();
                    let _ = run.await;
                }
                return Err(error);
            }
        };
        let finish = async {
            send(&mut socket, &original).await?;
            match receive(&mut socket).await? {
                Reply::Unchanged => Ok(original),
                Reply::Rendered {
                    content,
                    additional_contexts,
                } => {
                    original.content = content;
                    original.additional_contexts = additional_contexts;
                    Ok(original)
                }
                Reply::Settled { result } => settled(result, declaration),
                Reply::Next | Reply::Ask { .. } | Reply::Denied { .. } => {
                    Err(ToolError::OutcomeUnknown)
                }
            }
        };
        tokio::select! { biased; error = self.stopped(stop) => Err(error), result = finish => result }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{captured_view, endpoint};
    use super::*;
    use serde_json::json;
    use tokio::sync::{oneshot, watch};
    use tokio_tungstenite::accept_async;

    #[tokio::test]
    async fn losing_the_application_withdraws_the_parked_question_on_its_original_socket() {
        let (listener, endpoint) = endpoint().await;
        let (_publish, current) = watch::channel(Some(endpoint.clone()));
        let view = Arc::new(captured_view(endpoint, current));
        let (parked, wait) = oneshot::channel();
        let peer = async {
            let mut socket = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            let input = socket.next().await.unwrap().unwrap().into_text().unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&input).unwrap()["call_id"],
                "same-call"
            );
            send(
                &mut socket,
                &json!({"state":"ask","reason":"Inspect command"}),
            )
            .await
            .unwrap();
            wait.await.unwrap();
            socket.close(None).await.unwrap();
        };
        let review = async {
            let call = ToolCall {
                id: "same-call".into(),
                name: "bash".into(),
                arguments: json!({}),
            };
            let ToolGate::Ask {
                reason,
                mut pending,
            } = view
                .prepare(&call, None, &CancellationToken::new())
                .await
                .unwrap()
            else {
                panic!("expected live review")
            };
            assert_eq!(reason, "Inspect command");
            parked.send(()).unwrap();
            pending.withdrawn().await;
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(review, peer)
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_retired_view_never_polls_the_waiting_native_body() {
        let (listener, endpoint) = endpoint().await;
        let (_publish, current) = watch::channel(Some(endpoint.clone()));
        let view = captured_view(endpoint, current);
        let (socket, _peer) = tokio::join!(view.native_socket(), async {
            accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap()
        });
        view.lifetime.cancel();
        let result = view
            .resolve_native(
                socket.unwrap(),
                async { panic!("retired native body executed") },
                &CancellationToken::new(),
                None,
            )
            .await;
        assert!(matches!(result, Err(ToolError::OutcomeUnknown)));
    }

    #[tokio::test]
    async fn native_render_keeps_wide_values_and_control_frames_do_not_cancel_the_body() {
        let (listener, endpoint) = endpoint().await;
        let (_publish, current) = watch::channel(Some(endpoint.clone()));
        let view = captured_view(endpoint, current);
        let stop = CancellationToken::new();
        let (finish, done) = oneshot::channel();
        let peer = async {
            let mut peer = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            peer.send(Message::Ping(vec![1].into())).await.unwrap();
            assert!(matches!(peer.next().await, Some(Ok(Message::Pong(_)))));
            finish.send(()).unwrap();
            let original = peer.next().await.unwrap().unwrap().into_text().unwrap();
            assert!(original.contains("9007199254740993"));
            send(&mut peer,&json!({"state":"rendered","content":[{"type":"text","text":"edited"}],"additional_contexts":[]})).await.unwrap();
        };
        let client = async {
            let socket = view.native_socket().await.unwrap();
            let output = view
                .resolve_native(
                    socket,
                    async {
                        done.await.unwrap();
                        ToolResult::new(json!(9_007_199_254_740_993_u64), vec![], false)
                    },
                    &stop,
                    None,
                )
                .await
                .unwrap();
            assert_eq!(output.value, 9_007_199_254_740_993_u64);
            assert_eq!(
                output.content,
                [ToolContent::Text {
                    text: "edited".into()
                }]
            );
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(client, peer)
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn loss_after_next_cancels_and_drains_the_native_body() {
        let (listener, endpoint) = endpoint().await;
        let (_publish, current) = watch::channel(Some(endpoint.clone()));
        let view = captured_view(endpoint, current);
        let stop = CancellationToken::new();
        let (entered, entry) = oneshot::channel();
        let mut drained = false;
        let peer = async {
            let socket = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            entry.await.unwrap();
            drop(socket);
        };
        let client = async {
            let socket = view.native_socket().await.unwrap();
            let result = view
                .resolve_native(
                    socket,
                    async {
                        entered.send(()).unwrap();
                        stop.cancelled().await;
                        drained = true;
                        Err(ToolError::Cancelled)
                    },
                    &stop,
                    None,
                )
                .await;
            assert!(matches!(result, Err(ToolError::OutcomeUnknown)));
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(client, peer)
        })
        .await
        .unwrap();
        assert!(drained);
    }
}
