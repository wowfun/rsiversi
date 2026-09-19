//! Opt-in synchronous frame measurements for the calling thread.
use std::cell::Cell;
/// One actual `next_frame` operation; times are diagnostic nanoseconds.
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct FrameMeasurement {
    /// Complete frame-stream lock hold time, excluding lock acquisition.
    pub stream_lock_ns: u128,
    /// Number of complete global JSON materializations.
    pub section_materializations: usize,
    /// Global JSON materialization time, including detail-lock acquisition.
    pub section_materialization_ns: u128,
    /// Complete global JSON bytes traversed by the counting writer.
    pub section_counted_bytes: usize,
    /// Counting-writer elapsed time.
    pub section_count_ns: u128,
    /// Global top-level fields compared against the baseline.
    pub section_compared_fields: usize,
    /// Global field comparison elapsed time; includes nested value equality.
    pub section_comparison_ns: u128,
}
thread_local! { static LAST: Cell<FrameMeasurement> = Cell::new(FrameMeasurement::default()); }
pub(crate) fn update(edit: impl FnOnce(&mut FrameMeasurement)) {
    LAST.with(|last| {
        let mut value = last.get();
        edit(&mut value);
        last.set(value);
    });
}
/// Consumes the last synchronous frame sample on this calling thread.
pub fn take_frame_measurement() -> FrameMeasurement {
    LAST.with(Cell::take)
}
