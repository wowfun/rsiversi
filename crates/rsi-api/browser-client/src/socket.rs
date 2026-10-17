use crate::bridge::{Head, Pull, RequestData};
use js_sys::{ArrayBuffer, Uint8Array};
use rsi_api_protocol::{
    ApiError, ByteBudget, DeviceId, HostEpoch, Result, RetainedBytes,
    browser_stream::{CHUNK_BYTES, HEAD_BYTES, Preface},
};
use std::{
    cell::Cell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use wasm_bindgen::{JsCast, closure::Closure};
use web_sys::{Event, MessageEvent, WebSocket};

struct LocalSocket {
    socket: WebSocket,
    opened: CancellationToken,
    closed: CancellationToken,
    failed: CancellationToken,
    expected: Rc<Cell<usize>>,
    messages: mpsc::Receiver<Uint8Array>,
    _open: Closure<dyn FnMut(Event)>,
    _close: Closure<dyn FnMut(Event)>,
    _error: Closure<dyn FnMut(Event)>,
    _message: Closure<dyn FnMut(MessageEvent)>,
}
impl LocalSocket {
    fn new(data: &RequestData) -> Result<Self> {
        if data.authorization.is_some() || data.launch_ticket.is_some() {
            return Err(lost());
        }
        let header = |name| {
            data.headers
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.as_str())
                .ok_or_else(lost)
        };
        let preface = Preface {
            device: DeviceId::parse(header("x-rsi-expected-device")?)?,
            epoch: HostEpoch::parse(header("x-rsi-host-epoch")?)?,
            length: data.body.len(),
        };
        let origin = data.url.strip_prefix("http://").ok_or_else(lost)?;
        let socket = WebSocket::new_with_str(&format!("ws://{origin}"), &preface.encode())
            .map_err(|_| lost())?;
        socket.set_binary_type(web_sys::BinaryType::Arraybuffer);
        let opened = CancellationToken::new();
        let closed = CancellationToken::new();
        let failed = CancellationToken::new();
        let expected = Rc::new(Cell::new(0));
        let (sender, messages) = mpsc::channel(1);
        let signal = opened.clone();
        let open = Closure::new(move |_: Event| signal.cancel());
        socket.set_onopen(Some(open.as_ref().unchecked_ref()));
        let signal = closed.clone();
        let close = Closure::new(move |_: Event| signal.cancel());
        socket.set_onclose(Some(close.as_ref().unchecked_ref()));
        let signal = failed.clone();
        let error = Closure::new(move |_: Event| signal.cancel());
        socket.set_onerror(Some(error.as_ref().unchecked_ref()));
        let limit = expected.clone();
        let signal = failed.clone();
        let peer = socket.clone();
        let message = Closure::new(move |event: MessageEvent| {
            let maximum = limit.replace(0);
            let valid = event
                .data()
                .dyn_into::<ArrayBuffer>()
                .ok()
                .filter(|buffer| maximum != 0 && buffer.byte_length() as usize <= maximum);
            if let Some(buffer) = valid
                && sender.try_send(Uint8Array::new(&buffer)).is_ok()
            {
                return;
            }
            signal.cancel();
            let _ = peer.close();
        });
        socket.set_onmessage(Some(message.as_ref().unchecked_ref()));
        Ok(Self {
            socket,
            opened,
            closed,
            failed,
            expected,
            messages,
            _open: open,
            _close: close,
            _error: error,
            _message: message,
        })
    }
    async fn receive(
        &mut self,
        stopped: &CancellationToken,
        chunks: &ByteBudget,
    ) -> Result<RetainedBytes> {
        let bytes = tokio::select! { biased;
            () = stopped.cancelled() => return Err(ApiError::ShuttingDown),
            () = self.failed.cancelled() => return Err(lost()),
            value = self.messages.recv() => value.ok_or_else(lost)?,
            () = self.closed.cancelled() => return Err(lost()),
        };
        let reservation = chunks.reserve(bytes.length() as usize)?;
        let mut copied = vec![0; bytes.length() as usize];
        bytes.copy_to(&mut copied);
        reservation.retain_vec(copied)
    }
    async fn close(&self) {
        // Bridge::close bounds caller cleanup, but a missing close event must keep
        // this task's admission charged. Error events do not establish settlement.
        if !self.closed.is_cancelled() {
            let _ = self.socket.close();
            self.closed.cancelled().await;
        }
    }
}
impl Drop for LocalSocket {
    fn drop(&mut self) {
        let _ = self.socket.close();
        self.socket.set_onopen(None);
        self.socket.set_onclose(None);
        self.socket.set_onerror(None);
        self.socket.set_onmessage(None);
    }
}

