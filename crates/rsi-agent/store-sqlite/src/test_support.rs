//! Opt-in deterministic validation instrumentation for adapter consumers.

use super::*;

pub use crate::cas::{
    PublicationPhase as CasPublicationPhase, PublicationStep as CasPublicationStep,
};

/// Releases the paused Store worker on explicit or unwinding drop.
#[derive(Debug)]
pub struct WorkerPause(Option<std::sync::mpsc::Sender<()>>);

impl Drop for WorkerPause {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            let _ = release.send(());
        }
    }
}

impl SqliteStore {
    /// Fails one CAS operation before execution or after successful execution.
    ///
    /// # Panics
    /// Panics if another unconsumed fault is installed.
    pub fn fail_next_cas_publication(&self, step: CasPublicationStep, phase: CasPublicationPhase) {
        self.inner.cas_publication.inject(step, phase);
    }
    /// Pauses the next cold validation after dispatch; dropping the guard releases it.
    ///
    /// # Panics
    /// Panics if another pause is installed or the test barrier mutex is poisoned.
    pub fn pause_next_validation(&self) -> (tokio::sync::oneshot::Receiver<()>, WorkerPause) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let previous = self
            .inner
            .validation_barrier
            .lock()
            .unwrap()
            .replace((entered_tx, release_rx));
        assert!(
            previous.is_none(),
            "only one validation pause may be installed"
        );
        (entered_rx, WorkerPause(Some(release_tx)))
    }

    /// Pauses the next materialized Fact page after its database snapshot closes.
    ///
    /// # Panics
    /// Panics if a Fact-page pause is already installed or its mutex is poisoned.
    pub fn pause_next_fact_page(&self) -> (tokio::sync::oneshot::Receiver<()>, WorkerPause) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let previous = self
            .inner
            .fact_page_barrier
            .lock()
            .unwrap()
            .replace((entered_tx, release_rx));
        assert!(
            previous.is_none(),
            "only one Fact-page pause may be installed"
        );
        (entered_rx, WorkerPause(Some(release_tx)))
    }

    /// Returns completed/entered validations, materialized Facts, and decoded controls.
    pub fn validation_counts(&self) -> (u64, u64, u64) {
        (
            self.inner.validation_runs.load(Ordering::Relaxed),
            self.inner.fact_materializations.load(Ordering::Relaxed),
            self.inner.control_decodes.load(Ordering::Relaxed),
        )
    }
}

