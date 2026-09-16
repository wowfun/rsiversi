use crate::{
    RetrievalConfig, RetrievalError, RetrievalOperation, RetrievalResult, RetrievedSource, decode,
    exa_credential, network, safe_url,
};
use rsi_credentials_protocol::CredentialsResolve;
use rsi_settings_protocol::SettingsScope;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
type Result<T> = std::result::Result<T, RetrievalError>;

/// Per-operation work whose admission remains held by real blocking work.
#[derive(Clone, Debug)]
pub(crate) struct Work {
    pub(crate) dns: Result<Arc<hickory_resolver::TokioResolver>>,
    stop: CancellationToken,
    tasks: TaskTracker,
    _permit: Arc<OwnedSemaphorePermit>,
}
impl Work {
    pub(crate) async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(CancellationToken) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let retained = self.clone();
        self.tasks
            .spawn_blocking(move || {
                // Capture the whole owner so admission survives caller cancellation.
                let work = retained;
                if work.stop.is_cancelled() {
                    return Err(RetrievalError::Cancelled);
                }
                f(work.stop.clone())
            })
            .await
            .map_err(|_| RetrievalError::WorkerFailed)?
    }
}
#[derive(Debug)]
struct Cancel(CancellationToken);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
/// Settings-aware service. No request is made by configuration or history reads.
#[derive(Debug)]
pub struct RetrievalService {
    pub(crate) dns: Result<Arc<hickory_resolver::TokioResolver>>,
    pub(crate) settings: Arc<dyn SettingsScope>,
    pub(crate) credentials: Arc<dyn CredentialsResolve>,
    pub(crate) permits: Arc<Semaphore>,
    pub(crate) stop: CancellationToken,
    pub(crate) tasks: TaskTracker,
}
impl RetrievalService {
    /// Synchronous flags for pre-seal composition capture and current authorization.
    ///
    /// # Errors
    /// Returns a closed failure when current Settings cannot be decoded.
    pub fn config(&self) -> Result<RetrievalConfig> {
        serde_json::from_value(
            self.settings
                .get()
                .map_err(|_| RetrievalError::Configuration)?
                .value,
        )
        .map_err(|_| RetrievalError::Configuration)
    }
    /// Fetches public HTTP/S text under the current enabled flag and fixed bounds.
    ///
    /// # Errors
    /// Rejects disabled access, invalid or nonpublic URLs, cancellation, timeouts and bounded network/decoding failures.
    pub async fn fetch(
        self: &Arc<Self>,
        url: String,
        cancellation: CancellationToken,
    ) -> Result<RetrievalResult> {
        self.run(RetrievalOperation::Fetch, url, 1, cancellation)
            .await
    }
    /// Searches Exa directly. `None` means five results; requests above ten fail.
    ///
    /// # Errors
    /// Rejects disabled access, invalid queries, unavailable credentials, cancellation and bounded provider failures.
    pub async fn search(
        self: &Arc<Self>,
        query: String,
        maximum: Option<u8>,
        cancellation: CancellationToken,
    ) -> Result<RetrievalResult> {
        self.run(
            RetrievalOperation::Search,
            query,
            maximum.unwrap_or(5),
            cancellation,
        )
        .await
    }
    async fn run(
        self: &Arc<Self>,
        operation: RetrievalOperation,
        request: String,
        maximum: u8,
        cancellation: CancellationToken,
    ) -> Result<RetrievalResult> {
        let config = self.config()?;
        if !(match operation {
            RetrievalOperation::Fetch => config.web_fetch,
            RetrievalOperation::Search => config.web_search,
        }) {
            return Err(RetrievalError::Disabled);
        }
        if request.trim().is_empty() || request.len() > 8192 || !(1..=10).contains(&maximum) {
            return Err(RetrievalError::InvalidInput);
        }
        if operation == RetrievalOperation::Fetch {
            network::parse_url(&request)?;
        }
        if cancellation.is_cancelled() || self.stop.is_cancelled() {
            return Err(RetrievalError::Cancelled);
        }
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| RetrievalError::Busy)?;
        let stop = self.stop.child_token();
        let guard = Cancel(stop.clone());
        let work = Work {
            dns: self.dns.clone(),
            stop: stop.clone(),
            tasks: self.tasks.clone(),
            _permit: Arc::new(permit),
        };
        let active = self.clone();
        let (send, receive) = oneshot::channel();
        self.tasks.spawn(async move {
            let timeout = tokio::time::sleep(std::time::Duration::from_secs(30));
            let result = tokio::select! {
                biased;
                () = stop.cancelled() => Err(RetrievalError::Cancelled),
                () = cancellation.cancelled() => Err(RetrievalError::Cancelled),
                () = timeout => Err(RetrievalError::Timeout),
                result = active.execute(operation,request,maximum,&work) => result,
            };
            stop.cancel();
            let _ = send.send(result);
        });
        let result = receive.await.map_err(|_| RetrievalError::WorkerFailed)?;
        drop(guard);
        result
    }
    async fn execute(
        &self,
        operation: RetrievalOperation,
        request: String,
        maximum: u8,
        work: &Work,
    ) -> Result<RetrievalResult> {
        let result = match operation {
            RetrievalOperation::Fetch => {
                let body = network::fetch(&request, work).await?;
                let url = body.url.to_string();
                let text = work
                    .blocking(move |stop| decode::extract(&body, &stop))
                    .await?;
                RetrievalResult {
                    version: 1,
                    operation,
                    request,
                    omitted: 0,
                    truncated: text.truncated,
                    sources: vec![RetrievedSource {
                        url,
                        title: text.title,
                        text: text.text,
                        published_at: None,
                        truncated: text.truncated,
                    }],
                }
            }
            RetrievalOperation::Search => {
                let key = self
                    .credentials
                    .resolve(&exa_credential())
                    .await
                    .map_err(|_| RetrievalError::MissingCredential)?;
                let mut header = reqwest::header::HeaderValue::from_str(&format!(
                    "Bearer {}",
                    key.secret.expose_secret()
                ))
                .map_err(|_| RetrievalError::MissingCredential)?;
                header.set_sensitive(true);
                let body = network::exa(header, search_request(&request, maximum), work).await?;
                work.blocking(move |stop| {
                    let media = body.media.split(';').next().unwrap_or_default().trim();
                    if media != "application/json" && !media.ends_with("+json") {
                        return Err(RetrievalError::Protocol);
                    }
                    let text = decode::decode(&body, &stop)?;
                    normalize_search(request, maximum, &text)
                })
                .await?
            }
        };
        result.validate()?;
        Ok(result)
    }
    /// Stops callers and joins all actual asynchronous and blocking work.
    pub async fn shutdown(&self) {
        self.stop.cancel();
        self.tasks.close();
        self.tasks.wait().await;
    }
}
fn search_request(query: &str, maximum: u8) -> Vec<u8> {
    serde_json::to_vec(&json!({"query":query,"type":"auto","numResults":maximum,"contents":{"highlights":{"highlightsPerUrl":1}}})).expect("bounded search request")
}
fn clipped(value: &str, maximum: usize, truncated: &mut bool) -> String {
    let end = value.floor_char_boundary(maximum.min(value.len()));
    *truncated |= end < value.len();
    value[..end].to_owned()
}
fn normalize_search(request: String, maximum: u8, text: &str) -> Result<RetrievalResult> {
    let value: Value = serde_json::from_str(text).map_err(|_| RetrievalError::Protocol)?;
    let results = value
        .get("results")
        .and_then(Value::as_array)
        .filter(|list| list.len() <= 1024)
        .ok_or(RetrievalError::Protocol)?;
    let mut result = RetrievalResult {
        version: 1,
        operation: RetrievalOperation::Search,
        request,
        sources: vec![],
        omitted: 0,
        truncated: false,
    };
    for entry in results {
        let Some(url) = entry
            .get("url")
            .and_then(Value::as_str)
            .filter(|url| safe_url(url))
        else {
            result.omitted += 1;
            continue;
        };
        let highlights = match entry.get("highlights") {
            None | Some(Value::Null) => None,
            Some(Value::Array(list)) if list.iter().all(Value::is_string) => list
                .iter()
                .filter_map(Value::as_str)
                .find(|text| !text.trim().is_empty()),
            _ => return Err(RetrievalError::Protocol),
        };
        let Some(highlight) = highlights else {
            result.omitted += 1;
            continue;
        };
        if result.sources.len() >= usize::from(maximum) {
            result.truncated = true;
            continue;
        }
        let mut truncated = false;
        let mut field = |name, maximum| -> Result<Option<String>> {
            match entry.get(name) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(value)) => Ok(Some(clipped(value, maximum, &mut truncated))),
                _ => Err(RetrievalError::Protocol),
            }
        };
        let title = field("title", 1024)?.unwrap_or_default();
        let published_at = field("publishedDate", 128)?;
        let text = clipped(highlight, 8 * 1024, &mut truncated);
        result.truncated |= truncated;
        result.sources.push(RetrievedSource {
            url: url.into(),
            title,
            text,
            published_at,
            truncated,
        });
    }
    result.validate()?;
    Ok(result)
}

#[cfg(test)]
mod tests;