pub(crate) async fn run(
    data: RequestData,
    head: oneshot::Sender<Result<Head>>,
    mut pulls: mpsc::Receiver<Pull>,
    stopped: CancellationToken,
    windows: ByteBudget,
    chunks: ByteBudget,
    started: Arc<AtomicBool>,
) {
    if stopped.is_cancelled() {
        let _ = head.send(Err(ApiError::ShuttingDown));
        return;
    }
    started.store(true, Ordering::Release);
    let mut socket = match LocalSocket::new(&data) {
        Ok(socket) => socket,
        Err(error) => {
            let _ = head.send(Err(error));
            return;
        }
    };
    let mut head = Some(head);
    let result: Result<()> = async {
        tokio::select! { biased;
            () = stopped.cancelled() => return Err(ApiError::ShuttingDown),
            () = socket.failed.cancelled() => return Err(lost()),
            () = socket.closed.cancelled() => return Err(lost()),
            () = socket.opened.cancelled() => {},
        }
        let window = windows.reserve(HEAD_BYTES)?;
        socket.expected.set(HEAD_BYTES);
        // The bridge retains send admission until this local task has closed.
        let source = Uint8Array::from(data.body.as_bytes());
        socket
            .socket
            .send_with_array_buffer_view(source.as_ref())
            .map_err(|_| lost())?;
        let bytes = socket.receive(&stopped, &chunks).await?;
        let metadata = decode_head(bytes.as_bytes())?;
        drop(bytes);
        drop(window);
        head.take()
            .expect("one response head")
            .send(Ok(metadata))
            .map_err(|_| lost())?;
        loop {
            let pull = tokio::select! { biased;
                () = stopped.cancelled() => break,
                pull = pulls.recv() => match pull { Some(pull) => pull, None => break },
            };
            let window = windows.reserve(CHUNK_BYTES)?;
            socket.expected.set(CHUNK_BYTES);
            socket.socket.send_with_u8_array(&[1]).map_err(|_| lost())?;
            let received = socket.receive(&stopped, &chunks).await;
            drop(window);
            let ended = received.as_ref().is_ok_and(RetainedBytes::is_empty);
            let failed = received.is_err();
            if pull
                .send(received.map(|bytes| if bytes.is_empty() { None } else { Some(bytes) }))
                .is_err()
                || ended
                || failed
            {
                break;
            }
        }
        Ok(())
    }
    .await;
    if let Some(head) = head {
        let _ = head.send(Err(result.err().unwrap_or_else(lost)));
    }
    socket.close().await;
}

fn decode_head(bytes: &[u8]) -> Result<Head> {
    let metadata: rsi_api_protocol::browser_stream::Head =
        serde_json::from_slice(bytes).map_err(|_| lost())?;
    let mut headers = http::HeaderMap::new();
    headers.insert("x-rsi-wire-version", http::HeaderValue::from_static("1"));
    headers.insert("x-rsi-http-version", http::HeaderValue::from_static("1.1"));
    headers.insert(
        "x-rsi-endpoint-id",
        metadata.endpoint.as_str().parse().map_err(|_| lost())?,
    );
    headers.insert(
        "x-rsi-host-epoch",
        metadata.epoch.as_str().parse().map_err(|_| lost())?,
    );
    headers.insert(
        "content-type",
        metadata.content_type.parse().map_err(|_| lost())?,
    );
    if let Some(length) = metadata.content_length {
        headers.insert("content-length", http::HeaderValue::from(length));
    }
    Ok(Head {
        status: metadata.status,
        headers,
    })
}
fn lost() -> ApiError {
    ApiError::Invalid("browser subscription transport closed".into())
}
