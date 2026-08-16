use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::host::{RetirementEntry, RetirementRegistry};
use crate::model::Generation;
use crate::runtime::RuntimeHandle;
use tokio::sync::watch;

pub(super) async fn retire_generation(
    generation: &Arc<Generation>,
    runtime: &RuntimeHandle,
    phase: &AtomicU8,
) {
    generation.wait_for_lease_drain().await;
    phase.store(1, Ordering::Release);
    if runtime.retire().await.is_ok() && runtime.wait_retired().await.is_ok() {
        phase.store(2, Ordering::Release);
        let _ = runtime.stop().await;
        return;
    }
    phase.store(2, Ordering::Release);
    let _ = runtime.stop().await;
}

pub(super) fn register_retirement_waves(
    retirements: &RetirementRegistry,
    waves: Vec<Vec<Arc<Generation>>>,
) {
    let mut registered_waves = Vec::with_capacity(waves.len());
    for wave in waves {
        let mut registered = Vec::with_capacity(wave.len());
        for generation in wave {
            generation.stop_admission();
            let key = (generation.instance.clone(), generation.id);
            let phase = Arc::new(AtomicU8::new(0));
            let (cancel, cancelled) = watch::channel(false);
            let (done_sender, done) = watch::channel(false);
            retirements
                .lock()
                .expect("retirement registry mutex poisoned")
                .insert(
                    key.clone(),
                    RetirementEntry {
                        generation: Arc::clone(&generation),
                        phase: Arc::clone(&phase),
                        cancel,
                        done,
                    },
                );
            registered.push((key, generation, phase, cancelled, done_sender));
        }
        registered_waves.push(registered);
    }
    let retirements = Arc::clone(retirements);
    tokio::spawn(async move {
        for wave in registered_waves {
            futures_util::future::join_all(wave.into_iter().map(
                |(key, generation, phase, mut cancelled, done_sender)| {
                    let retirements = Arc::clone(&retirements);
                    async move {
                        if let Ok(runtime) = generation.runtime().cloned() {
                            let normal = retire_generation(&generation, &runtime, &phase);
                            tokio::pin!(normal);
                            tokio::select! {
                                () = &mut normal => {}
                                changed = cancelled.changed() => {
                                    if changed.is_ok() && *cancelled.borrow() {
                                        phase.store(2, Ordering::Release);
                                        let _ = runtime.stop().await;
                                    }
                                }
                            }
                        }
                        retirements
                            .lock()
                            .expect("retirement registry mutex poisoned")
                            .remove(&key);
                        done_sender.send_replace(true);
                    }
                },
            ))
            .await;
        }
    });
}
