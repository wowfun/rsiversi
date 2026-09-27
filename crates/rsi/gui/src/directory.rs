use crate::{
    GuiApplication,
    application::{Result, error},
};
use futures_util::future::BoxFuture;
use rsi_directory_picker_api::{CreateRequest, ListRequest};
use serde::Deserialize;
use std::sync::Arc;
fn reply<T: serde::Serialize>(
    value: rsi_directory_picker_api::Result<T>,
) -> serde_json::Result<serde_json::Value> {
    match value {
        Ok(value) => serde_json::to_value(value)
            .map(|value| serde_json::json!({"status":"ok","value":value})),
        Err(failure) => Ok(
            serde_json::json!({"status":"failed","message":failure.to_string(),"failure":failure}),
        ),
    }
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Status,
    List {
        path: Option<String>,
        request_id: String,
    },
    Cancel {
        request_id: String,
    },
    Create {
        parent: String,
        name: String,
    },
}
struct ReadLease {
    app: Arc<GuiApplication>,
    id: String,
}
impl Drop for ReadLease {
    fn drop(&mut self) {
        self.app
            .directory_reads
            .lock()
            .expect("directory reads poisoned")
            .remove(&self.id);
    }
}
impl GuiApplication {
    /// Forwards bounded directory operations through the composed shared API client.
    ///
    /// # Panics
    /// Panics if a prior panic poisoned directory read ownership.
    pub fn directory_input(self: &Arc<Self>, source: &str) -> BoxFuture<'static, Result<String>> {
        let request = (|| {
            if source.len() > 128 * 1024 {
                return Err("Directory request exceeds its limit".into());
            }
            serde_json::from_str::<Request>(source).map_err(error)
        })();
        let request = match request {
            Ok(request) => request,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        if let Request::Cancel { request_id } = &request {
            if let Some(stop) = self
                .directory_reads
                .lock()
                .expect("directory reads poisoned")
                .get(request_id)
            {
                stop.cancel();
            }
            return Box::pin(async { Ok("null".into()) });
        }
        self.admit(false, None, move |app| async move {
            let client = app
                .directory
                .as_ref()
                .ok_or("Directory browsing is unavailable; enter a path manually")?;
            let value = match request {
                Request::Status => reply(client.status().await.map_err(error)?),
                Request::List { path, request_id } => {
                    if request_id.is_empty()
                        || request_id.len() > 64
                        || !request_id
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                    {
                        return Err("Invalid directory request identity".into());
                    }
                    let stop = tokio_util::sync::CancellationToken::new();
                    {
                        let mut reads = app
                            .directory_reads
                            .lock()
                            .expect("directory reads poisoned");
                        if reads.contains_key(&request_id) {
                            return Err("Directory read identity is already active".into());
                        }
                        reads.insert(request_id.clone(), stop.clone());
                    }
                    let _lease = ReadLease {
                        app: app.clone(),
                        id: request_id,
                    };
                    let listing = tokio::select! {
                        biased;
                        () = stop.cancelled() => return Err("Directory read cancelled".into()),
                        result = client.list(ListRequest { path }) => result.map_err(error)?,
                    };
                    reply(listing)
                }
                Request::Cancel { .. } => {
                    unreachable!("handled without ordinary command admission")
                }
                Request::Create { parent, name } => reply(
                    client
                        .create(CreateRequest { parent, name })
                        .await
                        .map_err(error)?,
                ),
            }
            .map_err(error)?;
            serde_json::to_string(&value).map_err(error)
        })
    }
}
