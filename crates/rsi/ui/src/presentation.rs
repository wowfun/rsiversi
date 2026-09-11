use super::{Capture, Owner, Ui};
use crate::{
    ActionInput, MAXIMUM_VIEW_BYTES, ModelSnapshot, PresentationAction, PresentationIdentity,
    Result, UiError, UiModel, UiReference,
};
use futures_util::{FutureExt as _, future::BoxFuture};
use rsi_api_protocol::{ByteBudget, ByteReceiver, ByteReservation, RetainedBytes};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub(super) struct SnapshotPool {
    slots: Arc<Semaphore>,
    bytes: ByteBudget,
    released: watch::Sender<()>,
}
impl SnapshotPool {
    pub(super) fn new() -> Self {
        Self {
            slots: Arc::new(Semaphore::new(crate::MAXIMUM_SNAPSHOTS)),
            bytes: ByteBudget::new(crate::MAXIMUM_SNAPSHOT_BYTES).expect("snapshot byte limit"),
            released: watch::channel(()).0,
        }
    }
    fn reserve(&self) -> Result<Reservation> {
        let slot = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| UiError::Capacity)?;
        let bytes = self
            .bytes
            .reserve(MAXIMUM_VIEW_BYTES)
            .map_err(|_| UiError::Capacity)?;
        Ok(Reservation {
            bytes,
            release: Release {
                slot: Some(slot),
                changed: self.released.clone(),
            },
        })
    }
}
struct Reservation {
    // Release bytes before waking capacity-blocked materializers.
    bytes: ByteReservation,
    release: Release,
}
struct Release {
    slot: Option<OwnedSemaphorePermit>,
    changed: watch::Sender<()>,
}
impl Drop for Release {
    fn drop(&mut self) {
        self.slot.take();
        self.changed.send_replace(());
    }
}
impl Reservation {
    fn materialize(
        self,
        presentation: PresentationIdentity,
        revision: u64,
        model: UiModel,
    ) -> Result<SnapshotPin> {
        let snapshot = ModelSnapshot {
            presentation,
            revision,
            model,
        };
        let mut writer = SnapshotWriter(self.bytes.receive());
        snapshot.write_json(&mut writer)?;
        let bytes = writer
            .0
            .finish_compact()
            .map_err(|_| UiError::Invalid("encoded snapshot exceeds admission".into()))?;
        self.release.changed.send_replace(());
        let bytes = bytes.with_retention(self.release);
        Ok(SnapshotPin(Arc::new(Snapshot {
            identity: snapshot.presentation,
            revision,
            actions: snapshot
                .model
                .actions
                .into_iter()
                .map(|action| action.name)
                .collect(),
            sources: snapshot
                .model
                .sources
                .into_iter()
                .map(|source| source.name)
                .collect(),
            bytes,
        })))
    }
}
struct SnapshotWriter(ByteReceiver);
impl std::io::Write for SnapshotWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.append(bytes).map_err(std::io::Error::other)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
#[derive(Debug)]
struct Snapshot {
    identity: PresentationIdentity,
    revision: u64,
    actions: BTreeSet<String>,
    sources: BTreeSet<String>,
    bytes: RetainedBytes,
}
/// Shared immutable encoded model. Clones retain the same snapshot slot and byte charge.
#[derive(Clone, Debug)]
pub struct SnapshotPin(Arc<Snapshot>);
impl SnapshotPin {
    /// Exact owner and presentation epoch.
    pub fn identity(&self) -> &PresentationIdentity {
        &self.0.identity
    }
    /// Presentation-local monotonic snapshot revision.
    pub fn revision(&self) -> u64 {
        self.0.revision
    }
    /// Immutable complete snapshot JSON; its bytes keep the shared byte charge alive.
    pub fn bytes(&self) -> &RetainedBytes {
        &self.0.bytes
    }
    /// Decodes already validated data for a trusted local renderer; performs no I/O.
    ///
    /// # Panics
    /// Panics if the internally validated snapshot encoding is corrupted.
    pub fn model(&self) -> ModelSnapshot {
        serde_json::from_slice(self.0.bytes.as_ref()).expect("validated snapshot encoding")
    }
    /// Produces an address only for an action exposed by this snapshot.
    pub fn action(&self, name: &str) -> Option<PresentationAction> {
        self.0.actions.contains(name).then(|| PresentationAction {
            presentation: self.0.identity.clone(),
            revision: self.0.revision,
            action: name.into(),
        })
    }
}
/// Latest materialization state. Diagnostics remain local and bounded.
#[derive(Clone, Debug, Default)]
pub struct PresentationStatus {
    /// Current published snapshot, zero before the first successful publication.
    pub revision: u64,
    /// Most recent refresh failure, at most 4096 UTF-8 bytes.
    pub diagnostic: Option<String>,
    /// The owner has closed publication and invocation.
    pub stopped: bool,
    failure: Option<FailureKind>,
}
#[derive(Clone, Copy, Debug)]
enum FailureKind {
    Invalid,
    Retired,
    Capacity,
    Handler,
}
impl FailureKind {
    fn error(self, diagnostic: String) -> UiError {
        match self {
            Self::Invalid => UiError::Invalid(diagnostic),
            Self::Retired => UiError::Retired,
            Self::Capacity => UiError::Capacity,
            Self::Handler => UiError::Action(diagnostic),
        }
    }
}
#[derive(Debug)]
struct Publication {
    snapshot: Option<SnapshotPin>,
    action_epoch: u64,
    active_actions: usize,
    dirty: bool,
}
#[derive(Clone, Copy)]
struct RefreshTicket {
    revision: u64,
    action_epoch: u64,
}
impl Publication {
    fn revision(&self) -> u64 {
        self.snapshot.as_ref().map_or(0, SnapshotPin::revision)
    }
    fn accepts(&self, ticket: RefreshTicket) -> bool {
        self.revision() == ticket.revision
            && self.action_epoch == ticket.action_epoch
            && self.active_actions == 0
    }
}
struct ActionCompletion {
    state: Arc<Presentation>,
    failed: bool,
}
impl Drop for ActionCompletion {
    fn drop(&mut self) {
        let mut current = self
            .state
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        current.active_actions -= 1;
        current.dirty |= self.failed;
        self.state.wake.send_replace(());
    }
}
#[derive(Debug)]
struct Presentation {
    identity: PresentationIdentity,
    current: Mutex<Publication>,
    wake: watch::Sender<()>,
    changed: watch::Sender<PresentationStatus>,
    stop: CancellationToken,
    finished: CancellationToken,
    actions: Arc<Owner>,
    bound_context: Mutex<Option<rsi_meta::Context>>,
    cleanup_error: Mutex<Option<String>>,
}
impl Presentation {
    fn live(&self, capture: &Capture) -> bool {
        !self.stop.is_cancelled()
            && capture.entry.position.is_admitting()
            && capture.target.position.is_admitting()
            && !capture.entry.owner.stop.is_cancelled()
            && !capture.target.owner.stop.is_cancelled()
    }
    fn invalidate(&self) {
        self.current.lock().expect("presentation poisoned").dirty = true;
        self.wake.send_replace(());
    }
    fn begin_refresh(&self) -> Option<RefreshTicket> {
        let mut current = self.current.lock().expect("presentation poisoned");
        if !current.dirty || current.active_actions != 0 {
            return None;
        }
        current.dirty = false;
        Some(RefreshTicket {
            revision: current.revision(),
            action_epoch: current.action_epoch,
        })
    }
    fn publish(
        &self,
        reservation: Reservation,
        capture: &Capture,
        model: UiModel,
        predecessor: u64,
        refresh: Option<RefreshTicket>,
    ) -> Result<Option<SnapshotPin>> {
        validate_model(capture, &model)?;
        let next = predecessor.checked_add(1).ok_or(UiError::Capacity)?;
        let snapshot = reservation.materialize(self.identity.clone(), next, model)?;
        let mut current = self.current.lock().expect("presentation poisoned");
        if !self.live(capture) {
            return Err(UiError::Retired);
        }
        if current.revision() != predecessor
            || refresh.is_some_and(|ticket| !current.accepts(ticket))
        {
            return Ok(None);
        }
        current.snapshot = Some(snapshot.clone());
        self.changed.send_replace(PresentationStatus {
            revision: next,
            diagnostic: None,
            stopped: false,
            failure: None,
        });
        Ok(Some(snapshot))
    }
    fn fail_refresh(&self, capture: &Capture, ticket: RefreshTicket, error: &UiError) {
        let current = self.current.lock().expect("presentation poisoned");
        if self.live(capture) && current.accepts(ticket) {
            self.fail(error);
        }
    }
    fn fail(&self, error: &UiError) {
        let mut message = error.to_string();
        let mut end = message.len().min(4096);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        self.changed.send_modify(|status| {
            status.diagnostic = Some(message);
            status.failure = Some(match error {
                UiError::Invalid(_) => FailureKind::Invalid,
                UiError::Retired => FailureKind::Retired,
                UiError::Capacity => FailureKind::Capacity,
                UiError::Action(_) | UiError::Meta(_) => FailureKind::Handler,
            });
        });
    }
    fn retire(&self) {
        let mut current = self.current.lock().expect("presentation poisoned");
        self.stop.cancel();
        self.actions.retire();
        current.snapshot.take();
        self.changed.send_modify(|status| status.stopped = true);
    }
}
/// One live surface's materialization and displayed-action authority.
/// Dropping it requests retirement; the exact Meta owners retain final draining.
#[derive(Debug)]
pub struct PresentationLease {
    ui: Weak<Ui>,
    state: Arc<Presentation>,
}
impl PresentationLease {
    /// Requests one coalesced refresh for this exact live presentation.
    pub fn invalidate(&self) -> Result<()> {
        self.snapshot()?;
        self.state.invalidate();
        Ok(())
    }