/// Timing observations for one completed Store lane operation. Nanoseconds are
/// diagnostic samples, not deterministic performance gates or pure `SQLite` CPU time.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct LaneMeasurement {
    /// Diagnostic Rust result type for distinguishing operation samples.
    /// This label is neither unique nor stable across compiler versions.
    pub result_type: &'static str,
    /// Time waiting for the selected lane permit.
    pub admission_ns: u128,
    /// Time from dispatch to the blocking worker starting.
    pub scheduling_ns: u128,
    /// Complete worker operation, including SQL, row extraction and validation.
    pub worker_ns: u128,
    /// JSON decoding inside that operation; remaining work includes SQL and paging.
    pub json_decode_ns: u128,
}
thread_local! { static DECODE_NS: std::cell::Cell<Option<u128>> = const { std::cell::Cell::new(None) }; }
type Measurements = Arc<Mutex<Option<Vec<LaneMeasurement>>>>;
pub(super) struct LaneProbe {
    measurements: Measurements,
    started: std::time::Instant,
    value: LaneMeasurement,
}
impl LaneProbe {
    pub(super) fn start(
        measurements: Option<Measurements>,
        queued: std::time::Instant,
        dispatched: std::time::Instant,
        result_type: &'static str,
    ) -> Option<Self> {
        let measurements = measurements?;
        if measurements.lock().unwrap().is_none() {
            return None;
        }
        let started = std::time::Instant::now();
        DECODE_NS.with(|value| value.set(Some(0)));
        Some(Self {
            measurements,
            started,
            value: LaneMeasurement {
                result_type,
                admission_ns: dispatched.duration_since(queued).as_nanos(),
                scheduling_ns: started.duration_since(dispatched).as_nanos(),
                worker_ns: 0,
                json_decode_ns: 0,
            },
        })
    }
}
impl Drop for LaneProbe {
    fn drop(&mut self) {
        self.value.worker_ns = self.started.elapsed().as_nanos();
        self.value.json_decode_ns = DECODE_NS.with(|value| value.take().unwrap_or(0));
        if let Some(values) = self.measurements.lock().unwrap().as_mut() {
            // Instrumentation itself cannot accumulate an unbounded history.
            if values.len() < 4096 {
                values.push(self.value);
            }
        }
    }
}
pub(super) struct DecodeProbe(Option<std::time::Instant>);
impl DecodeProbe {
    pub(super) fn start() -> Self {
        Self(DECODE_NS.with(|value| value.get().map(|_| std::time::Instant::now())))
    }
}
impl Drop for DecodeProbe {
    fn drop(&mut self) {
        if let Some(started) = self.0 {
            DECODE_NS.with(|value| {
                if let Some(prior) = value.get() {
                    value.set(Some(prior + started.elapsed().as_nanos()));
                }
            });
        }
    }
}
impl SqliteStore {
    /// Starts a bounded diagnostic capture of subsequent normal reader operations.
    ///
    /// # Panics
    /// Panics if the test instrumentation mutex was poisoned.
    pub fn begin_reader_measurements(&self) {
        *self.inner.reader_measurements.lock().unwrap() = Some(Vec::new());
    }
    /// Stops capture and returns the first 4096 completed observations, or fewer.
    /// Excess completions are dropped until capture stops; this is a prefix sample.
    ///
    /// # Panics
    /// Panics if the test instrumentation mutex was poisoned.
    pub fn take_reader_measurements(&self) -> Vec<LaneMeasurement> {
        self.inner
            .reader_measurements
            .lock()
            .unwrap()
            .take()
            .unwrap_or_default()
    }
}

/// Bounded diagnostic samples, separated by independently admitted Store lanes.
#[derive(Debug, serde::Serialize)]
pub struct StoreMeasurements {
    /// Normal foreground reads.
    pub reader: Vec<LaneMeasurement>,
    /// Serialized writes.
    pub writer: Vec<LaneMeasurement>,
    /// Cold validation/proof operations.
    pub validation: Vec<LaneMeasurement>,
    /// Filesystem CAS publication and payload reads.
    pub cas: Vec<LaneMeasurement>,
}
impl SqliteStore {
    /// Starts bounded captures of reader, writer, cold-validation and CAS workers.
    ///
    /// # Panics
    /// Panics if a diagnostic mutex is poisoned.
    pub fn begin_store_measurements(&self) {
        self.begin_reader_measurements();
        *self.inner.writer_measurements.lock().unwrap() = Some(Vec::new());
        *self.inner.validation_measurements.lock().unwrap() = Some(Vec::new());
        *self.inner.cas_measurements.lock().unwrap() = Some(Vec::new());
    }
    /// Stops capture and returns the first 4096 completed observations per lane, or fewer.
    /// Excess completions are dropped until capture stops; these are prefix samples.
    ///
    /// # Panics
    /// Panics if a diagnostic mutex is poisoned.
    pub fn take_store_measurements(&self) -> StoreMeasurements {
        StoreMeasurements {
            reader: self.take_reader_measurements(),
            writer: self
                .inner
                .writer_measurements
                .lock()
                .unwrap()
                .take()
                .unwrap_or_default(),
            validation: self
                .inner
                .validation_measurements
                .lock()
                .unwrap()
                .take()
                .unwrap_or_default(),
            cas: self
                .inner
                .cas_measurements
                .lock()
                .unwrap()
                .take()
                .unwrap_or_default(),
        }
    }
}
