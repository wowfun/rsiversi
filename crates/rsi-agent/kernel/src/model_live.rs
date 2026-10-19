//! Kernel-owned live working sets and coalescing snapshot channels.
use super::*;
use rsi_agent_session_protocol::{ModelAttemptPrefix, ModelEventPurpose};
use rsi_agent_turn_protocol::{
    ModelAttemptLease, ModelLiveSnapshot, ModelLiveStatus, ModelLiveStream,
};

pub(super) const ATTEMPT_BYTES: usize = 64 * 1024 * 1024;
type Channel = watch::Sender<Option<Arc<ModelLiveSnapshot>>>;
pub(super) struct Models {
    permits: Arc<Semaphore>,
    channels: Mutex<BTreeMap<SessionId, Weak<Channel>>>,
    active: Mutex<BTreeSet<SessionId>>,
}
impl Models {
    pub(super) fn new(bytes: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(bytes / ATTEMPT_BYTES)),
            channels: Mutex::default(),
            active: Mutex::default(),
        }
    }
    fn channel(&self, session: &SessionId) -> Arc<Channel> {
        let mut channels = self
            .channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        channels.retain(|_, channel| channel.strong_count() > 0);
        if let Some(channel) = channels.get(session).and_then(Weak::upgrade) {
            return channel;
        }
        let channel = Arc::new(watch::channel(None).0);
        channels.insert(session.clone(), Arc::downgrade(&channel));
        channel
    }
}
struct Lease {
    inner: Arc<KernelInner>,
    claim: TurnClaim,
    channel: Arc<Channel>,
    snapshot: ModelLiveSnapshot,
    began: Option<u64>,
    elapsed: std::sync::atomic::AtomicU64,
    committed: bool,
    _permit: OwnedSemaphorePermit,
}
impl fmt::Debug for Lease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelAttemptLease")
            .field("effect", &self.snapshot.effect_id)
            .finish_non_exhaustive()
    }
}
impl Lease {
    fn announce(&mut self) -> TurnResult<()> {
        self.snapshot.revision = self
            .snapshot
            .revision
            .checked_add(1)
            .ok_or_else(|| TurnError::Invariant("live model revision overflow".into()))?;
        self.channel
            .send_replace(Some(Arc::new(self.snapshot.clone())));
        Ok(())
    }
}
impl ModelAttemptLease for Lease {
    fn started(&mut self) -> TurnResult<()> {
        if self.began.is_some() {
            return Err(TurnError::Invalid("model clock already started".into()));
        }
        let kernel = AgentKernel {
            inner: self.inner.clone(),
        };
        let state = lock_state(&self.inner);
        let turn = kernel.validate_claim(&state, &self.claim)?;
        if !matches!(turn.effects.get(&self.snapshot.effect_id), Some(ActiveEffect::Model { started: true, purpose, .. }) if purpose == &self.snapshot.purpose)
        {
            return Err(TurnError::Invalid(
                "model clock requires exact started attempt".into(),
            ));
        }
        self.began = Some(self.inner.clock.now_ms());
        Ok(())
    }
    fn elapsed_ms(&self) -> u64 {
        let elapsed = self
            .began
            .map_or(0, |began| self.inner.clock.now_ms().saturating_sub(began));
        self.elapsed
            .fetch_max(elapsed, Ordering::Relaxed)
            .max(elapsed)
    }
    fn publish(&mut self, prefix: ModelAttemptPrefix) -> TurnResult<()> {
        if self.began.is_none() {
            return Err(TurnError::Invalid("model clock has not started".into()));
        }
        let kernel = AgentKernel {
            inner: self.inner.clone(),
        };
        {
            let state = lock_state(&self.inner);
            let turn = kernel.validate_claim(&state, &self.claim)?;
            match turn.effects.get(&self.snapshot.effect_id) {
                Some(ActiveEffect::Model {
                    started: true,
                    purpose,
                    ..
                }) if purpose == &self.snapshot.purpose => {}
                _ => {
                    return Err(TurnError::Invalid(
                        "live prefix has no exact started model attempt".into(),
                    ));
                }
            }
        }
        if prefix.event_count() < self.snapshot.prefix.event_count() || self.committed {
            return Err(TurnError::Invalid(
                "live prefix revision moved backwards or followed commit".into(),
            ));
        }
        self.snapshot.prefix = prefix;
        self.announce()
    }
    fn committed(&mut self, receipt: &rsi_agent_turn_protocol::PublishedFacts) -> TurnResult<()> {
        if receipt.claim() != &self.claim {
            return Err(TurnError::StaleClaim);
        }
        let fact_seq = {
            let kernel = AgentKernel {
                inner: self.inner.clone(),
            };
            let state = lock_state(&self.inner);
            // Equality binds this lease; validate the receipt's issuer and resident
            // header identity once under the same lock as the durable watermark.
            kernel.validate_claim(&state, receipt.claim())?;
            let mut matches = receipt.facts().iter().filter(|fact| matches!(fact.body(),
                SessionFactBody::ModelAttemptSettled { turn_id, effect_id, purpose, .. }
                if turn_id == self.claim.turn_id() && effect_id == &self.snapshot.effect_id && purpose == &self.snapshot.purpose));
            let fact = matches.next().ok_or_else(|| {
                TurnError::Invalid("model completion has no exact settlement".into())
            })?;
            if self.committed
                || matches.next().is_some()
                || state.sessions[self.claim.session_id()].durable_seq < fact.seq()
            {
                return Err(TurnError::Invalid(
                    "model completion has no exact durable settlement".into(),
                ));
            }
            fact.seq()
        };
        self.snapshot.status = ModelLiveStatus::Committed { fact_seq };
        self.snapshot.prefix = ModelAttemptPrefix::default();
        self.committed = true;
        self.announce()
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if !self.committed {
            self.snapshot.status = ModelLiveStatus::Abandoned;
            self.snapshot.prefix = ModelAttemptPrefix::default();
            let _ = self.announce();
        }
        self.inner
            .model_live
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(self.claim.session_id());
    }
}
pub(super) async fn reserve(
    kernel: &AgentKernel,
    claim: &TurnClaim,
    effect_id: EffectId,
    purpose: ModelEventPurpose,
    cancellation: CancellationToken,
) -> TurnResult<Box<dyn ModelAttemptLease>> {
    kernel.validate_claim(&lock_state(&kernel.inner), claim)?;
    let permit = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(TurnError::Cancelled),
        result = tokio::time::timeout(Duration::from_mins(1), kernel.inner.model_live.permits.clone().acquire_owned()) =>
            result.map_err(|_| TurnError::Capacity)?.map_err(|_| TurnError::ShuttingDown)?,
    };
    kernel.validate_claim(&lock_state(&kernel.inner), claim)?;
    let channel = kernel.inner.model_live.channel(claim.session_id());
    if !kernel
        .inner
        .model_live
        .active
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(claim.session_id().clone())
    {
        return Err(TurnError::Invalid(
            "Session already owns a live model reservation".into(),
        ));
    }
    let mut lease = Lease {
        inner: kernel.inner.clone(),
        claim: claim.clone(),
        channel,
        snapshot: ModelLiveSnapshot {
            session_id: claim.session_id().clone(),
            turn_id: claim.turn_id().clone(),
            effect_id,
            purpose,
            revision: 0,
            prefix: ModelAttemptPrefix::default(),
            status: ModelLiveStatus::Streaming,
        },
        began: None,
        elapsed: std::sync::atomic::AtomicU64::new(0),
        committed: false,
        _permit: permit,
    };
    lease.announce()?;
    Ok(Box::new(lease))
}
pub(super) fn observe(
    inner: &Arc<KernelInner>,
    session: &SessionId,
) -> TurnResult<ModelLiveStream> {
    let lease = ObserverLease::acquire(inner, ObserverKind::Session)?;
    let channel = inner.model_live.channel(session);
    let receiver = channel.subscribe();
    let state = (Arc::downgrade(inner), channel, receiver, lease, true);
    Ok(stream::unfold(
        state,
        |(weak, channel, mut receiver, lease, mut initial)| async move {
            loop {
                let inner = weak.upgrade()?;
                if !initial {
                    tokio::select! {
                        () = inner.stop_worker.cancelled() => return None,
                        result = receiver.changed() => { if result.is_err() { return None; } }
                    }
                }
                initial = false;
                let snapshot = receiver.borrow_and_update().clone();
                if let Some(snapshot) = snapshot {
                    let item = inner.observation_retention.retain_model(snapshot);
                    return Some((item, (weak, channel, receiver, lease, false)));
                }
            }
        },
    )
    .boxed())
}
