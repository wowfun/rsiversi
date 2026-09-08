use futures_util::FutureExt;
use js_sys::{Function, JsString, Reflect, Uint8Array};
use rsi_api_client::ResponseBytes;
use rsi_api_protocol::{
    ApiError, ByteBudget, ByteReservation, OperationClass, Result, RetainedBytes,
};
use rsi_meta::Execution;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AbortController, DedicatedWorkerGlobalScope, Headers, ReadableStreamByobReader, Request,
    RequestCredentials, RequestInit, RequestMode, RequestRedirect, Response,
};
use zeroize::Zeroizing;

const TASKS: usize = 64;
const WINDOW: u32 = 64 * 1024;

/// Explicitly owned browser bridge resources; excludes opaque platform buffering.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BrowserResourceSnapshot {
    /// Tasks that still own platform handles or pending promises.
    pub active_requests: usize,
    /// Retained JavaScript request source copies.
    pub send_bytes: usize,
    /// JavaScript BYOB windows supplied to pending reads.
    pub window_bytes: usize,
    /// Rust chunks awaiting consumption by the shared decoder.
    pub chunk_bytes: usize,
}

#[derive(Debug)]
pub(crate) struct Bridge {
    slots: Arc<Semaphore>,
    stopped: CancellationToken,
    drained: Notify,
    send: [ByteBudget; 2],
    window: [ByteBudget; 2],
    chunk: [ByteBudget; 2],
}
fn pools() -> [ByteBudget; 2] {
    [
        ByteBudget::new(2 * 1024 * 1024).expect("constant budget"),
        ByteBudget::default(),
    ]
}
impl Bridge {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            slots: Arc::new(Semaphore::new(TASKS)),
            stopped: CancellationToken::new(),
            drained: Notify::new(),
            send: pools(),
            window: pools(),
            chunk: pools(),
        })
    }
    pub fn snapshot(&self) -> BrowserResourceSnapshot {
        BrowserResourceSnapshot {
            active_requests: TASKS - self.slots.available_permits(),
            send_bytes: self.send.iter().map(ByteBudget::used).sum(),
            window_bytes: self.window.iter().map(ByteBudget::used).sum(),
            chunk_bytes: self.chunk.iter().map(ByteBudget::used).sum(),
        }
    }
    pub fn retire(&self) {
        self.stopped.cancel();
    }
    pub async fn close(&self, execution: &Execution) -> Result<()> {
        self.retire();
        execution
            .deadline_after(Duration::from_secs(5))
            .timeout(async {
                loop {
                    let changed = self.drained.notified();
                    tokio::pin!(changed);
                    changed.as_mut().enable();
                    if self.slots.available_permits() == TASKS {
                        return;
                    }
                    changed.await;
                }
            })
            .await
            .map_err(|_| ApiError::Backend("browser Fetch cleanup did not settle".into()))
    }
    // This synchronous boundary creates only Worker-local tasks. The returned
    // channels and cancellation guard contain no JavaScript values.
    pub fn start(self: &Arc<Self>, request: RequestData) -> Result<Pending> {
        if self.stopped.is_cancelled() {
            return Err(ApiError::ShuttingDown);
        }
        let slot = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Capacity)?;
        let lane = usize::from(request.class != OperationClass::Control);
        let send = self.send[lane].reserve(request.body.len())?;
        let local = LocalRequest::new(request)?;
        let stopped = self.stopped.child_token();
        let guard = CancelOnDrop(stopped.clone());
        let (head_tx, head) = oneshot::channel();
        let (pull_tx, pull_rx) = mpsc::channel(1);
        let lease = TaskLease {
            bridge: self.clone(),
            slot: Some(slot),
        };
        let window = self.window[lane].clone();
        let chunk = self.chunk[lane].clone();
        let started = Arc::new(AtomicBool::new(false));
        let dispatched = started.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let _lease = lease;
            let _send = send;
            local
                .run(head_tx, pull_rx, stopped, window, chunk, dispatched)
                .await;
        });
        let source = Box::pin(async_stream::try_stream! {
            let _guard = guard;
            loop {
                let (sender, receiver) = oneshot::channel();
                pull_tx.send(sender).await.map_err(|_| lost())?;
                let chunk: Option<RetainedBytes> = receiver.await.map_err(|_| lost())??;
                let Some(chunk) = chunk else { break; };
                yield chunk.into_bytes();
            }
        });
        Ok(Pending {
            head,
            source,
            started,
        })
    }
}
struct TaskLease {
    bridge: Arc<Bridge>,
    slot: Option<OwnedSemaphorePermit>,
}
impl Drop for TaskLease {
    fn drop(&mut self) {
        drop(self.slot.take());
        self.bridge.drained.notify_waiters();
    }
}
struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub(crate) struct RequestData {
    pub url: String,
    pub class: OperationClass,
    pub headers: Vec<(&'static str, String)>,
    pub authorization: Option<Zeroizing<String>>,
    pub body: RetainedBytes,
}
pub(crate) struct Head {
    pub status: u16,
    pub headers: http::HeaderMap,
}
pub(crate) struct Pending {
    pub head: oneshot::Receiver<Result<Head>>,
    pub source: ResponseBytes,
    pub started: Arc<AtomicBool>,
}
type Pull = oneshot::Sender<Result<Option<RetainedBytes>>>;

struct LocalRequest {
    worker: DedicatedWorkerGlobalScope,
    request: Request,
    controller: AbortController,
    url: String,
    // Retain the admitted explicit JS source copy through final local cleanup.
    _source: Uint8Array,
}
impl LocalRequest {
    fn new(data: RequestData) -> Result<Self> {
        let worker = worker()?;
        let controller = AbortController::new().map_err(|_| invalid())?;
        let headers = Headers::new().map_err(|_| invalid())?;
        for (name, value) in data.headers {
            headers.set(name, &value).map_err(|_| invalid())?;
        }
        if let Some(value) = data.authorization {
            headers
                .set("authorization", &value)
                .map_err(|_| invalid())?;
        }
        let source = Uint8Array::from(data.body.as_bytes());
        let options = RequestInit::new();
        options.set_method("POST");
        options.set_credentials(RequestCredentials::SameOrigin);
        options.set_mode(RequestMode::SameOrigin);
        options.set_redirect(RequestRedirect::Error);
        options.set_signal(Some(&controller.signal()));
        options.set_headers(headers.as_ref());
        // A missing body gives the server an exact zero size hint for login/logout.
        if !data.body.is_empty() {
            options.set_body_opt_u8_array(Some(&source));
        }
        let request = Request::new_with_str_and_init(&data.url, &options).map_err(|_| invalid())?;
        Ok(Self {
            worker,
            request,
            controller,
            url: data.url,
            _source: source,
        })
    }
    async fn run(
        self,
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
        let fetch = JsFuture::from(self.worker.fetch_with_request(&self.request)).fuse();
        futures_util::pin_mut!(fetch);
        let fetched = tokio::select! { biased;
            () = stopped.cancelled() => {
                self.controller.abort();
                fetch.await
            }
            value = &mut fetch => value,
        };
        let Ok(response) = fetched.and_then(JsCast::dyn_into::<Response>) else {
            let _ = head.send(Err(lost()));
            return;
        };
        let Some(body) = response.body() else {
            self.controller.abort();
            let _ = head.send(Err(invalid()));
            return;
        };
        let metadata = response_head(&response, &self.url);
        if stopped.is_cancelled() || metadata.is_err() {
            self.controller.abort();
            let _ = JsFuture::from(body.cancel()).await;
            let _ = head.send(Err(metadata.err().unwrap_or(ApiError::ShuttingDown)));
            return;
        }
        let Ok(reader) = ReadableStreamByobReader::new(&body) else {
            self.controller.abort();
            let _ = JsFuture::from(body.cancel()).await;
            let _ = head.send(Err(invalid()));
            return;
        };
        if head.send(metadata).is_ok() {
            let mut ended = false;
            loop {
                let pull = tokio::select! { biased;
                    () = stopped.cancelled() => break,
                    pull = pulls.recv() => match pull { Some(pull) => pull, None => break },
                };
                if ended {
                    let _ = pull.send(Ok(None));
                    break;
                }
                let result = read(&reader, &self.controller, &stopped, &windows, &chunks).await;
                let failed = result.is_err();
                let result = result.map(|(chunk, done)| {
                    ended = done;
                    chunk
                });
                if pull.send(result).is_err() || failed {
                    break;
                }
            }
        }
        self.controller.abort();
        let _ = JsFuture::from(reader.cancel()).await;
        reader.release_lock();
    }
}

async fn read(
    reader: &ReadableStreamByobReader,
    controller: &AbortController,
    stopped: &CancellationToken,
    windows: &ByteBudget,
    chunks: &ByteBudget,
) -> Result<(Option<RetainedBytes>, bool)> {
    let _window: ByteReservation = windows.reserve(WINDOW as usize)?;
    let view = Uint8Array::new_with_length(WINDOW);
    let pending = JsFuture::from(reader.read_with_array_buffer_view(view.as_ref())).fuse();
    futures_util::pin_mut!(pending);
    let result = tokio::select! { biased;
        () = stopped.cancelled() => {
            controller.abort();
            let cancel = JsFuture::from(reader.cancel());
            let _ = futures_util::future::join(pending, cancel).await;
            return Err(ApiError::ShuttingDown);
        }
        result = &mut pending => result.map_err(|_| lost())?,
    };
    let done = Reflect::get(&result, &JsValue::from_str("done"))
        .map_err(|_| invalid())?
        .as_bool()
        .ok_or_else(invalid)?;
    let value = Reflect::get(&result, &JsValue::from_str("value")).map_err(|_| invalid())?;
    if done && value.is_undefined() {
        return Ok((None, true));
    }
    let value = value.dyn_into::<Uint8Array>().map_err(|_| invalid())?;
    if value.length() > WINDOW || value.buffer().byte_length() > WINDOW {
        return Err(invalid());
    }
    if value.length() == 0 {
        return if done {
            Ok((None, true))
        } else {
            Err(invalid())
        };
    }
    let capacity = chunks.reserve(value.length() as usize)?;
    let mut bytes = vec![0; value.length() as usize];
    value.copy_to(&mut bytes);
    Ok((Some(capacity.retain_vec(bytes)?), done))
}

fn response_head(response: &Response, url: &str) -> Result<Head> {
    if bounded_string(
        Reflect::get(response.as_ref(), &JsValue::from_str("url")).map_err(|_| invalid())?,
        2048,
    )? != url
        || response.redirected()
    {
        return Err(invalid());
    }
    let headers = response.headers();
    let get = Reflect::get(headers.as_ref(), &JsValue::from_str("get"))
        .map_err(|_| invalid())?
        .dyn_into::<Function>()
        .map_err(|_| invalid())?;
    let mut values = http::HeaderMap::new();
    for name in [
        "x-rsi-wire-version",
        "x-rsi-endpoint-id",
        "x-rsi-host-epoch",
        "content-type",
        "content-length",
        "content-encoding",
        "x-rsi-http-version",
    ] {
        let value = get
            .call1(headers.as_ref(), &JsValue::from_str(name))
            .map_err(|_| invalid())?;
        if value.is_null() {
            continue;
        }
        let value = bounded_string(value, 1024)?;
        values.insert(
            name,
            http::HeaderValue::from_str(&value).map_err(|_| invalid())?,
        );
    }
    Ok(Head {
        status: response.status(),
        headers: values,
    })
}
fn bounded_string(value: JsValue, maximum: u32) -> Result<String> {
    let text = value.dyn_into::<JsString>().map_err(|_| invalid())?;
    if text.length() > maximum {
        return Err(invalid());
    }
    text.as_string().ok_or_else(invalid)
}
fn worker() -> Result<DedicatedWorkerGlobalScope> {
    js_sys::global()
        .dyn_into()
        .map_err(|_| ApiError::Invalid("API Fetch requires a Dedicated Worker".into()))
}
pub(crate) fn origin(allow_http: bool) -> Result<String> {
    let origin = bounded_string(
        Reflect::get(worker()?.as_ref(), &JsValue::from_str("origin")).map_err(|_| invalid())?,
        2048,
    )?;
    // Worker origin is platform-canonicalized. Restrict the only development
    // exception to these exact authorities, with an optional numeric port.
    if !allow_http && origin.starts_with("https://") {
        return Ok(origin);
    }
    if allow_http && let Some(authority) = origin.strip_prefix("http://") {
        for host in ["127.0.0.1", "localhost", "[::1]"] {
            if authority == host
                || authority.strip_prefix(host).is_some_and(|port| {
                    port.strip_prefix(':').is_some_and(|port| {
                        !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit())
                    })
                })
            {
                return Ok(origin);
            }
        }
    }
    Err(ApiError::Invalid(
        "HTTPS or explicit loopback HTTP Worker origin is required".into(),
    ))
}
pub(crate) fn invalid() -> ApiError {
    ApiError::Invalid("invalid browser API response".into())
}
pub(crate) fn lost() -> ApiError {
    ApiError::Backend("browser API response was lost".into())
}
