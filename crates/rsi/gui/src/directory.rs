use crate::{
    GuiApplication,
    application::{Result, error},
};
use futures_util::future::BoxFuture;
use rsi_directory_picker_api::{CreateRequest, ExecutionLocation, ListRequest};
use serde::Deserialize;
use std::sync::Arc;
fn reply<T: serde::Serialize>(
    value: rsi_directory_picker_api::Result<T>,
) -> serde_json::Result<String> {
    #[derive(serde::Serialize)]
    #[serde(tag = "status", rename_all = "snake_case")]
    enum Reply<T> {
        Ok {
            value: T,
        },
        Failed {
            message: String,
            failure: rsi_directory_picker_api::Failure,
        },
    }
    serde_json::to_string(&match value {
        Ok(value) => Reply::Ok { value },
        Err(failure) => Reply::Failed {
            message: failure.to_string(),
            failure,
        },
    })
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Desktop,
    Reveal {
        path: String,
    },
    Status {
        location: ExecutionLocation,
    },
    List {
        location: ExecutionLocation,
        path: Option<String>,
        request_id: String,
    },
    Cancel {
        request_id: String,
    },
    Create {
        location: ExecutionLocation,
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
        self.admit_reply(move |app| async move {
            let client = app
                .directory
                .as_ref()
                .ok_or("Directory browsing is unavailable; enter a path manually")?;
            match request {
                Request::Desktop => reply(client.desktop().await.map_err(error)?),
                Request::Reveal { path } => reply(client.reveal(&path).await.map_err(error)?),
                Request::Status { location } => reply(client.status(location).await.map_err(error)?),
                Request::List { location, path, request_id } => {
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
                        result = client.list(location, ListRequest { path }) => result.map_err(error)?,
                    };
                    reply(listing)
                }
                Request::Cancel { .. } => {
                    unreachable!("handled without ordinary command admission")
                }
                Request::Create { location, parent, name } => reply(
                    client
                        .create(location, CreateRequest { parent, name })
                        .await
                        .map_err(error)?,
                ),
            }
            .map_err(error)
        })
    }
}
