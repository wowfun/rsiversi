//! Bounded dispatch of one source-ordered run of parallel-safe calls.
use super::*;
use futures_util::stream::FuturesUnordered;
use tokio::sync::{mpsc, watch};

type DriveResult<T> = std::result::Result<T, DriveFailure>;
type Completion = (
    usize,
    Arc<SessionFact>,
    ToolResultIdentity,
    DriveResult<ToolResult>,
);
const RESULT_QUEUE_BYTES: usize = 4 * 1024 * 1024;

struct Batch<'a> {
    driver: &'a Driver,
    source: &'a EffectId,
    claim: &'a TurnClaim,
    composition: &'a AgentCompositionPin,
    jobs: Option<&'a JobScopeAuthority>,
    policy: ResolvedTurnPolicy,
    cancellation: &'a CancellationToken,
    stop: &'a CancellationToken,
}

enum Outcome {
    Rejected(Box<SessionFactBody>),
    Returned(Arc<SessionFact>, ToolResult),
    Retained(Arc<SessionFact>),
    Failed,
}

#[derive(Default)]
struct OrderedResults {
    ready: BTreeMap<usize, (Outcome, usize)>,
    next: usize,
    bytes: usize,
    retained: usize,
    failure: Option<DriveFailure>,
    published: Vec<Arc<SessionFact>>,
}

impl OrderedResults {
    fn receive(&mut self, batch: &Batch<'_>, (index, intent, identity, result): Completion) {
        let (outcome, bytes) = match result {
            Ok(result) if index == self.next => (Outcome::Returned(intent, result), 0),
            Ok(result) => {
                if let Some(bytes) = result.encoded_bytes_within(RESULT_QUEUE_BYTES - self.bytes) {
                    (Outcome::Returned(intent, result), bytes)
                } else {
                    self.retained += 1;
                    (Outcome::Retained(intent), 0)
                }
            }
            Err(error) => {
                if matches!(
                    batch.composition.tools().query(&identity),
                    Ok(RetainedToolResult::Absent)
                ) {
                    batch.driver.clear_tracked_tool(batch.claim, &identity);
                }
                self.failure.get_or_insert(error);
                (Outcome::Failed, 0)
            }
        };
        self.bytes += bytes;
        self.ready.insert(index, (outcome, bytes));
    }

    async fn publish(&mut self, batch: &Batch<'_>, fold: &mut ModelContextState) {
        while let Some((outcome, bytes)) = self.ready.remove(&self.next) {
            self.next += 1;
            self.bytes -= bytes;
            let result = match outcome {
                Outcome::Rejected(body) => batch
                    .driver
                    .publish_durable(batch.claim, fold, vec![*body])
                    .await
                    .map(|receipt| self.published.extend(receipt.into_facts())),
                Outcome::Returned(intent, result) => batch
                    .driver
                    .publish_tool_result(batch.claim, batch.composition, fold, intent, result)
                    .await
                    .map(|fact| self.published.push(fact)),
                Outcome::Retained(intent) => {
                    self.retained -= 1;
                    let SessionFactBody::ToolIntent { identity, .. } = intent.body() else {
                        unreachable!("a prepared Tool effect carries its published intent")
                    };
                    match batch.composition.tools().query(identity) {
                        Ok(RetainedToolResult::Returned(result)) => batch
                            .driver
                            .publish_tool_result(
                                batch.claim,
                                batch.composition,
                                fold,
                                intent,
                                result,
                            )
                            .await
                            .map(|fact| self.published.push(fact)),
                        Ok(_) => Err(DriveFailure::Turn(TurnOutcome::Interrupted {
                            effect: Some(EffectKind::Tool),
                            reason:
                                "completed parallel Tool result unavailable from its pinned owner"
                                    .into(),
                        })),
                        Err(error) => Err(tool_failure(&error)),
                    }
                }
                Outcome::Failed => continue,
            };
            if let Err(error) = result {
                self.failure.get_or_insert(error);
            }
        }
    }
}

