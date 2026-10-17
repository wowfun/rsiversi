use crate::{
    access::Access,
    delivery::Delivery,
    policy::one,
    server::{Body, State, failure, full, operation},
};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http::{Request, Response};
use http_body_util::BodyExt;
use hyper::{
    body::{Body as _, Incoming},
    upgrade::OnUpgrade,
};
use hyper_util::rt::TokioIo;
use rsi_api_protocol::{
    ApiError, ApiInvocation, ApiOutput, ByteReservation, OperationClass, OperationEffect, Result,
    browser_stream::{CHUNK_BYTES, HEAD_BYTES, Head, Preface},
};
use std::{sync::Arc, time::Duration};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        protocol::{Role, WebSocketConfig},
    },
};
use tokio_util::sync::CancellationToken;

type Socket = WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>;

pub(crate) fn is_upgrade(request: &Request<Incoming>) -> bool {
    request.headers().contains_key("upgrade")
}

pub(crate) struct Pending {
    upgrade: OnUpgrade,
    invocation: Box<dyn ApiInvocation>,
    input: ByteReservation,
    scratch: ByteReservation,
    length: usize,
    revoked: CancellationToken,
}

pub(crate) fn prepare(
    state: &State,
    mut request: Request<Incoming>,
    delivery: &Delivery,
) -> Result<(Response<Body>, Pending)> {
    state.access.fence(&request)?;
    if !matches!(&state.access, Access::Remote { policy, .. } if !policy.secure)
        || request.version() != http::Version::HTTP_11
        || request.method() != http::Method::GET
        || request.uri().query().is_some()
        || !request.body().is_end_stream()
    {
        return Err(ApiError::Unauthorized);
    }
    let headers = request.headers();
    // These fields come only from the closed browser subprotocol, never a mix of
    // custom headers and cookie authority. Duplicate handshake fields also reject.
    for name in [
        "authorization",
        "x-rsi-expected-device",
        "x-rsi-host-epoch",
        "x-rsi-csrf",
        "x-rsi-wire-version",
    ] {
        if headers.contains_key(name) {
            return Err(ApiError::Unauthorized);
        }
    }
    for name in [
        "upgrade",
        "connection",
        "sec-websocket-key",
        "sec-websocket-version",
    ] {
        one(headers, name)?;
    }
    let protocol = one(headers, "sec-websocket-protocol")?
        .ok_or(ApiError::Unauthorized)?
        .to_owned();
    let preface = Preface::parse(&protocol)?;
    let mut response =
        tokio_tungstenite::tungstenite::handshake::server::create_response_with_body(
            &request,
            || full(Bytes::new()),
        )
        .map_err(|_| ApiError::Invalid("invalid subscription handshake".into()))?;
    response.headers_mut().insert(
        "sec-websocket-protocol",
        protocol.parse().map_err(|_| ApiError::Unauthorized)?,
    );
    let caller = state
        .access
        .authenticate_socket(request.headers(), &preface.device)?;
    if preface.epoch != state.epoch {
        return Err(ApiError::ShuttingDown);
    }
    let id = operation(request.uri().path())?;
    let invocation = state.dispatch.admit(&id, caller.origin)?;
    let spec = invocation.spec();
    if spec.class != OperationClass::Subscription
        || spec.effect != OperationEffect::Read
        || preface.length > spec.maximum_request_bytes
    {
        return Err(ApiError::Invalid(
            "socket requires a bounded read subscription".into(),
        ));
    }
    let input = invocation.input_budget().reserve(preface.length)?;
    // Covers the incoming message, fixed read buffer, encoded head and bounded
    // outgoing codec buffer independently of the API's retained body allocation.
    let scratch = state.socket_scratch.reserve(
        preface
            .length
            .checked_mul(2)
            .ok_or(ApiError::Capacity)?
            .checked_add(2 * CHUNK_BYTES + HEAD_BYTES + 1024)
            .ok_or(ApiError::Capacity)?,
    )?;
    delivery.admit(invocation.retain_admission());
    Ok((
        response,
        Pending {
            upgrade: hyper::upgrade::on(&mut request),
            invocation,
            input,
            scratch,
            length: preface.length,
            revoked: caller.revoked,
        },
    ))
}

