//! Authenticated streaming routes belonging to an application process.
use crate::server::{Body, State};
use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Limited, StreamBody};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use rsi_api_protocol::{ApiError, AuthenticatedDevice, CallOrigin, Result};
use rsi_credentials_protocol::SecretValue;
use std::{io, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{net::TcpStream, sync::Semaphore, time::timeout};
use tokio_util::sync::CancellationToken;

const MAX_BODY: usize = 64 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(15);

/// Resident listeners select one concrete process per incoming request.
#[derive(Debug)]
pub struct ApplicationRoutes(std::sync::Mutex<Arc<ApplicationUpstream>>);

impl ApplicationRoutes {
    /// Starts with the selected process, which may still be preparing.
    pub fn new(process: Arc<ApplicationUpstream>) -> Self {
        Self(std::sync::Mutex::new(process))
    }

    pub(crate) fn current(&self) -> Arc<ApplicationUpstream> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Replaces the process and retires its outstanding request handles.
    pub fn replace(&self, next: Arc<ApplicationUpstream>) {
        let mut current = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        current.retired.cancel();
        let previous = std::mem::replace(&mut *current, next);
        drop(current);
        previous.retire();
    }
}
/// Cache policy admitted by the application forwarder.
#[derive(Clone)]
pub(super) struct ApplicationCache(pub HeaderValue);

fn code_cache(headers: &HeaderMap, status: StatusCode) -> Option<ApplicationCache> {
    let mime = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(str::trim);
    let code = matches!(
        mime,
        Some("text/javascript" | "application/javascript" | "text/css")
    );
    if !(status == StatusCode::OK && code || status == StatusCode::NOT_MODIFIED) {
        return None;
    }
    let mut directives = Vec::new();
    for value in headers.get_all("cache-control") {
        let value = value.to_str().ok()?;
        if value.contains('"') {
            return None;
        }
        directives.extend(value.split(',').map(str::trim));
    }
    if directives
        .iter()
        .any(|d| d.eq_ignore_ascii_case("no-store"))
    {
        return None;
    }
    if headers.contains_key("etag")
        && directives
            .iter()
            .any(|d| d.eq_ignore_ascii_case("no-cache"))
    {
        return Some(ApplicationCache(HeaderValue::from_static(
            "private, no-cache",
        )));
    }
    if !code {
        return None;
    }
    if !["public", "immutable"]
        .iter()
        .all(|required| directives.iter().any(|d| d.eq_ignore_ascii_case(required)))
    {
        return None;
    }
    directives.retain(|d| !d.eq_ignore_ascii_case("public"));
    directives.push("private");
    Some(ApplicationCache(directives.join(", ").parse().ok()?))
}

/// Private process listener with explicitly assigned, disjoint route prefixes.
/// The secret authenticates the native forwarder to the application's listener.
/// The owner must cancel `retired` before releasing the process or its leases.
#[derive(Debug)]
pub struct ApplicationUpstream {
    endpoint: tokio::sync::watch::Sender<Option<(SocketAddr, Vec<String>)>>,
    secret: SecretValue,
    prefixes: Vec<String>,
    retired: CancellationToken,
    slots: Arc<Semaphore>,
    requests: Requests,
}
type Requests = Arc<std::sync::Mutex<std::collections::BTreeMap<String, AppRequest>>>;
#[derive(Debug)]
struct AppRequest {
    device: AuthenticatedDevice,
    resources: std::collections::BTreeMap<String, RequestResource>,
}
trait ResourceValue: std::fmt::Debug + Send + Sync + std::any::Any {}
impl<T: std::fmt::Debug + Send + Sync + 'static> ResourceValue for T {}
#[derive(Debug)]
struct RequestResource {
    value: Arc<dyn ResourceValue>,
    retired: CancellationToken,
}
impl Drop for RequestResource {
    fn drop(&mut self) {
        self.retired.cancel();
    }
}
/// An explicit device callback lifetime owned by native application composition.
/// Its opaque identity is invalid after this guard or the upstream retires.
pub struct ApplicationCallback {
    id: String,
    requests: Requests,
    _permit: tokio::sync::OwnedSemaphorePermit,
}
impl std::fmt::Debug for ApplicationCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApplicationCallback")
            .finish_non_exhaustive()
    }
}
impl ApplicationCallback {
    /// Opaque callback identity for the selected application process.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}
impl Drop for ApplicationCallback {
    fn drop(&mut self) {
        self.requests
            .lock()
            .expect("application requests")
            .remove(&self.id);
    }
}

impl ApplicationUpstream {
    /// Whether this process completed its route handshake.
    #[must_use]
    pub fn published(&self) -> bool {
        self.endpoint.borrow().is_some()
    }

