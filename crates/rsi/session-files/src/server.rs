use crate::wire::{self, FilesOperation, Input, Reply, Request};
use crate::{
    SessionFilesError,
    resources::{FileOwners, FilesView},
};
use rsi_api_protocol::{
    ApiError, ApiHandler, ApiRegistrar, ApiRegistration, CallOrigin, json_handler,
};
use rsi_execution::ExecutionResolver;
use rsi_files_protocol::{FilesBinding, FilesCaller, FilesError};
use rsi_session_protocol::{SessionError, SessionReads};
use serde::{Serialize, de::DeserializeOwned};
use std::{future::Future, sync::Arc};
use tokio_util::sync::CancellationToken;

const SCRATCH_BYTES: usize =
    4 * rsi_agent_session_protocol::MAXIMUM_SESSION_HEADER_BYTES + 4 * wire::MAXIMUM_RESPONSE_BYTES;
#[derive(Clone, Debug)]
struct Services {
    reads: Arc<dyn SessionReads>,
    resolver: Arc<dyn ExecutionResolver>,
    owners: Arc<FileOwners>,
    caller: FilesCaller,
    scratch: Arc<tokio::sync::Semaphore>,
}
#[derive(Serialize)]
#[serde(transparent)]
struct Admitted<T> {
    value: T,
    #[serde(skip)]
    _scratch: tokio::sync::OwnedSemaphorePermit,
}
fn session_error(error: SessionError) -> SessionFilesError {
    if let SessionError::Api(error) = error {
        return error.into();
    }
    match error {
        SessionError::Invalid(_) => FilesError::Invalid,
        SessionError::NotFound(_) => FilesError::Unavailable,
        SessionError::Capacity => FilesError::Capacity,
        SessionError::ShuttingDown => FilesError::Cancelled,
        _ => FilesError::Io,
    }
    .into()
}
fn release(owners: Arc<FileOwners>) -> Arc<dyn ApiHandler> {
    json_handler(move |context, request: Request<wire::Release>| {
        let owners = owners.clone();
        async move {
            if matches!(&context.origin, CallOrigin::Device(device) if device.revoked.is_cancelled())
            {
                return Err(ApiError::Unauthorized);
            }
            let result = request
                .target
                .validate()
                .map_err(|_| FilesError::Invalid)
                .and_then(|()| {
                    owners.release(&context.origin, &request.target, &request.input.token)
                });
            Ok(result.map(|()| Reply { request, body: () }))
        }
    })
}
fn finite<I, O, F, Fut>(services: Services, call: F) -> Arc<dyn ApiHandler>
where
    I: Input + Clone + Serialize + DeserializeOwned + Send + Sync + 'static,
    O: Serialize + Send + 'static,
    F: Fn(Arc<FilesView>, FilesBinding, I, CancellationToken) -> Fut
        + Clone
        + Send
        + Sync
        + 'static,
    Fut: Future<Output = rsi_files_protocol::Result<O>> + Send + 'static,
{
    json_handler(move |context, request: Request<I>| {
        let services = services.clone();
        let call = call.clone();
        async move {
            let scratch = services
                .scratch
                .clone()
                .acquire_many_owned(u32::try_from(SCRATCH_BYTES).expect("bounded Files scratch"))
                .await
                .map_err(|_| ApiError::ShuttingDown)?;
            let operation = async {
                request.target.validate().map_err(session_error)?;
                request.input.validate()?;
                let lease = services
                    .reads
                    .acquire(context.origin.clone(), &request.target)
                    .await
                    .map_err(session_error)?;
                let binding = FilesBinding::new(
                    services.caller.clone(),
                    request.target.session_id.as_str(),
                    &request.target.header_key,
                    lease.header().canonical_cwd().into(),
                )?;
                let execution = services.resolver.lease(
                    context.origin.clone(),
                    lease.header().coordinates().location(),
                )?;
                let files = Arc::new(FilesView::new(
                    services.owners.clone(),
                    execution,
                    request.target.clone(),
                    &context.origin,
                ));
                let cancellation = context.retiring.child_token();
                let _on_drop = cancellation.clone().drop_guard();
                let result = tokio::select! {
                    biased;
                    () = lease.retiring().cancelled() => Err(FilesError::Cancelled),
                    result = call(files, binding, request.input.clone(), cancellation) => result,
                }?;
                Ok::<_, SessionFilesError>(Reply {
                    request,
                    body: result,
                })
            };
            let revoked = match &context.origin {
                CallOrigin::Device(device) => device.revoked.clone(),
                CallOrigin::Local => CancellationToken::new(),
            };
            tokio::select! {
                biased;
                () = revoked.cancelled() => Err(ApiError::Unauthorized),
                () = context.retiring.cancelled() => Err(ApiError::ShuttingDown),
                value = operation => match value {
                    Ok(value) => Ok(Ok(Admitted { value, _scratch: scratch })),
                    Err(SessionFilesError::Files(error)) => Ok(Err(error)),
                    Err(SessionFilesError::Api(error)) => Err(error),
                },
            }
        }
    })
}