impl Batch<'_> {
    async fn execute(&self, index: usize, effect: PreparedToolEffect) -> Completion {
        let result = self
            .driver
            .start_tool_raw(
                &effect.effect_id,
                effect.prepared,
                &effect.identity,
                self.composition,
                self.claim,
                self.jobs,
                ToolScheduling::ParallelSafe,
                self.policy.sandbox,
                self.cancellation,
                self.stop,
                None,
            )
            .await;
        (index, effect.intent, effect.identity, result)
    }

    async fn prepare(
        &self,
        calls: Vec<ModelToolCall>,
        mut admitted: watch::Receiver<usize>,
        ready: mpsc::Sender<(usize, DriveResult<ToolPreparation>)>,
    ) {
        let origin = rsi_agent_session_protocol::ToolOrigin::Model {
            effect_id: self.source.clone(),
        };
        for (index, call) in calls.into_iter().enumerate() {
            if admitted.wait_for(|end| index < *end).await.is_err() {
                return;
            }
            let outcome = self
                .driver
                .prepare_tool_call(
                    &origin,
                    self.claim,
                    self.composition,
                    call,
                    ToolScheduling::ParallelSafe,
                    self.policy,
                    self.cancellation,
                    self.stop,
                )
                .await;
            let failed = outcome.is_err();
            if ready.send((index, outcome)).await.is_err() || failed {
                return;
            }
        }
    }

    async fn run(
        &self,
        calls: Vec<ModelToolCall>,
        fold: &mut ModelContextState,
    ) -> DriveResult<Vec<Arc<SessionFact>>> {
        let limit = self.driver.config.maximum_parallel_tool_calls;
        let count = calls.len();
        let mut running = FuturesUnordered::new();
        let mut results = OrderedResults::default();
        {
            let (admission, horizon) = watch::channel(0);
            let (sender, mut ready) = mpsc::channel(limit);
            let preparation = self.prepare(calls, horizon, sender);
            let dispatch = async {
                let mut admitted = 0;
                let mut received = 0;
                let mut wave = Vec::new();
                loop {
                    results.publish(self, fold).await;
                    if self.stop.is_cancelled() {
                        results.failure.get_or_insert(DriveFailure::Stopped);
                    } else if self.cancellation.is_cancelled() {
                        results
                            .failure
                            .get_or_insert(DriveFailure::Turn(TurnOutcome::Cancelled));
                    }
                    if results.failure.is_some() {
                        break;
                    }
                    let occupied = running.len() + (admitted - received);
                    let free = (limit - occupied).min(2 * limit - occupied - results.ready.len());
                    if admitted < count
                        && free > 0
                        && results.retained == 0
                        && results.bytes < RESULT_QUEUE_BYTES
                    {
                        admitted += free.min(count - admitted);
                        admission.send_replace(admitted);
                    }
                    tokio::select! { biased;
                        Some(completed) = running.next() => {
                            results.receive(self, completed);
                            continue;
                        }
                        n = ready.recv_many(&mut wave, admitted - received), if admitted > received => {
                            if n == 0 { break; }
                            received += n;
                        }
                        else => break,
                    }
                    let mut pending = Vec::with_capacity(wave.len());
                    for (index, outcome) in wave.drain(..) {
                        match outcome {
                            Ok(ToolPreparation::Ready(effect)) => pending.push((index, effect)),
                            Ok(ToolPreparation::Rejected(body)) => {
                                results.ready.insert(index, (Outcome::Rejected(body), 0));
                            }
                            Err(error) => {
                                results.failure = Some(error);
                            }
                        }
                    }
                    if results.failure.is_some()
                        || pending.is_empty()
                        || self.stop.is_cancelled()
                        || self.cancellation.is_cancelled()
                    {
                        continue;
                    }
                    match self
                        .driver
                        .publish_parallel_tool_starts(self.claim, self.composition, fold, pending)
                        .await
                    {
                        Ok(effects) => running.extend(
                            effects
                                .into_iter()
                                .map(|(index, effect)| self.execute(index, effect)),
                        ),
                        Err(error) => {
                            results.failure = Some(error);
                        }
                    }
                }
            };
            // Preparation and publication both use Session admission. Keep polling
            // them on this task while either is waiting on the other's transaction.
            tokio::pin!(dispatch);
            tokio::select! {
                () = &mut dispatch => {},
                () = preparation => dispatch.await,
            }
        }
        while let Some(completed) = running.next().await {
            results.receive(self, completed);
            results.publish(self, fold).await;
        }
        results.failure.map_or(Ok(results.published), Err)
    }
}

impl Driver {
    #[allow(clippy::too_many_arguments)] // All calls share the same pinned Turn authorities.
    pub(super) async fn run_parallel_tools(
        &self,
        source: &EffectId,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        jobs: Option<&JobScopeAuthority>,
        fold: &mut ModelContextState,
        calls: Vec<ModelToolCall>,
        policy: ResolvedTurnPolicy,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> DriveResult<Vec<Arc<SessionFact>>> {
        Batch {
            driver: self,
            source,
            claim,
            composition,
            jobs,
            policy,
            cancellation,
            stop,
        }
        .run(calls, fold)
        .await
    }
}
