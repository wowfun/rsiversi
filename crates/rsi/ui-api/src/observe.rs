use crate::{
    Item, Observe, UiBinding,
    state::{Displayed, Presentation, Session, State, key, name, scope},
    ui_error,
};
use futures_util::{FutureExt as _, Stream, StreamExt as _, stream::FuturesUnordered};
use rsi_api_protocol::{
    ApiContext, ApiError, ApiMessage, ApiOutput, ApiResponseCapacity, CallOrigin, Result,
};
use std::{
    collections::BTreeSet,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use tokio::sync::{OwnedSemaphorePermit, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

impl State {
    pub fn observe(
        self: &Arc<Self>,
        context: ApiContext,
        request: Observe,
        capacity: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        name(&request.application)?;
        if request.selections.is_empty() || request.selections.len() > 16 {
            return Err(ApiError::Capacity);
        }
        for selection in &request.selections {
            scope(&selection.scope)?;
            name(&selection.bundle)?;
            name(&selection.surface)?;
        }
        let ApiResponseCapacity::Subscription { .. } = &capacity else {
            return Err(ApiError::Unavailable);
        };
        let slot = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Capacity)?;
        let session = Arc::new(Session {
            presentations: Mutex::new(Vec::new()),
            changed: tokio::sync::Notify::new(),
            stop: self.stop.child_token(),
        });
        let key = key(&context.origin, &request.application);
        {
            let mut sessions = self.sessions.lock().expect("UI applications poisoned");
            if self.stop.is_cancelled() {
                return Err(ApiError::ShuttingDown);
            }
            if sessions
                .get(&key)
                .and_then(std::sync::Weak::upgrade)
                .is_some()
            {
                return Err(ApiError::Capacity);
            }
            sessions.insert(key.clone(), Arc::downgrade(&session));
        }
        let (sender, receiver) = mpsc::channel(1);
        let (terminal, end) = oneshot::channel();
        let stream = Forwarded {
            receiver: Some(receiver),
            terminal: Some(end),
            stop: session.stop.clone(),
        };
        let state = self.clone();
        let token = self.tasks.token();
        self.execution.spawn(async move {
            let (_slot, _token): (OwnedSemaphorePermit, _) = (slot, token);
            let mut bindings = Vec::new();
            let revoked = match &context.origin { CallOrigin::Local => CancellationToken::new(), CallOrigin::Device(device) => device.revoked.clone() };
            let mut result = tokio::select! {
                biased;
                () = session.stop.cancelled() => Ok(()),
                () = context.retiring.cancelled() => Err(ApiError::ShuttingDown),
                () = revoked.cancelled() => Err(ApiError::Unauthorized),
                result = std::panic::AssertUnwindSafe(state.produce(&session, &context, request, &sender, capacity, &mut bindings)).catch_unwind() => result.unwrap_or(Err(ApiError::Backend("UI target or observation panicked".into()))),
            };
            session.stop.cancel();
            let presentations = std::mem::take(&mut *session.presentations.lock().expect("UI presentations poisoned"));
            for presentation in presentations { if let Err(error) = presentation.lease.close().await { state.failed.store(true, std::sync::atomic::Ordering::Release); result = Err(ui_error(&error)); } }
            for binding in bindings { if let Err(error) = binding.close().await { state.failed.store(true, std::sync::atomic::Ordering::Release); result = Err(error); } }
            state.sessions.lock().expect("UI applications poisoned").remove(&key);
            drop(sender);
            let _ = terminal.send(result);
        });
        Ok(ApiOutput::Stream(Box::pin(stream)))
    }
    async fn bind_selections(
        &self,
        session: &Session,
        context: &ApiContext,
        request: Observe,
        bindings: &mut Vec<UiBinding>,
    ) -> Result<()> {
        for selection in request.selections {
            let binding = self
                .binder
                .bind(
                    context.origin.clone(),
                    selection.scope,
                    session.stop.clone(),
                )
                .await?;
            // Retain each acquired owner before the next fallible operation.
            bindings.push(binding);
            let binding = bindings.last().expect("binding just inserted");
            let reference = binding
                .ui
                .surfaces(&binding.target)
                .map_err(|error| ui_error(&error))?
                .into_iter()
                .find(|entry| {
                    entry.bundle == selection.bundle && entry.reference.name == selection.surface
                })
                .ok_or(ApiError::Unavailable)?
                .reference;
            let lease = Arc::new(
                binding
                    .ui
                    .present(&reference)
                    .map_err(|error| ui_error(&error))?,
            );
            let presentation = Arc::new(Presentation {
                identity: lease.identity().clone(),
                lease,
                displayed: Mutex::new(Displayed {
                    revision: 0,
                    ticket: None,
                    busy: false,
                }),
            });
            // Store the lease before awaiting its source, so cancellation joins it.
            session
                .presentations
                .lock()
                .expect("UI presentations poisoned")
                .push(presentation.clone());
            presentation
                .lease
                .ready()
                .await
                .map_err(|error| ui_error(&error))?;
        }
        Ok(())
    }
    async fn produce(
        &self,
        session: &Session,
        context: &ApiContext,
        request: Observe,
        sender: &mpsc::Sender<ApiMessage>,
        capacity: ApiResponseCapacity,
        bindings: &mut Vec<UiBinding>,
    ) -> Result<()> {
        let ApiResponseCapacity::Subscription { budget, maximum } = capacity else {
            return Err(ApiError::Unavailable);
        };
        self.bind_selections(session, context, request, bindings)
            .await?;
        let presentations = session
            .presentations
            .lock()
            .expect("UI presentations poisoned")
            .clone();
        let mut pending: BTreeSet<usize> = (0..presentations.len()).collect();
        let mut sent = vec![None; presentations.len()];
        let mut changes = FuturesUnordered::new();
        for (index, presentation) in presentations.iter().enumerate() {
            changes.push(changed(index, presentation.lease.changes()));
        }
        loop {
            if let Some(index) = pending.pop_first() {
                let permit = sender.reserve().await.map_err(|_| ApiError::Unavailable)?;
                let reservation = budget.reserve(maximum)?;
                let presentation = &presentations[index];
                let snapshot = presentation
                    .lease
                    .snapshot()
                    .map_err(|error| ui_error(&error))?
                    .ok_or(ApiError::Unavailable)?;
                let ticket = {
                    let mut shown = presentation.displayed.lock().expect("UI ticket poisoned");
                    if shown.revision != snapshot.revision() {
                        shown.revision = snapshot.revision();
                        shown.ticket = None;
                    }
                    if !shown.busy && shown.ticket.is_none() {
                        shown.ticket =
                            Some(rsi_ui::fresh_identity("input").map_err(ApiError::Backend)?);
                    }
                    shown.ticket.clone()
                };
                let delivery = (snapshot.revision(), ticket.clone());
                if sent[index].as_ref() == Some(&delivery) {
                    continue;
                }
                let item = Item {
                    selection: index,
                    snapshot: snapshot.model(),
                    ticket,
                };
                let json = reservation.encode(&item)?.with_retention(snapshot);
                sent[index] = Some(delivery);
                permit.send(ApiMessage { json, binary: None });
                continue;
            }
            tokio::select! {
                () = sender.closed() => return Ok(()),
                () = session.changed.notified() => { pending.extend(0..presentations.len()); },
                change = changes.next() => {
                    let Some((index, receiver, result)) = change else { return Ok(()); };
                    result?;
                    pending.insert(index);
                    changes.push(changed(index, receiver));
                }
            }
        }
    }
}
async fn changed(
    index: usize,
    mut receiver: tokio::sync::watch::Receiver<rsi_ui::PresentationStatus>,
) -> (
    usize,
    tokio::sync::watch::Receiver<rsi_ui::PresentationStatus>,
    Result<()>,
) {
    let result = receiver
        .changed()
        .await
        .map_err(|_| ApiError::Unavailable)
        .and_then(|()| {
            if receiver.borrow_and_update().stopped {
                Err(ApiError::Unavailable)
            } else {
                Ok(())
            }
        });
    (index, receiver, result)
}
struct Forwarded {
    receiver: Option<mpsc::Receiver<ApiMessage>>,
    terminal: Option<oneshot::Receiver<Result<()>>>,
    stop: CancellationToken,
}
impl Stream for Forwarded {
    type Item = Result<ApiMessage>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.receiver.is_none() {
            return Poll::Ready(None);
        }
        if let Some(terminal) = &mut self.terminal
            && let Poll::Ready(end) = Pin::new(terminal).poll(cx)
        {
            self.terminal = None;
            if let Err(error) = end.unwrap_or(Err(ApiError::Backend(
                "UI observation stopped without terminal result".into(),
            ))) {
                self.receiver = None;
                return Poll::Ready(Some(Err(error)));
            }
        }
        match self
            .receiver
            .as_mut()
            .expect("receiver checked")
            .poll_recv(cx)
        {
            Poll::Ready(None) if self.terminal.is_some() => Poll::Pending,
            Poll::Ready(None) => {
                self.receiver = None;
                Poll::Ready(None)
            }
            value => value.map(|item| item.map(Ok)),
        }
    }
}
impl Drop for Forwarded {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