    /// Exact presentation identity, available before the first model is produced.
    pub fn identity(&self) -> &PresentationIdentity {
        &self.state.identity
    }
    /// Reads an existing snapshot synchronously without calling its source.
    ///
    /// # Panics
    /// Panics if the presentation mutex was poisoned by an earlier panic.
    pub fn snapshot(&self) -> Result<Option<SnapshotPin>> {
        let ui = self.ui.upgrade().ok_or(UiError::Retired)?;
        if !ui.is_current(&self.state.identity.reference) || self.state.stop.is_cancelled() {
            return Err(UiError::Retired);
        }
        Ok(self
            .state
            .current
            .lock()
            .expect("presentation poisoned")
            .snapshot
            .clone())
    }
    /// Latest bounded state for an owner-rendered diagnostic.
    pub fn status(&self) -> PresentationStatus {
        self.state.changed.borrow().clone()
    }
    /// Coalesced materialization changes, unrelated to durable observations.
    pub fn changes(&self) -> watch::Receiver<PresentationStatus> {
        self.state.changed.subscribe()
    }
    /// Waits for the first snapshot or a materialization error.
    pub async fn ready(&self) -> Result<SnapshotPin> {
        let mut changes = self.changes();
        loop {
            if let Some(snapshot) = self.snapshot()? {
                return Ok(snapshot);
            }
            {
                let status = changes.borrow_and_update();
                if let Some(failure) = status.failure {
                    return Err(failure.error(status.diagnostic.clone().unwrap_or_default()));
                }
            }
            changes.changed().await.map_err(|_| UiError::Retired)?;
        }
    }
    /// Closes publication, cancels reads and drains already admitted actions.
    pub async fn close(&self) -> Result<()> {
        self.state.retire();
        self.state.finished.cancelled().await;
        match self
            .state
            .cleanup_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            Some(error) => Err(UiError::Action(error)),
            None => Ok(()),
        }
    }
    /// Admits one displayed action before returning a waiter. Dropping the waiter
    /// preserves the task under its contribution, target and presentation owners.
    ///
    /// # Panics
    /// Panics if the presentation mutex was poisoned by an earlier panic.
    pub fn invoke(
        &self,
        reference: &PresentationAction,
        input: ActionInput,
    ) -> BoxFuture<'static, Result<SnapshotPin>> {
        let admitted = (|| {
            input.validate()?;
            let ui = self.ui.upgrade().ok_or(UiError::Retired)?;
            let mut current = self.state.current.lock().expect("presentation poisoned");
            let snapshot = current.snapshot.as_ref().ok_or(UiError::Retired)?;
            if self.state.stop.is_cancelled()
                || reference.presentation != self.state.identity
                || reference.revision != snapshot.revision()
                || !snapshot.0.actions.contains(&reference.action)
            {
                return Err(UiError::Retired);
            }
            let capture = ui.capture(&reference.presentation.reference)?;
            let handler = capture
                .entry
                .value
                .actions
                .iter()
                .find(|action| {
                    action.name == reference.action && action.target == capture.target.kind
                })
                .ok_or(UiError::Retired)?
                .handler
                .clone();
            let permit = ui
                .slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| UiError::Capacity)?;
            let token = self.state.actions.admit()?;
            let reservation = ui.snapshots.reserve()?;
            current.action_epoch = current
                .action_epoch
                .checked_add(1)
                .ok_or(UiError::Capacity)?;
            current.active_actions += 1;
            let completion = ActionCompletion {
                state: self.state.clone(),
                failed: true,
            };
            Ok((ui, capture, handler, permit, token, reservation, completion))
        })();
        let (ui, capture, handler, permit, token, reservation, completion) = match admitted {
            Ok(admitted) => admitted,
            Err(error) => return Box::pin(async { Err(error) }),
        };
        let state = self.state.clone();
        let revision = reference.revision;
        let task = ui.execution.spawn(async move {
            let mut completion = completion;
            let (_permit, _token) = (permit, token);
            let model = handler
                .invoke_model(action_target(&capture, &state), input)
                .await
                .map_err(|error| UiError::Action(error.to_string()))?;
            let snapshot = state
                .publish(reservation, &capture, model, revision, None)
                .map_err(|error| UiError::Action(error.to_string()))?
                .ok_or_else(|| {
                    UiError::Action("action reply was superseded after execution".into())
                })?;
            completion.failed = false;
            Ok(snapshot)
        });
        Box::pin(async move {
            task.await
                .map_err(|error| UiError::Action(error.to_string()))?
        })
    }
    /// Reads a bounded byte window only from a source exposed by the current snapshot.
    ///
    /// # Panics
    /// Panics if the presentation mutex was poisoned by an earlier panic.
    pub fn source(
        &self,
        revision: u64,
        name: &str,
        offset: u64,
        maximum: usize,
    ) -> BoxFuture<'static, Result<RetainedBytes>> {
        let admitted = (|| {
            if maximum == 0 || maximum > crate::MAXIMUM_INPUT_BYTES {
                return Err(UiError::Invalid("source window exceeds limit".into()));
            }
            let ui = self.ui.upgrade().ok_or(UiError::Retired)?;
            let current = self.state.current.lock().expect("presentation poisoned");
            let snapshot = current.snapshot.as_ref().ok_or(UiError::Retired)?;
            if self.state.stop.is_cancelled()
                || revision != snapshot.revision()
                || !snapshot.0.sources.contains(name)
            {
                return Err(UiError::Retired);
            }
            let capture = ui.capture(&self.state.identity.reference)?;
            let renderer = surface(&capture, &self.state.identity.reference)?;
            let token = self.state.actions.admit()?;
            let permit = ui
                .slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| UiError::Capacity)?;
            let reservation = ui
                .snapshots
                .bytes
                .reserve(maximum)
                .map_err(|_| UiError::Capacity)?;
            let reservation = Reservation {
                bytes: reservation,
                release: Release {
                    slot: None,
                    changed: ui.snapshots.released.clone(),
                },
            };
            Ok((ui, capture, renderer, token, permit, reservation))
        })();
        let (ui, capture, renderer, token, permit, reservation) = match admitted {
            Ok(admitted) => admitted,
            Err(error) => return Box::pin(async { Err(error) }),
        };
        let state = self.state.clone();
        let name = name.to_owned();
        let task = ui.execution.spawn(async move {
            let (_token, _permit) = (token, permit);
            let bytes = tokio::select! {
                biased;
                () = state.stop.cancelled() => return Err(UiError::Retired),
                () = capture.entry.owner.stop.cancelled() => return Err(UiError::Retired),
                () = capture.target.owner.stop.cancelled() => return Err(UiError::Retired),
                bytes = renderer.source(action_target(&capture, &state), name, offset, maximum) => bytes?,
            };
            reservation.bytes.retain_vec(bytes).map(|bytes| {
                    reservation.release.changed.send_replace(());
                    bytes.with_retention(reservation.release)
                }).map_err(|_| UiError::Invalid("source exceeded its admitted window".into()))
        });
        Box::pin(async move {
            task.await
                .map_err(|error| UiError::Action(error.to_string()))?
        })
    }
}
impl Drop for PresentationLease {
    fn drop(&mut self) {
        self.state.retire();
    }
}