    /// Withdraws this process's sockets, callbacks and their retained resources.
    pub fn retire(&self) {
        self.retired.cancel();
        let resources = std::mem::take(
            &mut *self
                .requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        drop(resources);
    }
    /// Binds prefix ownership to a concrete loopback endpoint for one lifetime.
    pub fn new(
        address: SocketAddr,
        secret: SecretValue,
        prefixes: Vec<String>,
        retired: CancellationToken,
    ) -> Result<Self> {
        let upstream = Self::preparing(secret, Vec::new(), retired)?;
        upstream.publish(address, prefixes)?;
        Ok(upstream)
    }
    /// Allows declared application requests to wait while immutable packages prepare.
    pub fn preparing(
        secret: SecretValue,
        prefixes: Vec<String>,
        retired: CancellationToken,
    ) -> Result<Self> {
        if secret.expose_secret().len() != 64
            || !secret
                .expose_secret()
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
            || prefixes.len() > 64
            || prefixes.iter().any(|p| !valid_prefix(p))
        {
            return Err(ApiError::Invalid(
                "invalid private application listener".into(),
            ));
        }
        Ok(Self {
            endpoint: tokio::sync::watch::channel(None).0,
            secret,
            prefixes,
            retired,
            slots: Arc::new(Semaphore::new(32)),
            requests: Arc::default(),
        })
    }
    /// Resolves a live native request handle for an application callback.
    pub fn caller(&self, id: &str) -> Result<CallOrigin> {
        if id.len() != 64 || self.retired.is_cancelled() {
            return Err(ApiError::Unauthorized);
        }
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .filter(|request| !request.device.revoked.is_cancelled())
            .map(|request| request.device.clone())
            .map(CallOrigin::Device)
            .ok_or(ApiError::Unauthorized)
    }
    /// Holds a process-local resource until explicit release or physical request retirement.
    ///
    /// # Errors
    /// Returns an error for a retired request, a reused key, or a full request.
    pub fn retain_resource<T: std::fmt::Debug + Send + Sync + 'static>(
        &self,
        id: &str,
        key: String,
        resource: Arc<T>,
    ) -> Result<()> {
        if key.is_empty() || key.len() > 256 {
            return Err(ApiError::Invalid("application resource key".into()));
        }
        let mut requests = self
            .requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let request = requests
            .get_mut(id)
            .filter(|r| !r.device.revoked.is_cancelled() && !self.retired.is_cancelled())
            .ok_or(ApiError::Unauthorized)?;
        if request.resources.len() >= 64 || request.resources.contains_key(&key) {
            return Err(ApiError::Capacity);
        }
        request.resources.insert(
            key,
            RequestResource {
                value: resource,
                retired: request.device.revoked.child_token(),
            },
        );
        Ok(())
    }
    /// Borrows a typed shared resource while the originating device and request remain live.
    ///
    /// # Errors
    /// Returns an error for a retired request, an absent key or a mismatched resource type.
    pub fn shared_resource<T: std::fmt::Debug + Send + Sync + 'static>(
        &self,
        id: &str,
        key: &str,
    ) -> Result<(Arc<T>, CancellationToken)> {
        let requests = self
            .requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let request = requests
            .get(id)
            .filter(|request| {
                !request.device.revoked.is_cancelled() && !self.retired.is_cancelled()
            })
            .ok_or(ApiError::Unauthorized)?;
        let held = request.resources.get(key).ok_or(ApiError::Unavailable)?;
        let erased: Arc<dyn std::any::Any + Send + Sync> = held.value.clone();
        Ok((
            erased.downcast::<T>().map_err(|_| ApiError::Unavailable)?,
            held.retired.clone(),
        ))
    }
    /// Idempotently releases only a resource held by this exact request.
    pub fn release_resource(&self, id: &str, key: &str) {
        let resource = self
            .requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(id)
            .and_then(|r| r.resources.remove(key));
        drop(resource);
    }
    /// Retains an explicitly supplied device principal until the returned guard retires.
    /// The caller owns the principal and its cancellation policy; this grants no local authority.
    pub fn open_callback(&self, device: &AuthenticatedDevice) -> Result<ApplicationCallback> {
        if device.revoked.is_cancelled() || self.retired.is_cancelled() {
            return Err(ApiError::Unauthorized);
        }
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Capacity)?;
        let mut random = [0; 32];
        getrandom::fill(&mut random).map_err(|_| ApiError::Unavailable)?;
        let id = hex::encode(random);
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                id.clone(),
                AppRequest {
                    device: device.clone(),
                    resources: std::collections::BTreeMap::new(),
                },
            );
        Ok(ApplicationCallback {
            id,
            requests: self.requests.clone(),
            _permit: permit,
        })
    }
    /// Publishes the actual private endpoint once the application is listening.
    pub fn publish(&self, address: SocketAddr, prefixes: Vec<String>) -> Result<()> {
        if !address.ip().is_loopback()
            || address.port() == 0
            || prefixes.is_empty()
            || prefixes.len() > 64
            || prefixes.iter().any(|p| !valid_prefix(p))
            || self.prefixes.iter().any(|p| !prefixes.contains(p))
        {
            return Err(ApiError::Invalid("invalid application routes".into()));
        }
        if !self.endpoint.send_if_modified(|endpoint| {
            if endpoint.is_some() {
                return false;
            }
            *endpoint = Some((address, prefixes));
            true
        }) {
            return Err(ApiError::Invalid(
                "application endpoint already published".into(),
            ));
        }
        Ok(())
    }
    pub(super) fn owns(&self, path: &str) -> bool {
        if let Some(version) = path.strip_prefix("/api/v")
            && version
                .split('/')
                .next()
                .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        {
            return false;
        }
        let published = self.endpoint.borrow();
        let prefixes = published.as_ref().map_or(&self.prefixes, |(_, p)| p);
        prefixes.iter().any(|p| {
            path == p
                || path
                    .strip_prefix(p)
                    .is_some_and(|tail| tail.starts_with('/'))
        })
    }
    fn prepare_request(
        &self,
        parts: &mut http::request::Parts,
        state: &State,
        upgrade: bool,
    ) -> Result<()> {
        let path = parts
            .uri
            .path_and_query()
            .ok_or(ApiError::Unauthorized)?
            .clone();
        parts.uri = http::Uri::builder()
            .path_and_query(path)
            .build()
            .map_err(|_| ApiError::Unauthorized)?;
        parts.version = http::Version::HTTP_11;
        strip_hop_headers(&mut parts.headers)?;
        parts.headers.remove("cookie");
        parts.headers.remove("authorization");
        let internal: Vec<_> = parts
            .headers
            .keys()
            .filter(|name| {
                name.as_str().starts_with("x-rsi-")
                    || name.as_str().starts_with("x-forwarded-")
                    || name.as_str() == "forwarded"
            })
            .cloned()
            .collect();
        for name in internal {
            parts.headers.remove(name);
        }
        parts.headers.insert(
            "x-rsi-upstream-key",
            self.secret
                .expose_secret()
                .parse()
                .map_err(|_| ApiError::Unavailable)?,
        );
        if let crate::access::Access::Remote { policy, .. } = &state.access {
            parts.headers.insert(
                "host",
                policy
                    .authority
                    .parse()
                    .map_err(|_| ApiError::Unavailable)?,
            );
        }
        if upgrade {
            parts
                .headers
                .insert("connection", HeaderValue::from_static("upgrade"));
            parts
                .headers
                .insert("upgrade", HeaderValue::from_static("websocket"));
        }
        Ok(())
    }
    pub(super) async fn forward(
        &self,
        state: &State,
        mut incoming: Request<Incoming>,
    ) -> Result<Response<Body>> {
        if self.retired.is_cancelled() {
            return Err(ApiError::ShuttingDown);
        }
        let upgrade = incoming.headers().contains_key("upgrade");
        if upgrade
            && (incoming.method() != Method::GET
                || incoming.version() != http::Version::HTTP_11
                || crate::policy::one(incoming.headers(), "upgrade")?
                    .is_none_or(|v| !v.eq_ignore_ascii_case("websocket")))
        {
            return Err(ApiError::Unauthorized);
        }
        let safe_read = !upgrade && matches!(*incoming.method(), Method::GET | Method::HEAD);
        let mut device = state
            .access
            .application_device(incoming.headers(), safe_read, upgrade)?;
        state.bind_browser(&mut device).await?;
        let slot = self.open_callback(&device)?;
        let client_upgrade = upgrade.then(|| hyper::upgrade::on(&mut incoming));
        let (mut parts, body) = incoming.into_parts();
        self.prepare_request(&mut parts, state, upgrade)?;
        parts.headers.insert(
            "x-rsi-upstream-request",
            slot.id.parse().map_err(|_| ApiError::Unavailable)?,
        );
        let mut endpoint = self.endpoint.subscribe();
        let address = tokio::select! {biased;
            ()=self.retired.cancelled()=>return Err(ApiError::ShuttingDown),
            ()=device.revoked.cancelled()=>return Err(ApiError::Unauthorized),
            ready=endpoint.wait_for(Option::is_some)=>ready.map_err(|_|ApiError::Unavailable)?.as_ref().ok_or(ApiError::Unavailable)?.0,
        };
        let socket = timeout(TIMEOUT, TcpStream::connect(address))
            .await
            .map_err(|_| ApiError::Unavailable)?
            .map_err(|_| ApiError::Unavailable)?;
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
            .await
            .map_err(|_| ApiError::Unavailable)?;
        let retired = self.retired.clone();
        let revoked = device.revoked.clone();
        drop(state.execution.spawn(async move {
            tokio::select!{ biased; ()=retired.cancelled()=>{}, ()=revoked.cancelled()=>{}, _=connection.with_upgrades()=>{} }
        }));
        let request = Request::from_parts(parts, Limited::new(body, MAX_BODY));
        let mut response = tokio::select! {
            biased;
            ()=self.retired.cancelled()=>return Err(ApiError::ShuttingDown),
            ()=device.revoked.cancelled()=>return Err(ApiError::Unauthorized),
            reply=sender.send_request(request)=>reply.map_err(|_|ApiError::Unavailable)?,
        };
        let switching = response.status() == StatusCode::SWITCHING_PROTOCOLS;
        if switching && !upgrade {
            return Err(ApiError::Unavailable);
        }
        prepare_response(&mut response, safe_read)?;
        if switching {
            let peer = hyper::upgrade::on(&mut response);
            let client = client_upgrade.ok_or(ApiError::Unauthorized)?;
            let retired = self.retired.clone();
            let revoked = device.revoked;
            drop(state.execution.spawn(async move {
                let _slot=slot;
                tokio::select! {
                    biased; ()=retired.cancelled()=>{}, ()=revoked.cancelled()=>{},
                    _=async {
                        let (a,b)=tokio::try_join!(client,peer)?;
                        let _=tokio::io::copy_bidirectional(&mut TokioIo::new(a),&mut TokioIo::new(b)).await;
                        Ok::<_,hyper::Error>(())
                    }=>{}
                }
            }));
            response
                .headers_mut()
                .insert("connection", HeaderValue::from_static("upgrade"));
            response
                .headers_mut()
                .insert("upgrade", HeaderValue::from_static("websocket"));
            return Ok(response.map(|_| crate::server::full(Bytes::new())));
        }
        let retired = self.retired.clone();
        let revoked = device.revoked;
        Ok(response.map(|body| {
            let mut body=Limited::new(body,MAX_BODY);
            let stream=async_stream::stream!{
                let _slot=slot;
                loop {
                    let frame=tokio::select! {biased;
                        ()=retired.cancelled()=>{yield Err(io::Error::other("application retired"));break;},
                        ()=revoked.cancelled()=>{yield Err(io::Error::other("device revoked"));break;},
                        frame=body.frame()=>frame,
                    };
                    let Some(frame)=frame else {break};
                    yield frame.map_err(io::Error::other);
                }
            };
            StreamBody::new(stream).boxed_unsync()
        }))
    }
}