impl Pending {
    pub async fn run(self, state: Arc<State>, stop: CancellationToken) {
        let revoked = self.revoked.clone();
        let retiring = self.invocation.retiring();
        tokio::select! { biased;
            () = stop.cancelled() => {},
            () = revoked.cancelled() => {},
            () = retiring.cancelled() => {},
            _ = self.run_inner(&state) => {},
        }
    }
    async fn run_inner(self, state: &State) -> Result<()> {
        let Self {
            upgrade,
            invocation,
            input,
            scratch,
            length,
            revoked,
        } = self;
        let _scratch = scratch;
        let deadline = state.execution.deadline_after(Duration::from_secs(15));
        let upgraded = deadline
            .timeout(upgrade)
            .await
            .map_err(|_| lost())?
            .map_err(|_| lost())?;
        let config = WebSocketConfig::default()
            .read_buffer_size(1024)
            .write_buffer_size(0)
            .max_write_buffer_size(2 * CHUNK_BYTES)
            .max_message_size(Some(length.max(1)))
            .max_frame_size(Some(length.max(125)))
            .accept_unmasked_frames(false);
        let mut socket =
            WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, Some(config))
                .await;
        let message = deadline
            .timeout(next_application_message(&mut socket))
            .await
            .map_err(|_| lost())??;
        let Message::Binary(bytes) = message else {
            return Err(lost());
        };
        if bytes.len() != length {
            return Err(lost());
        }
        let input = input.copy(&bytes)?;
        drop(bytes);
        let output = tokio::select! { biased;
            _ = next_application_message(&mut socket) => return Err(lost()),
            output = invocation.invoke(input) => output,
        };
        let response = match output {
            Ok(ApiOutput::Stream(stream)) => crate::sse::response(stream, revoked),
            Ok(ApiOutput::Reply(_)) => failure(ApiError::Backend(
                "subscription returned finite success".into(),
            )),
            Err(error) => failure(error),
        };
        let head = Head {
            status: response.status().as_u16(),
            endpoint: state.endpoint.clone(),
            epoch: state.epoch.clone(),
            content_type: one(response.headers(), "content-type")?
                .ok_or_else(lost)?
                .to_owned(),
            content_length: one(response.headers(), "content-length")?
                .map(str::parse)
                .transpose()
                .map_err(|_| lost())?,
        };
        rsi_api_protocol::measure_json(&head, HEAD_BYTES)?;
        let encoded = serde_json::to_vec(&head).map_err(|_| lost())?;
        socket
            .send(Message::Binary(encoded.into()))
            .await
            .map_err(|_| lost())?;
        deliver(&mut socket, response.into_body()).await
    }
}

async fn deliver(socket: &mut Socket, mut body: Body) -> Result<()> {
    let mut pending = Bytes::new();
    loop {
        match next_application_message(socket).await? {
            Message::Binary(credit) if credit.as_ref() == [1] => {}
            _ => return Err(lost()),
        }
        while pending.is_empty() {
            let frame = tokio::select! { biased;
                _ = next_application_message(socket) => return Err(lost()),
                frame = body.frame() => frame,
            };
            let Some(frame) = frame else {
                socket
                    .send(Message::Binary(Bytes::new()))
                    .await
                    .map_err(|_| lost())?;
                socket.close(None).await.map_err(|_| lost())?;
                return Ok(());
            };
            pending = frame.map_err(|_| lost())?.into_data().map_err(|_| lost())?;
        }
        let chunk = pending.split_to(pending.len().min(CHUNK_BYTES));
        socket
            .send(Message::Binary(chunk))
            .await
            .map_err(|_| lost())?;
    }
}
async fn next_application_message(socket: &mut Socket) -> Result<Message> {
    loop {
        match socket.next().await.ok_or_else(lost)?.map_err(|_| lost())? {
            Message::Ping(_) => socket.flush().await.map_err(|_| lost())?,
            Message::Pong(_) => (),
            message => return Ok(message),
        }
    }
}
fn lost() -> ApiError {
    ApiError::Invalid("browser subscription transport closed".into())
}
