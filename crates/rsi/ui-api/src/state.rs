use crate::{
    CatalogCursor, CatalogEntry, CatalogPage, CatalogRequest, ExportScope, Invoke, Source,
    UiTargetBinder, ui_error,
};
use rsi_api_protocol::{ApiContext, ApiError, CallOrigin, DeviceId, Result, RetainedBytes};
use rsi_meta::Execution;
use rsi_ui::{PresentationIdentity, PresentationLease};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::{Notify, Semaphore};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub(crate) type Key = (Option<DeviceId>, String);
pub(crate) fn key(origin: &CallOrigin, application: &str) -> Key {
    (
        match origin {
            CallOrigin::Local => None,
            CallOrigin::Device(device) => Some(device.id.clone()),
        },
        application.into(),
    )
}
pub(crate) fn name(value: &str) -> Result<()> {
    if !rsi_ui::name_valid(value) {
        return Err(ApiError::Invalid("invalid UI name".into()));
    }
    Ok(())
}
pub(crate) fn scope(value: &ExportScope) -> Result<()> {
    value
        .validate()
        .map_err(|_| ApiError::Invalid("invalid UI scope".into()))
}
#[derive(Debug)]
pub(crate) struct Displayed {
    pub revision: u64,
    pub ticket: Option<String>,
    pub busy: bool,
}
#[derive(Debug)]
pub(crate) struct Presentation {
    pub identity: PresentationIdentity,
    pub lease: Arc<PresentationLease>,
    pub displayed: Mutex<Displayed>,
}
#[derive(Debug)]
pub(crate) struct Session {
    pub presentations: Mutex<Vec<Arc<Presentation>>>,
    pub changed: Notify,
    pub stop: CancellationToken,
}
#[derive(Debug)]
pub(crate) struct State {
    pub failed: std::sync::atomic::AtomicBool,
    pub execution: Execution,
    pub binder: Arc<dyn UiTargetBinder>,
    pub tasks: TaskTracker,
    pub stop: CancellationToken,
    pub slots: Arc<Semaphore>,
    pub sessions: Mutex<BTreeMap<Key, Weak<Session>>>,
}
impl State {
    pub fn new(execution: Execution, binder: Arc<dyn UiTargetBinder>) -> Self {
        Self {
            failed: std::sync::atomic::AtomicBool::new(false),
            execution,
            binder,
            tasks: TaskTracker::new(),
            stop: CancellationToken::new(),
            slots: Arc::new(Semaphore::new(16)),
            sessions: Mutex::new(BTreeMap::new()),
        }
    }
    pub async fn catalog(
        &self,
        context: ApiContext,
        requested: CatalogRequest,
        stop: CancellationToken,
    ) -> Result<CatalogPage> {
        scope(&requested.scope)?;
        if requested.maximum == 0 || requested.maximum > 64 {
            return Err(ApiError::Capacity);
        }
        if let Some(after) = &requested.after {
            name(&after.bundle)?;
            name(&after.surface)?;
        }
        if self.stop.is_cancelled() {
            return Err(ApiError::ShuttingDown);
        }
        let revoked = match &context.origin {
            CallOrigin::Local => CancellationToken::new(),
            CallOrigin::Device(device) => device.revoked.clone(),
        };
        let binding = tokio::select! {
            biased;
            () = stop.cancelled() => return Err(ApiError::ShuttingDown),
            () = context.retiring.cancelled() => return Err(ApiError::ShuttingDown),
            () = revoked.cancelled() => return Err(ApiError::Unauthorized),
            binding = self.binder.bind(context.origin.clone(), requested.scope, stop.clone()) => binding?,
        };
        let result = binding
            .ui
            .surfaces(&binding.target)
            .map_err(|error| ui_error(&error))
            .map(|entries| {
                let mut entries: Vec<_> = entries
                    .into_iter()
                    .map(|entry| CatalogEntry {
                        bundle: entry.bundle,
                        surface: entry.reference.name,
                        title: entry.title,
                    })
                    .collect();
                entries.sort_by(|a, b| (&a.bundle, &a.surface).cmp(&(&b.bundle, &b.surface)));
                entries.retain(|entry| {
                    requested.after.as_ref().is_none_or(|after| {
                        (&entry.bundle, &entry.surface) > (&after.bundle, &after.surface)
                    })
                });
                let more = entries.len() > requested.maximum;
                entries.truncate(requested.maximum);
                let next = if more {
                    entries.last().map(|entry| CatalogCursor {
                        bundle: entry.bundle.clone(),
                        surface: entry.surface.clone(),
                    })
                } else {
                    None
                };
                CatalogPage { entries, next }
            });
        binding.close().await.inspect_err(|_| {
            self.failed
                .store(true, std::sync::atomic::Ordering::Release);
        })?;
        result
    }
    fn session(&self, context: &ApiContext, application: &str) -> Result<Arc<Session>> {
        name(application)?;
        if self.stop.is_cancelled() {
            return Err(ApiError::ShuttingDown);
        }
        let session = self
            .sessions
            .lock()
            .expect("UI applications poisoned")
            .get(&key(&context.origin, application))
            .and_then(Weak::upgrade)
            .ok_or(ApiError::Unavailable)?;
        if session.stop.is_cancelled() {
            return Err(ApiError::Unavailable);
        }
        Ok(session)
    }
    fn presentation(
        session: &Session,
        identity: &PresentationIdentity,
    ) -> Result<Arc<Presentation>> {
        session
            .presentations
            .lock()
            .expect("UI presentations poisoned")
            .iter()
            .find(|p| p.identity == *identity)
            .cloned()
            .ok_or(ApiError::Unavailable)
    }
    pub async fn invoke(&self, context: ApiContext, request: Invoke) -> Result<()> {
        let session = self.session(&context, &request.application)?;
        let presentation = Self::presentation(&session, &request.action.presentation)?;
        {
            let mut shown = presentation.displayed.lock().expect("UI ticket poisoned");
            if shown.busy
                || shown.revision != request.action.revision
                || shown.ticket.as_deref() != Some(&request.ticket)
            {
                return Err(ApiError::OutcomeUnknown);
            }
            // Consumption precedes both form validation and every UI admission check.
            shown.ticket.take();
            shown.busy = true;
        }
        let busy = Busy {
            presentation: presentation.clone(),
            session,
        };
        // The API registry owns this mutation task; PresentationLease independently
        // owns admitted business work if even this waiter is cancelled or panics.
        let result = presentation
            .lease
            .invoke(&request.action, request.input)
            .await
            .map(|_| ())
            .map_err(|error| ui_error(&error));
        drop(busy);
        result
    }
    pub async fn source(&self, context: ApiContext, request: Source) -> Result<RetainedBytes> {
        let session = self.session(&context, &request.application)?;
        let presentation = Self::presentation(&session, &request.presentation)?;
        if presentation
            .displayed
            .lock()
            .expect("UI ticket poisoned")
            .revision
            != request.revision
        {
            return Err(ApiError::Unavailable);
        }
        presentation
            .lease
            .source(
                request.revision,
                &request.name,
                request.offset,
                request.maximum,
            )
            .await
            .map_err(|error| ui_error(&error))
    }
}
struct Busy {
    presentation: Arc<Presentation>,
    session: Arc<Session>,
}
impl Drop for Busy {
    fn drop(&mut self) {
        self.presentation
            .displayed
            .lock()
            .expect("UI ticket poisoned")
            .busy = false;
        self.session.changed.notify_one();
    }
}
