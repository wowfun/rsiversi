use crate::{
    ActionInput, ActionTarget, BlockInput, BoundView, Contributions, MAXIMUM_ACTIONS,
    MAXIMUM_BUNDLES, MAXIMUM_CONTRIBUTIONS, MAXIMUM_TARGETS, MAXIMUM_VIEW_BYTES, Result,
    SurfaceDescriptor, TargetKind, UiElement, UiError, UiReference, UiView,
};
use futures_util::future::BoxFuture;
use rsi_meta::{
    ActivationPlan, Context, Execution, LocalContract, RegistrationLease,
    RegistrationOrderSnapshot, RegistrationPosition, RuntimeIdentity,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::{Semaphore, watch};
use tokio_util::{
    sync::CancellationToken,
    task::{TaskTracker, task_tracker::TaskTrackerToken},
};
#[path = "presentation.rs"]
mod presentation;
pub use presentation::{PresentationLease, PresentationStatus, SnapshotPin};

#[derive(Debug)]
pub(crate) struct Owner {
    pub(crate) stop: CancellationToken,
    pub(crate) tasks: TaskTracker,
    admission: Mutex<()>,
    changed: watch::Sender<()>,
}
impl Default for Owner {
    fn default() -> Self {
        Self {
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
            admission: Mutex::new(()),
            changed: watch::channel(()).0,
        }
    }
}
impl Owner {
    fn invalidate(&self) -> Result<()> {
        let _admission = self.admission.lock().expect("UI owner admission poisoned");
        if self.stop.is_cancelled() {
            return Err(UiError::Retired);
        }
        self.changed.send_replace(());
        Ok(())
    }
    fn admit(&self) -> Result<TaskTrackerToken> {
        let _admission = self.admission.lock().expect("UI owner admission poisoned");
        if self.stop.is_cancelled() {
            return Err(UiError::Retired);
        }
        Ok(self.tasks.token())
    }
    pub(crate) fn retire(&self) {
        let _admission = self.admission.lock().expect("UI owner admission poisoned");
        self.stop.cancel();
        self.tasks.close();
    }
    async fn drain(&self) {
        self.retire();
        self.tasks.wait().await;
    }
}
#[derive(Debug)]
struct Entry {
    position: RegistrationPosition,
    value: Contributions,
    owner: Arc<Owner>,
}
#[derive(Debug)]
struct Target {
    position: RegistrationPosition,
    context: Context,
    kind: TargetKind,
    owner: Arc<Owner>,
}
#[derive(Debug, Default)]
struct State {
    next: u64,
    entries: BTreeMap<String, Arc<Entry>>,
    targets: BTreeMap<String, Arc<Target>>,
}
impl State {
    fn next(&mut self) -> Result<String> {
        self.next = self.next.checked_add(1).ok_or(UiError::Capacity)?;
        Ok(self.next.to_string())
    }
}
/// One ordinary application's UI registry, independent of domains and layout.
#[derive(Debug)]
pub struct Ui {
    application: String,
    runtime: RuntimeIdentity,
    execution: Execution,
    state: Mutex<State>,
    owner: Arc<Owner>,
    slots: Arc<Semaphore>,
    changed: watch::Sender<u64>,
    presentations: Arc<Semaphore>,
    snapshots: presentation::SnapshotPool,
}
/// Nominal application contribution registry capability.
#[derive(Debug)]
pub struct UiContract;
impl LocalContract for UiContract {
    const KEY: &'static str = "rsi.ui.registry";
    type Service = Ui;
}
/// Actual application or Shell surface target; grants no lifecycle control.
#[derive(Debug)]
pub struct UiTarget {
    ui: Weak<Ui>,
    id: String,
    kind: TargetKind,
}
/// Nominal exact target capability published within its own surface mapping.
#[derive(Debug)]
pub struct UiTargetContract;
impl LocalContract for UiTargetContract {
    const KEY: &'static str = "rsi.ui.target";
    type Service = UiTarget;
}
impl UiTarget {
    /// Explicit target class.
    pub fn kind(&self) -> TargetKind {
        self.kind
    }
}
/// Exact Meta registration plus admitted-work cleanup.
#[derive(Debug)]
pub struct ContributionLease {
    registration: RegistrationLease,
    owner: Arc<Owner>,
}
impl ContributionLease {
    /// Invalidates data only for this exact contribution or target registration.
    pub fn invalidate(&self) -> Result<()> {
        self.owner.invalidate()
    }

    /// Immediately fences new target/contribution work; disposal joins its cleanup.
    pub fn retire(&self) {
        self.owner.retire();
    }
    /// Withdraws and joins both Meta registration cleanup and admitted actions.
    pub async fn dispose(&self) -> rsi_meta::CleanupReport {
        self.owner.retire();
        let report = self.registration.dispose().await;
        self.owner.drain().await;
        report
    }
}
impl Drop for ContributionLease {
    fn drop(&mut self) {
        self.owner.retire();
    }
}

impl Ui {
    pub(crate) fn new(context: &Context) -> Result<Self> {
        let (changed, _) = watch::channel(0);
        Ok(Self {
            application: crate::fresh_identity("ui").map_err(UiError::Action)?,
            runtime: context.runtime_identity(),
            execution: context.runtime().execution().clone(),
            state: Mutex::new(State::default()),
            owner: Arc::new(Owner::default()),
            slots: Arc::new(Semaphore::new(MAXIMUM_ACTIONS)),
            changed,
            presentations: Arc::new(Semaphore::new(crate::MAXIMUM_PRESENTATIONS)),
            snapshots: presentation::SnapshotPool::new(),
        })
    }
    fn membership_changed(&self) {
        self.changed
            .send_modify(|value| *value = value.saturating_add(1));
    }
    /// Coalesced registration changes for catalogs and menus, never domain data.
    pub fn membership_changes(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
    fn ensure_owner(&self, plan: &ActivationPlan) -> Result<()> {
        if plan.context().runtime_identity() != self.runtime {
            return Err(UiError::Invalid("foreign Runtime UI owner".into()));
        }
        if self.owner.stop.is_cancelled() {
            return Err(UiError::Retired);
        }
        Ok(())
    }
    fn own_work(plan: &ActivationPlan, owner: &Arc<Owner>) -> Result<()> {
        let owner = owner.clone();
        plan.defer(
            "drain UI owner actions",
            Box::new(move || {
                Box::pin(async move {
                    owner.drain().await;
                    Ok(())
                })
            }),
        )?;
        Ok(())
    }
    /// Registers one validated bundle under the contributing activation's exact owner.
    /// The returned lease must remain held while its contributions are desired.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned registry state.
    pub fn register(
        self: &Arc<Self>,
        plan: &ActivationPlan,
        value: Contributions,
    ) -> Result<ContributionLease> {
        self.ensure_owner(plan)?;
        validate_contributions(&value)?;
        let owner = Arc::new(Owner::default());
        Self::own_work(plan, &owner)?;
        let id = self.state.lock().expect("UI state poisoned").next()?;
        let weak = Arc::downgrade(self);
        let undo_id = id.clone();
        let undo_owner = owner.clone();
        let entry_owner = owner.clone();
        let ((), registration) = plan.context().registration_context()?.register(
            "withdraw UI contribution",
            move || {
                undo_owner.retire();
                if let Some(ui) = weak.upgrade() {
                    let removed = ui
                        .state
                        .lock()
                        .expect("UI state poisoned")
                        .entries
                        .remove(&undo_id);
                    drop(removed);
                    ui.membership_changed();
                }
                Ok(())
            },
            |position| {
                let mut state = self.state.lock().expect("UI state poisoned");
                if self.owner.stop.is_cancelled() {
                    return Err(meta(UiError::Retired));
                }
                if state.entries.len() >= MAXIMUM_BUNDLES {
                    return Err(meta(UiError::Capacity));
                }
                if state
                    .entries
                    .values()
                    .any(|entry| entry.value.name == value.name)
                {
                    return Err(meta(UiError::Invalid("duplicate UI bundle name".into())));
                }
                state.entries.insert(
                    id,
                    Arc::new(Entry {
                        position,
                        value,
                        owner: entry_owner,
                    }),
                );
                Ok(())
            },
        )?;
        self.membership_changed();
        Ok(ContributionLease {
            registration,
            owner,
        })
    }
    /// Registers the actual target under its exact activation owner.
    /// Domain-specific target factories should require the Local dependencies whose
    /// replacement must retire this target, then publish `UiTargetContract` locally.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned registry state.
    pub fn register_target(
        self: &Arc<Self>,
        plan: &ActivationPlan,
        kind: TargetKind,
    ) -> Result<(Arc<UiTarget>, ContributionLease)> {
        self.ensure_owner(plan)?;
        let owner = Arc::new(Owner::default());
        Self::own_work(plan, &owner)?;
        let id = self.state.lock().expect("UI state poisoned").next()?;
        let weak = Arc::downgrade(self);
        let undo_id = id.clone();
        let undo_owner = owner.clone();
        let target_owner = owner.clone();
        let ((), registration) = plan.context().registration_context()?.register(
            "withdraw UI target",
            move || {
                undo_owner.retire();
                if let Some(ui) = weak.upgrade() {
                    let removed = ui
                        .state
                        .lock()
                        .expect("UI state poisoned")
                        .targets
                        .remove(&undo_id);
                    drop(removed);
                    ui.membership_changed();
                }
                Ok(())
            },
            |position| {
                let mut state = self.state.lock().expect("UI state poisoned");
                if self.owner.stop.is_cancelled() {
                    return Err(meta(UiError::Retired));
                }
                if state.targets.len() >= MAXIMUM_TARGETS {
                    return Err(meta(UiError::Capacity));
                }
                state.targets.insert(
                    id.clone(),
                    Arc::new(Target {
                        position,
                        context: plan.context().clone(),
                        kind,
                        owner: target_owner,
                    }),
                );
                Ok(())
            },
        )?;
        self.membership_changed();
        Ok((
            Arc::new(UiTarget {
                ui: Arc::downgrade(self),
                id,
                kind,
            }),
            ContributionLease {
                registration,
                owner,
            },
        ))
    }
    fn target(&self, state: &State, handle: &UiTarget) -> Result<Arc<Target>> {
        if !std::ptr::eq(handle.ui.as_ptr(), self) {
            return Err(UiError::Retired);
        }
        self.target_id(state, &handle.id)
    }
    fn target_id(&self, state: &State, id: &str) -> Result<Arc<Target>> {
        if self.owner.stop.is_cancelled() {
            return Err(UiError::Retired);
        }
        state
            .targets
            .get(id)
            .filter(|target| target.position.is_admitting() && !target.owner.stop.is_cancelled())
            .cloned()
            .ok_or(UiError::Retired)
    }
    fn reference(&self, target: &str, contribution: &str, name: &str) -> UiReference {
        UiReference {
            application: self.application.clone(),
            target: target.into(),
            contribution: contribution.into(),
            name: name.into(),
        }
    }
    fn ordered(state: &State) -> Result<Vec<(String, Arc<Entry>)>> {
        let entries: Vec<_> = state
            .entries
            .iter()
            .filter(|(_, entry)| entry.position.is_admitting() && !entry.owner.stop.is_cancelled())
            .map(|(id, entry)| (id.clone(), entry.clone()))
            .collect();
        let positions: Vec<_> = entries
            .iter()
            .map(|(_, entry)| entry.position.clone())
            .collect();
        let order = RegistrationOrderSnapshot::capture(&positions)?;
        let mut ranked: Vec<_> = entries.into_iter().zip(order.ranks()).collect();
        ranked.sort_by(|((_, left), lrank), ((_, right), rrank)| {
            lrank
                .compare_position(rrank)
                .then_with(|| left.value.name.cmp(&right.value.name))
        });
        Ok(ranked.into_iter().map(|(entry, _)| entry).collect())
    }
    /// Captures declaration-ordered menu entries for one actual live target.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned registry state.
    pub fn surfaces(&self, handle: &UiTarget) -> Result<Vec<SurfaceDescriptor>> {
        let state = self.state.lock().expect("UI state poisoned");
        let target = self.target(&state, handle)?;
        let entries = Self::ordered(&state)?;
        Ok(entries
            .iter()
            .flat_map(|(id, entry)| {
                entry
                    .value
                    .surfaces
                    .iter()
                    .filter(|surface| surface.target == target.kind)
                    .map(|surface| SurfaceDescriptor {
                        bundle: entry.value.name.clone(),
                        reference: self.reference(&handle.id, id, &surface.name),
                        title: surface.title.clone(),
                    })
            })
            .collect())
    }
    /// Checks whether a displayed view still belongs to a live target and bundle.
    /// Adapters use this before publishing delayed results; action admission checks again.
    pub fn is_current(&self, reference: &UiReference) -> bool {
        self.capture(reference).is_ok()
    }

    /// Tests whether this reference belongs to the supplied exact live target.
    pub fn matches_target(&self, handle: &UiTarget, reference: &UiReference) -> bool {
        std::ptr::eq(handle.ui.as_ptr(), self)
            && handle.id == reference.target
            && self.is_current(reference)
    }
    /// Whether the target has any currently admitting block renderer.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned registry state.
    pub fn has_block_renderers(&self, handle: &UiTarget) -> bool {
        let state = self.state.lock().expect("UI state poisoned");
        self.target(&state, handle).is_ok()
            && state.entries.values().any(|entry| {
                entry.position.is_admitting()
                    && !entry.owner.stop.is_cancelled()
                    && entry
                        .value
                        .renderers
                        .iter()
                        .any(|renderer| renderer.target == handle.kind)
            })
    }

    fn capture(&self, reference: &UiReference) -> Result<Capture> {
        let state = self.state.lock().expect("UI state poisoned");
        if reference.application != self.application
            || !rsi_ui_protocol::name_valid(&reference.name)
        {
            return Err(UiError::Retired);
        }
        let target = self.target_id(&state, &reference.target)?;
        let entry = state
            .entries
            .get(&reference.contribution)
            .filter(|entry| entry.position.is_admitting() && !entry.owner.stop.is_cancelled())
            .cloned()
            .ok_or(UiError::Retired)?;
        let tokens = (
            entry.owner.admit()?,
            target.owner.admit()?,
            self.owner.admit()?,
        );
        Ok(Capture {
            entry,
            target,
            _tokens: tokens,
        })
    }
    fn bind(&self, reference: &UiReference, capture: &Capture, view: UiView) -> Result<BoundView> {
        view.validate()?;
        let mut actions = BTreeMap::new();
        for element in &view.elements {
            if let UiElement::Button { action, .. } = element {
                if !capture.entry.value.actions.iter().any(|candidate| {
                    candidate.name == *action && candidate.target == capture.target.kind
                }) {
                    return Err(UiError::Invalid(
                        "view references an unavailable bundle action".into(),
                    ));
                }
                actions.insert(
                    action.clone(),
                    self.reference(&reference.target, &reference.contribution, action),
                );
            }
        }
        Ok(BoundView {
            reference: reference.clone(),
            view,
            actions,
        })
    }
    /// Renders a concrete surface, outside registry locks and under both owners.
    pub fn surface(&self, reference: &UiReference) -> Result<BoundView> {
        let capture = self.capture(reference)?;
        let surface = capture
            .entry
            .value
            .surfaces
            .iter()
            .find(|surface| surface.name == reference.name && surface.target == capture.target.kind)
            .ok_or(UiError::Retired)?;
        let view = surface.renderer.render(&capture.target.context)?;
        self.bind(reference, &capture, view)
    }
    /// Returns the first matching renderer in current declaration order.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned registry state.
    pub fn block(&self, handle: &UiTarget, block: &BlockInput<'_>) -> Result<Option<BoundView>> {
        if block.text.len() > MAXIMUM_VIEW_BYTES || block.key.len() > 4096 {
            return Err(UiError::Invalid(
                "block input window exceeds UI limit".into(),
            ));
        }
        let entries = {
            let state = self.state.lock().expect("UI state poisoned");
            self.target(&state, handle)?;
            Self::ordered(&state)?
        };
        for (id, entry) in entries {
            for renderer in &entry.value.renderers {
                if renderer.target != handle.kind {
                    continue;
                }
                let reference = self.reference(&handle.id, &id, &renderer.name);
                let Ok(capture) = self.capture(&reference) else {
                    continue;
                };
                if let Some(view) = renderer.renderer.render(&capture.target.context, block)? {
                    return self.bind(&reference, &capture, view).map(Some);
                }
            }
        }
        Ok(None)
    }

    /// Starts the first matching inline source under its exact registered owners.
    ///
    /// # Panics
    /// Panics if a prior panic poisoned the registry lock.
    pub fn present_inline(
        self: &Arc<Self>,
        handle: &UiTarget,
        block: &BlockInput<'_>,
    ) -> Result<Option<PresentationLease>> {
        if block.text.len() > MAXIMUM_VIEW_BYTES || block.key.len() > 4096 {
            return Err(UiError::Invalid(
                "block input window exceeds UI limit".into(),
            ));
        }
        let entries = {
            let state = self.state.lock().expect("UI state poisoned");
            self.target(&state, handle)?;
            Self::ordered(&state)?
        };
        for (id, entry) in entries {
            for renderer in &entry.value.renderers {
                if renderer.target != handle.kind {
                    continue;
                }
                let reference = self.reference(&handle.id, &id, &renderer.name);
                let Ok(capture) = self.capture(&reference) else {
                    continue;
                };
                if let Some(source) = renderer.renderer.inline(&capture.target.context, block)? {
                    return self.present_captured(&reference, capture, source).map(Some);
                }
            }
        }
        Ok(None)
    }
    /// Admits once before returning; dropping the waiter never cancels admitted work.
    pub fn invoke(
        self: &Arc<Self>,
        reference: &UiReference,
        input: ActionInput,
    ) -> BoxFuture<'static, Result<BoundView>> {
        self.invoke_in_view(reference, input, CancellationToken::new())
    }
    /// Admits owned work with a separate presentation-close signal for read handlers.
    /// Closing the view never automatically drops an admitted mutation.
    pub fn invoke_in_view(
        self: &Arc<Self>,
        reference: &UiReference,
        input: ActionInput,
        presentation_stop: CancellationToken,
    ) -> BoxFuture<'static, Result<BoundView>> {
        let admitted = (|| {
            input.validate()?;
            let capture = self.capture(reference)?;
            let handler = capture
                .entry
                .value
                .actions
                .iter()
                .find(|action| {
                    action.name == reference.name && action.target == capture.target.kind
                })
                .ok_or(UiError::Retired)?
                .handler
                .clone();
            let permit = self
                .slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| UiError::Capacity)?;
            Ok((capture, handler, permit))
        })();
        let (capture, handler, permit) = match admitted {
            Ok(value) => value,
            Err(error) => return Box::pin(async { Err(error) }),
        };
        let ui = self.clone();
        let reference = reference.clone();
        let task = self.execution.spawn(async move {
            let _permit = permit;
            let target = ActionTarget {
                context: capture.target.context.clone(),
                contribution_stop: capture.entry.owner.stop.clone(),
                target_stop: capture.target.owner.stop.clone(),
                presentation_stop,
                presentation: None,
            };
            let result = handler.invoke(target, input).await;
            let _ = capture.target.owner.invalidate();
            ui.bind(&reference, &capture, result?)
        });
        Box::pin(async move {
            task.await
                .map_err(|error| UiError::Action(error.to_string()))?
        })
    }
    pub(crate) fn retire(&self) {
        let state = self.state.lock().expect("UI state poisoned");
        self.owner.retire();
        self.slots.close();
        for entry in state.entries.values() {
            entry.owner.retire();
        }
        for target in state.targets.values() {
            target.owner.retire();
        }
        self.membership_changed();
    }
    pub(crate) async fn close(&self) {
        self.retire();
        self.owner.tasks.wait().await;
        let (entries, targets) = {
            let mut state = self.state.lock().expect("UI state poisoned");
            (
                std::mem::take(&mut state.entries),
                std::mem::take(&mut state.targets),
            )
        };
        drop((entries, targets));
    }
}
struct Capture {
    entry: Arc<Entry>,
    target: Arc<Target>,
    _tokens: (TaskTrackerToken, TaskTrackerToken, TaskTrackerToken),
}
fn validate_contributions(value: &Contributions) -> Result<()> {
    if !rsi_ui_protocol::name_valid(&value.name) {
        return Err(UiError::Invalid("invalid UI bundle name".into()));
    }
    for names in [
        value
            .surfaces
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        value
            .actions
            .iter()
            .map(|entry| entry.name.as_str())
            .collect(),
        value
            .renderers
            .iter()
            .map(|entry| entry.name.as_str())
            .collect(),
    ] {
        let mut seen = BTreeSet::new();
        if names.len() > MAXIMUM_CONTRIBUTIONS
            || names
                .into_iter()
                .any(|name| !rsi_ui_protocol::name_valid(name) || !seen.insert(name))
        {
            return Err(UiError::Invalid(
                "invalid or duplicate UI contribution name".into(),
            ));
        }
    }
    if value
        .surfaces
        .iter()
        .any(|surface| surface.title.is_empty() || surface.title.len() > 256)
    {
        return Err(UiError::Invalid("invalid UI surface title".into()));
    }
    Ok(())
}
pub(crate) fn meta(error: impl std::fmt::Display) -> rsi_meta::MetaError {
    rsi_meta::MetaError::Activation(error.to_string())
}