impl Ui {
    /// Starts one coalesced asynchronous materializer under its actual Meta owners.
    pub fn present(self: &Arc<Self>, reference: &UiReference) -> Result<PresentationLease> {
        let capture = self.capture(reference)?;
        let renderer = surface(&capture, reference)?;
        let permit = self
            .presentations
            .clone()
            .try_acquire_owned()
            .map_err(|_| UiError::Capacity)?;
        let initial = self.snapshots.reserve()?;
        let (status_sender, _) = watch::channel(PresentationStatus::default());
        let state = Arc::new(Presentation {
            identity: PresentationIdentity {
                reference: reference.clone(),
                epoch: crate::fresh_identity("presentation").map_err(UiError::Action)?,
            },
            current: Mutex::new(Publication {
                snapshot: None,
                action_epoch: 0,
                active_actions: 0,
                dirty: true,
            }),
            wake: watch::channel(()).0,
            changed: status_sender,
            stop: CancellationToken::new(),
            finished: CancellationToken::new(),
            actions: Arc::new(Owner::default()),
            bound_context: Mutex::new(None),
            cleanup_error: Mutex::new(None),
        });
        let changes = (
            capture.entry.owner.changed.subscribe(),
            capture.target.owner.changed.subscribe(),
        );
        let lease = PresentationLease {
            ui: Arc::downgrade(self),
            state: state.clone(),
        };
        let pool = self.snapshots.clone();
        let work = async move {
            let _permit = permit;
            let mut binding = None;
            let result = std::panic::AssertUnwindSafe(async {
                binding = bind(&state, &capture, renderer.as_ref()).await?;
                *state
                    .bound_context
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    binding.as_ref().map(|binding| binding.context().clone());
                materialize(&state, &capture, renderer, pool, initial, changes).await;
                Ok::<_, UiError>(())
            })
            .catch_unwind()
            .await;
            match result {
                Ok(Err(error)) => state.fail(&error),
                Err(_) => state.fail(&UiError::Action("model source panicked".into())),
                Ok(Ok(())) => {}
            }
            state.retire();
            state.actions.drain().await;
            state
                .bound_context
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(binding) = binding {
                let result = std::panic::AssertUnwindSafe(binding.close())
                    .catch_unwind()
                    .await;
                if !matches!(result, Ok(Ok(()))) {
                    let error = "presentation scope cleanup failed".to_owned();
                    *state
                        .cleanup_error
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error.clone());
                    state.fail(&UiError::Action(error));
                }
            }
            state.finished.cancel();
        };
        drop(self.execution.spawn(work));
        Ok(lease)
    }
    /// Current logical resource admission, including escaped immutable byte readers.
    pub fn presentation_usage(&self) -> (usize, usize, usize) {
        (
            crate::MAXIMUM_PRESENTATIONS - self.presentations.available_permits(),
            crate::MAXIMUM_SNAPSHOTS - self.snapshots.slots.available_permits(),
            self.snapshots.bytes.used(),
        )
    }
}
async fn bind(
    state: &Presentation,
    capture: &Capture,
    renderer: &dyn crate::SurfaceRenderer,
) -> Result<Option<crate::PresentationBinding>> {
    let stop = state.stop.child_token();
    let future = renderer.bind(
        capture.target.context.clone(),
        state.identity.clone(),
        stop.clone(),
    );
    tokio::pin!(future);
    tokio::select! {
        result = &mut future => result,
        () = state.stop.cancelled() => { stop.cancel(); future.await },
        () = capture.entry.owner.stop.cancelled() => { stop.cancel(); future.await },
        () = capture.target.owner.stop.cancelled() => { stop.cancel(); future.await },
    }
}
fn context(capture: &Capture, state: &Presentation) -> rsi_meta::Context {
    state
        .bound_context
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .unwrap_or_else(|| capture.target.context.clone())
}
async fn materialize(
    state: &Presentation,
    capture: &Capture,
    renderer: Arc<dyn crate::SurfaceRenderer>,
    pool: SnapshotPool,
    initial: Reservation,
    changes: (watch::Receiver<()>, watch::Receiver<()>),
) {
    let (mut contribution_changes, mut target_changes) = changes;
    let mut initial = Some(initial);
    let mut released = pool.released.subscribe();
    let mut wake = state.wake.subscribe();
    let mut capacity_blocked = false;
    loop {
        wake.borrow_and_update();
        for changes in [&mut contribution_changes, &mut target_changes] {
            if changes.has_changed().unwrap_or(false) {
                changes.borrow_and_update();
                state.invalidate();
            }
        }
        if !capacity_blocked && let Some(ticket) = state.begin_refresh() {
            released.borrow_and_update();
            let reservation = initial.take().map_or_else(|| pool.reserve(), Ok);
            let result = match reservation {
                Ok(reservation) => tokio::select! {
                    biased;
                    () = state.stop.cancelled() => break,
                    () = capture.entry.owner.stop.cancelled() => break,
                    () = capture.target.owner.stop.cancelled() => break,
                    result = renderer.model_in(context(capture, state), state.identity.clone()) =>
                        result.and_then(|model| state.publish(reservation, capture, model, ticket.revision, Some(ticket))),
                },
                Err(error) => {
                    capacity_blocked = true;
                    state.invalidate();
                    Err(error)
                }
            };
            if let Err(error) = result {
                state.fail_refresh(capture, ticket, &error);
            }
            if !capacity_blocked {
                continue;
            }
        }
        tokio::select! {
            biased;
            () = state.stop.cancelled() => break,
            () = capture.entry.owner.stop.cancelled() => break,
            () = capture.target.owner.stop.cancelled() => break,
            result = contribution_changes.changed() => {
                if result.is_err() { break; }
                state.invalidate();
            },
            result = target_changes.changed() => {
                if result.is_err() { break; }
                state.invalidate();
            },
            _ = wake.changed() => {},
            _ = released.changed(), if capacity_blocked => { capacity_blocked = false; },
        }
    }
}

fn action_target(capture: &Capture, state: &Presentation) -> crate::ActionTarget {
    crate::ActionTarget {
        context: context(capture, state),
        contribution_stop: capture.entry.owner.stop.clone(),
        target_stop: capture.target.owner.stop.clone(),
        presentation_stop: state.stop.clone(),
        presentation: Some(state.identity.clone()),
    }
}
fn surface(capture: &Capture, reference: &UiReference) -> Result<Arc<dyn crate::SurfaceRenderer>> {
    capture
        .entry
        .value
        .surfaces
        .iter()
        .find(|surface| surface.name == reference.name && surface.target == capture.target.kind)
        .map(|surface| surface.renderer.clone())
        .ok_or(UiError::Retired)
}
fn validate_model(capture: &Capture, model: &UiModel) -> Result<()> {
    for action in &model.actions {
        if !capture.entry.value.actions.iter().any(|candidate| {
            candidate.name == action.name && candidate.target == capture.target.kind
        }) {
            return Err(UiError::Invalid(
                "model exposes an unavailable action".into(),
            ));
        }
    }
    Ok(())
}