/// Owns Files API registrations, without owning Session or native reader state.
#[derive(Debug)]
pub struct SessionFilesApi {
    owners: Arc<FileOwners>,
    registrations: Vec<ApiRegistration>,
}
impl SessionFilesApi {
    /// Register authenticated finite operations against exact owned dependencies.
    pub fn register(
        registrar: &dyn ApiRegistrar,
        reads: Arc<dyn SessionReads>,
        resolver: Arc<dyn ExecutionResolver>,
    ) -> rsi_api_protocol::Result<Self> {
        let services = Services {
            reads,
            resolver,
            owners: Arc::new(FileOwners::default()),
            caller: FilesCaller::default(),
            scratch: Arc::new(tokio::sync::Semaphore::new(
                rsi_api_protocol::MAXIMUM_API_BYTES,
            )),
        };
        let owners = services.owners.clone();
        let handlers = [
            (
                FilesOperation::Open,
                finite(
                    services.clone(),
                    |files, binding, input: wire::Open, cancellation| async move {
                        files
                            .open(binding, input.path, input.kind, cancellation)
                            .await
                    },
                ),
            ),
            (
                FilesOperation::Read,
                finite(
                    services.clone(),
                    |files, binding, input: wire::Read, cancellation| async move {
                        if files.describe(&binding, &input.file.token)? != input.file {
                            return Err(FilesError::Binding);
                        }
                        let page = files
                            .read(
                                binding,
                                input.file.token,
                                input.offset,
                                input.maximum,
                                cancellation,
                            )
                            .await?;
                        if page.total != input.file.length {
                            return Err(FilesError::Changed);
                        }
                        Ok(page)
                    },
                ),
            ),
            (
                FilesOperation::List,
                finite(
                    services.clone(),
                    |files, binding, input: wire::List, cancellation| async move {
                        if files.describe(&binding, &input.file.token)? != input.file {
                            return Err(FilesError::Binding);
                        }
                        let page = files
                            .list(
                                binding,
                                input.file.token,
                                input.offset,
                                input.maximum,
                                cancellation,
                            )
                            .await?;
                        if page.total as u64 != input.file.length {
                            return Err(FilesError::Changed);
                        }
                        Ok(page)
                    },
                ),
            ),
            (FilesOperation::Release, release(services.owners)),
        ];
        let mut registrations = Vec::new();
        for (operation, handler) in handlers {
            registrations.push(registrar.register(operation.spec(), handler)?);
        }
        Ok(Self {
            owners,
            registrations,
        })
    }
    /// Retire admission and cancel all finite readers before dependencies retire.
    pub async fn close(self) {
        futures_util::future::join_all(self.registrations.into_iter().map(ApiRegistration::close))
            .await;
        self.owners.clear();
    }
}