fn prepare_response<B>(response: &mut Response<B>, safe_read: bool) -> Result<()> {
    strip_hop_headers(response.headers_mut())?;
    response.headers_mut().remove("set-cookie");
    if safe_read && let Some(cache) = code_cache(response.headers(), response.status()) {
        response.extensions_mut().insert(cache);
    }
    Ok(())
}

fn valid_prefix(path: &str) -> bool {
    path.len() > 1
        && path.len() <= 128
        && path.starts_with('/')
        && !path.ends_with('/')
        && !path.split('/').skip(1).any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}

fn strip_hop_headers(headers: &mut HeaderMap) -> Result<()> {
    let mut nominated = Vec::new();
    for value in headers.get_all("connection") {
        for name in value
            .to_str()
            .map_err(|_| ApiError::Unauthorized)?
            .split(',')
        {
            let name = http::HeaderName::from_bytes(name.trim().as_bytes())
                .map_err(|_| ApiError::Unauthorized)?;
            if matches!(name.as_str(), "host" | "origin" | "sec-fetch-site") {
                return Err(ApiError::Unauthorized);
            }
            nominated.push(name);
        }
    }
    for name in nominated {
        headers.remove(name);
    }
    for name in [
        "connection",
        "proxy-authorization",
        "proxy-authenticate",
        "keep-alive",
        "transfer-encoding",
        "upgrade",
        "te",
        "trailer",
    ] {
        headers.remove(name);
    }
    Ok(())
}
#[cfg(test)]
mod request_resource_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct Count(Arc<AtomicUsize>);
    impl Drop for Count {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn replacing_a_process_keeps_the_browser_and_retires_every_old_request() {
        let device = AuthenticatedDevice {
            id: rsi_api_protocol::DeviceId::generate().unwrap(),
            revoked: CancellationToken::new(),
        };
        let process = |key: &str| {
            Arc::new(
                ApplicationUpstream::preparing(
                    SecretValue::new(key.repeat(64)).unwrap(),
                    vec!["/api".into()],
                    CancellationToken::new(),
                )
                .unwrap(),
            )
        };
        let old = process("a");
        let routes = ApplicationRoutes::new(old.clone());
        let request = old.open_callback(&device).unwrap();
        old.retain_resource(&request.id, "wait".into(), Arc::new(8u64))
            .unwrap();
        let (_, retired) = old.shared_resource::<u64>(&request.id, "wait").unwrap();
        let next = process("b");
        routes.replace(next.clone());
        assert!(retired.is_cancelled());
        assert!(old.caller(&request.id).is_err());
        assert!(next.caller(&request.id).is_err());
        let fresh = routes.current().open_callback(&device).unwrap();
        assert!(next.caller(&fresh.id).is_ok());
    }

    #[test]
    fn a_physical_request_owns_only_its_resources_and_releases_them_on_retirement() {
        let upstream = ApplicationUpstream::new(
            ([127, 0, 0, 1], 8123).into(),
            SecretValue::new("a".repeat(64)).unwrap(),
            vec!["/api".into()],
            CancellationToken::new(),
        )
        .unwrap();
        let device = AuthenticatedDevice {
            id: rsi_api_protocol::DeviceId::generate().unwrap(),
            revoked: CancellationToken::new(),
        };
        let first = upstream.open_callback(&device).unwrap();
        let other = upstream.open_callback(&device).unwrap();
        upstream
            .retain_resource(&first.id, "observer".into(), Arc::new(17u64))
            .unwrap();
        let (observer, lifetime) = upstream
            .shared_resource::<u64>(&first.id, "observer")
            .unwrap();
        assert_eq!(*observer, 17);
        assert!(
            upstream
                .shared_resource::<String>(&first.id, "observer")
                .is_err()
        );
        assert!(
            upstream
                .shared_resource::<u64>(&other.id, "observer")
                .is_err()
        );
        upstream.release_resource(&first.id, "observer");
        assert!(lifetime.is_cancelled());
        let count = Arc::new(AtomicUsize::new(0));
        for key in ["one", "two"] {
            upstream
                .retain_resource(&first.id, key.into(), Arc::new(Count(count.clone())))
                .unwrap();
        }
        upstream.release_resource(&other.id, "one");
        assert_eq!(count.load(Ordering::SeqCst), 0);
        upstream.release_resource(&first.id, "one");
        upstream.release_resource(&first.id, "one");
        assert_eq!(count.load(Ordering::SeqCst), 1);
        drop(first);
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }
    #[test]
    fn an_application_callback_outlives_browser_credentials_but_not_its_owner() {
        let upstream = ApplicationUpstream::preparing(
            SecretValue::new("a".repeat(64)).unwrap(),
            vec![],
            CancellationToken::new(),
        )
        .unwrap();
        let owner = AuthenticatedDevice {
            id: rsi_api_protocol::DeviceId::generate().unwrap(),
            revoked: CancellationToken::new(),
        };
        let browser = AuthenticatedDevice {
            id: owner.id.clone(),
            revoked: CancellationToken::new(),
        };
        let background = upstream.open_callback(&owner).unwrap();
        let request = upstream.open_callback(&browser).unwrap();
        let request_id = request.id().to_owned();
        upstream
            .retain_resource(background.id(), "observer".into(), Arc::new(17u64))
            .unwrap();
        let (observer, stopped) = upstream
            .shared_resource::<u64>(background.id(), "observer")
            .unwrap();
        assert_eq!(*observer, 17);
        browser.revoked.cancel();
        assert!(upstream.caller(&request_id).is_err());
        drop(request);
        assert!(matches!(upstream.caller(background.id()).unwrap(),
            CallOrigin::Device(device) if device.id == owner.id));
        assert!(!stopped.is_cancelled());
        owner.revoked.cancel();
        assert!(stopped.is_cancelled());
        assert!(upstream.caller(background.id()).is_err());
        assert!(upstream.open_callback(&owner).is_err());
        let id = background.id().to_owned();
        drop(background);
        assert!(upstream.shared_resource::<u64>(&id, "observer").is_err());
    }
}
