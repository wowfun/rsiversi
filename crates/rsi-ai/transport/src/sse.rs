use crate::{
    ByteStream, MAX_HTTP_RESPONSE_ITEM_BYTES, MAX_PROVIDER_SSE_FRAME_BYTES, SseTermination,
    TransportError,
};
use async_stream::stream;
use futures_util::{Stream, StreamExt as _};
use memchr::{memchr, memchr2};
use std::{
    collections::BTreeMap,
    fmt,
    pin::Pin,
    sync::{Arc, LazyLock, Mutex},
};
use tokio::sync::Notify;

const ADMISSION_UNIT_BYTES: usize = 256 * 1024;
const MAXIMUM_ADMISSION_UNITS: usize = MAX_PROVIDER_SSE_FRAME_BYTES / ADMISSION_UNIT_BYTES;
const MAXIMUM_ADMISSION_CLAIMS: usize = 1_024;
const _: () = assert!(MAX_PROVIDER_SSE_FRAME_BYTES.is_multiple_of(ADMISSION_UNIT_BYTES));

static FRAME_ADMISSION: LazyLock<Arc<AdmissionPool>> = LazyLock::new(|| {
    Arc::new(AdmissionPool::new(
        ADMISSION_UNIT_BYTES,
        MAXIMUM_ADMISSION_UNITS,
    ))
});

/// Pull-based decoded SSE `data` fields.
pub type SseStream = Pin<Box<dyn Stream<Item = Result<SseData, TransportError>> + Send + 'static>>;

/// One decoded SSE `data` value that retains its actual byte admission.
pub struct SseData {
    data: String,
    _admission: FrameLease,
}

impl SseData {
    /// Returns the decoded `data` field without transferring its admission.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.data
    }
}

impl fmt::Debug for SseData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SseData")
            .field("bytes", &self.data.len())
            .finish_non_exhaustive()
    }
}

/// Decodes SSE framing incrementally under one provider-selected finite frame bound.
#[allow(clippy::too_many_lines)] // One state machine owns cross-chunk CR/LF and frame grammar.
pub fn decode_sse(
    mut body: ByteStream,
    termination: SseTermination,
    maximum_frame_bytes: usize,
) -> SseStream {
    Box::pin(stream! {
        if let Err(error) = validate_frame_limit(maximum_frame_bytes) {
            yield Err(error);
            return;
        }
        let mut admission = match FrameAdmission::begin(
            Arc::clone(&FRAME_ADMISSION),
            maximum_frame_bytes,
        ).await {
            Ok(admission) => Some(admission),
            Err(error) => {
                yield Err(error);
                return;
            }
        };
        let mut frame = Vec::new();
        let mut line_start = 0_usize;
        let mut previous_was_cr = false;
        let mut done = false;
        while let Some(chunk) = body.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    yield Err(error);
                    return;
                }
            };
            if chunk.len() > MAX_HTTP_RESPONSE_ITEM_BYTES {
                yield Err(TransportError::new(
                    "sse.transport_item_too_large",
                    format!(
                        "SSE transport item exceeds {MAX_HTTP_RESPONSE_ITEM_BYTES} bytes"
                    ),
                ));
                return;
            }
            if chunk.is_empty() {
                continue;
            }
            let mut offset = 0;
            if previous_was_cr && chunk.first() == Some(&b'\n') {
                offset = 1;
            }
            previous_was_cr = false;
            while offset < chunk.len() {
                let Some(relative) = memchr2(b'\r', b'\n', &chunk[offset..]) else {
                    let projected = frame
                        .len()
                        .saturating_add(chunk.len() - offset);
                    if projected > maximum_frame_bytes {
                        yield Err(frame_too_large(maximum_frame_bytes));
                        return;
                    }
                    if let Err(error) = prepare_frame_capacity(
                        &mut frame,
                        &mut admission,
                        projected,
                        maximum_frame_bytes,
                    ).await {
                        yield Err(error);
                        return;
                    }
                    frame.extend_from_slice(&chunk[offset..]);
                    break;
                };
                let terminator = offset + relative;
                let projected_line = frame.len().saturating_add(terminator - offset);
                if projected_line > maximum_frame_bytes {
                    yield Err(frame_too_large(maximum_frame_bytes));
                    return;
                }
                if let Err(error) = prepare_frame_capacity(
                    &mut frame,
                    &mut admission,
                    projected_line,
                    maximum_frame_bytes,
                ).await {
                    yield Err(error);
                    return;
                }
                frame.extend_from_slice(&chunk[offset..terminator]);
                if frame.len() == line_start {
                    let complete = std::mem::take(&mut frame);
                    line_start = 0;
                    let decoded = match decode_sse_frame(complete) {
                        Ok(decoded) => decoded,
                        Err(error) => {
                            yield Err(error);
                            return;
                        }
                    };
                    match decoded {
                        Some(data)
                            if termination == SseTermination::DoneSentinel
                                && data == "[DONE]" =>
                        {
                            drop(admission.take());
                            done = true;
                            break;
                        }
                        Some(mut data) => {
                            let Some(frame_admission) = admission.take() else {
                                yield Err(invalid_admission_state());
                                return;
                            };
                            data.shrink_to_fit();
                            let lease = frame_admission.seal(data.capacity());
                            yield Ok(SseData {
                                data,
                                _admission: lease,
                            });
                        }
                        None => drop(admission.take()),
                    }
                    admission = match FrameAdmission::begin(
                        Arc::clone(&FRAME_ADMISSION),
                        maximum_frame_bytes,
                    ).await {
                        Ok(next) => Some(next),
                        Err(error) => {
                            yield Err(error);
                            return;
                        }
                    };
                } else {
                    let projected_frame = frame.len().saturating_add(1);
                    if projected_frame > maximum_frame_bytes {
                        yield Err(frame_too_large(maximum_frame_bytes));
                        return;
                    }
                    if let Err(error) = prepare_frame_capacity(
                        &mut frame,
                        &mut admission,
                        projected_frame,
                        maximum_frame_bytes,
                    ).await {
                        yield Err(error);
                        return;
                    }
                    frame.push(b'\n');
                    line_start = frame.len();
                }
                previous_was_cr = chunk[terminator] == b'\r';
                offset = terminator + 1;
                if previous_was_cr && chunk.get(offset) == Some(&b'\n') {
                    offset += 1;
                    previous_was_cr = false;
                }
            }
            if done {
                break;
            }
        }
        if done {
            return;
        }
        if !frame.is_empty() {
            yield Err(TransportError::new(
                "sse.incomplete_frame",
                "SSE stream ended inside a frame",
            ));
            return;
        }
        if termination == SseTermination::DoneSentinel {
            yield Err(TransportError::new(
                "sse.missing_done",
                "SSE stream ended without [DONE]",
            ));
        }
    })
}

async fn prepare_frame_capacity(
    frame: &mut Vec<u8>,
    admission: &mut Option<FrameAdmission>,
    projected_len: usize,
    maximum_frame_bytes: usize,
) -> Result<(), TransportError> {
    let capacity = next_frame_capacity(frame.capacity(), projected_len, maximum_frame_bytes);
    ensure_admitted(admission, capacity.max(1)).await?;
    if frame.capacity() < capacity {
        frame.reserve_exact(capacity - frame.len());
    }
    Ok(())
}

fn next_frame_capacity(
    current_capacity: usize,
    projected_len: usize,
    maximum_frame_bytes: usize,
) -> usize {
    if projected_len <= current_capacity {
        return current_capacity;
    }
    let rounded_projected = projected_len.div_ceil(ADMISSION_UNIT_BYTES) * ADMISSION_UNIT_BYTES;
    let rounded_maximum = maximum_frame_bytes.div_ceil(ADMISSION_UNIT_BYTES) * ADMISSION_UNIT_BYTES;
    current_capacity
        .checked_mul(2)
        .unwrap_or(rounded_maximum)
        .max(rounded_projected)
        .min(rounded_maximum)
}

async fn ensure_admitted(
    admission: &mut Option<FrameAdmission>,
    projected_bytes: usize,
) -> Result<(), TransportError> {
    let admission = admission.as_mut().ok_or_else(invalid_admission_state)?;
    admission.ensure_bytes(projected_bytes).await
}

fn invalid_admission_state() -> TransportError {
    TransportError::new(
        "sse.invalid_admission_state",
        "SSE decoder lost the current frame admission",
    )
}

fn validate_frame_limit(maximum_frame_bytes: usize) -> Result<(), TransportError> {
    if maximum_frame_bytes == 0 || maximum_frame_bytes > MAX_PROVIDER_SSE_FRAME_BYTES {
        return Err(TransportError::new(
            "sse.invalid_frame_limit",
            format!("SSE frame limit must be between 1 and {MAX_PROVIDER_SSE_FRAME_BYTES} bytes"),
        ));
    }
    Ok(())
}

fn frame_too_large(maximum_frame_bytes: usize) -> TransportError {
    TransportError::new(
        "sse.frame_too_large",
        format!("SSE frame exceeds {maximum_frame_bytes} bytes"),
    )
}

fn decode_sse_frame(mut frame: Vec<u8>) -> Result<Option<String>, TransportError> {
    std::str::from_utf8(&frame)
        .map_err(|_| TransportError::new("sse.invalid_utf8", "SSE frame is not valid UTF-8"))?;
    let mut read = 0;
    let mut write = 0;
    let mut found = false;
    while read < frame.len() {
        let line_end =
            memchr(b'\n', &frame[read..]).map_or(frame.len(), |relative| read + relative);
        let line = &frame[read..line_end];
        if line.starts_with(b"data:") {
            let mut value_start = read + b"data:".len();
            if frame.get(value_start) == Some(&b' ') {
                value_start += 1;
            }
            if found {
                frame[write] = b'\n';
                write += 1;
            }
            frame.copy_within(value_start..line_end, write);
            write += line_end - value_start;
            found = true;
        }
        read = line_end.saturating_add(1);
    }
    if !found {
        return Ok(None);
    }
    frame.truncate(write);
    Ok(Some(String::from_utf8(frame).expect(
        "validated SSE frame remains UTF-8 after compaction",
    )))
}

#[derive(Debug)]
struct AdmissionPool {
    unit_bytes: usize,
    total_units: usize,
    state: Mutex<AdmissionState>,
}

#[derive(Debug, Default)]
struct AdmissionState {
    next_claim: u64,
    next_ticket: u64,
    fixed_units: usize,
    declared_maximum_units: usize,
    claims: BTreeMap<u64, Claim>,
    waiters: BTreeMap<u64, Waiter>,
}

#[derive(Clone, Copy, Debug)]
struct Claim {
    maximum_units: usize,
    allocated_units: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WaitKind {
    Growth,
    Begin,
}

#[derive(Clone, Debug)]
struct Waiter {
    claim: u64,
    target_units: usize,
    kind: WaitKind,
    changed: Arc<Notify>,
}

impl AdmissionPool {
    fn new(unit_bytes: usize, total_units: usize) -> Self {
        assert!(unit_bytes > 0);
        assert!(total_units > 0);
        Self {
            unit_bytes,
            total_units,
            state: Mutex::new(AdmissionState::default()),
        }
    }

    fn units_for_bytes(&self, bytes: usize) -> usize {
        bytes.saturating_add(self.unit_bytes - 1) / self.unit_bytes
    }

    fn register_claim(&self, maximum_units: usize) -> Result<u64, TransportError> {
        if maximum_units == 0 || maximum_units > self.total_units {
            return Err(TransportError::new(
                "sse.invalid_frame_limit",
                "SSE frame admission claim exceeds the process budget",
            ));
        }
        let mut state = self.lock();
        if state.claims.len() >= MAXIMUM_ADMISSION_CLAIMS {
            return Err(TransportError::new(
                "sse.admission_capacity",
                "SSE unfinished-frame admission capacity is exhausted",
            ));
        }
        state.next_claim = state.next_claim.checked_add(1).ok_or_else(|| {
            TransportError::new(
                "sse.admission_exhausted",
                "SSE frame admission identity space is exhausted",
            )
        })?;
        let claim = state.next_claim;
        state.declared_maximum_units = state
            .declared_maximum_units
            .checked_add(maximum_units)
            .ok_or_else(|| {
                TransportError::new(
                    "sse.admission_exhausted",
                    "SSE frame admission declaration space is exhausted",
                )
            })?;
        state.claims.insert(
            claim,
            Claim {
                maximum_units,
                allocated_units: 0,
            },
        );
        Ok(claim)
    }

    async fn wait_for(
        self: &Arc<Self>,
        claim: u64,
        target_units: usize,
        kind: WaitKind,
    ) -> Result<(), TransportError> {
        let changed = Arc::new(Notify::new());
        let ticket = {
            let mut state = self.lock();
            let current = state.claims.get(&claim).ok_or_else(|| {
                TransportError::new(
                    "sse.admission_closed",
                    "SSE frame admission claim is absent",
                )
            })?;
            if target_units <= current.allocated_units || target_units > current.maximum_units {
                return Err(TransportError::new(
                    "sse.invalid_frame_limit",
                    "SSE frame admission target is outside its declared claim",
                ));
            }
            state.next_ticket = state.next_ticket.checked_add(1).ok_or_else(|| {
                TransportError::new(
                    "sse.admission_exhausted",
                    "SSE frame admission ticket space is exhausted",
                )
            })?;
            let ticket = state.next_ticket;
            state.waiters.insert(
                ticket,
                Waiter {
                    claim,
                    target_units,
                    kind,
                    changed: Arc::clone(&changed),
                },
            );
            ticket
        };
        let mut registration = WaitRegistration {
            pool: Arc::clone(self),
            ticket,
            active: true,
        };
        self.schedule_waiters();
        loop {
            let notified = changed.notified();
            tokio::pin!(notified);
            let _enabled = notified.as_mut().enable();
            if !self.lock().waiters.contains_key(&ticket) {
                registration.active = false;
                return Ok(());
            }
            notified.await;
        }
    }

    fn schedule_waiters(&self) {
        loop {
            let changed = {
                let mut state = self.lock();
                let Some(ticket) = select_waiter(&state, self.total_units) else {
                    return;
                };
                let waiter = state
                    .waiters
                    .remove(&ticket)
                    .expect("selected SSE admission waiter exists");
                let claim = state
                    .claims
                    .get_mut(&waiter.claim)
                    .expect("SSE admission waiter retains its claim");
                claim.allocated_units = waiter.target_units;
                waiter.changed
            };
            changed.notify_one();
        }
    }

    fn remove_waiter(&self, ticket: u64) {
        let removed = self.lock().waiters.remove(&ticket).is_some();
        if removed {
            self.schedule_waiters();
        }
    }

    fn remove_claim(&self, claim: u64) {
        let mut state = self.lock();
        let removed = state.claims.remove(&claim);
        if let Some(removed) = removed {
            state.declared_maximum_units = state
                .declared_maximum_units
                .checked_sub(removed.maximum_units)
                .expect("SSE claim maximum is released exactly once");
        }
        state.waiters.retain(|_, waiter| waiter.claim != claim);
        drop(state);
        if removed.is_some() {
            self.schedule_waiters();
        }
    }

    fn seal_claim(&self, claim: u64, actual_units: usize) {
        let mut state = self.lock();
        let current = state
            .claims
            .remove(&claim)
            .expect("sealed SSE frame admission claim exists");
        state.declared_maximum_units = state
            .declared_maximum_units
            .checked_sub(current.maximum_units)
            .expect("sealed SSE claim maximum is released exactly once");
        assert!(actual_units > 0);
        assert!(actual_units <= current.allocated_units);
        state.fixed_units = state
            .fixed_units
            .checked_add(actual_units)
            .expect("bounded SSE admission cannot overflow");
        drop(state);
        self.schedule_waiters();
    }

    fn release_fixed(&self, units: usize) {
        let mut state = self.lock();
        state.fixed_units = state
            .fixed_units
            .checked_sub(units)
            .expect("SSE delivered admission is released exactly once");
        drop(state);
        self.schedule_waiters();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, AdmissionState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn select_waiter(state: &AdmissionState, total_units: usize) -> Option<u64> {
    [WaitKind::Growth, WaitKind::Begin]
        .into_iter()
        .find_map(|kind| {
            state.waiters.iter().find_map(|(ticket, waiter)| {
                (waiter.kind == kind && safe_after_grant(state, waiter, total_units))
                    .then_some(*ticket)
            })
        })
}

fn safe_after_grant(state: &AdmissionState, waiter: &Waiter, total_units: usize) -> bool {
    let Some(current) = state.claims.get(&waiter.claim) else {
        return false;
    };
    if waiter.target_units <= current.allocated_units || waiter.target_units > current.maximum_units
    {
        return false;
    }
    if state
        .fixed_units
        .checked_add(state.declared_maximum_units)
        .is_some_and(|declared| declared <= total_units)
    {
        return true;
    }
    let mut used = state.fixed_units;
    let mut unfinished = Vec::with_capacity(state.claims.len());
    for (id, claim) in &state.claims {
        let allocated = if *id == waiter.claim {
            waiter.target_units
        } else {
            claim.allocated_units
        };
        if allocated > claim.maximum_units {
            return false;
        }
        let Some(next_used) = used.checked_add(allocated) else {
            return false;
        };
        used = next_used;
        unfinished.push((allocated, claim.maximum_units - allocated));
    }
    let Some(mut work) = total_units.checked_sub(used) else {
        return false;
    };
    unfinished.sort_unstable_by_key(|(_, remaining)| *remaining);
    for (allocated, remaining) in unfinished {
        if remaining > work {
            return false;
        }
        work = work
            .checked_add(allocated)
            .expect("bounded SSE admission simulation cannot overflow");
    }
    true
}

struct WaitRegistration {
    pool: Arc<AdmissionPool>,
    ticket: u64,
    active: bool,
}

impl Drop for WaitRegistration {
    fn drop(&mut self) {
        if self.active {
            self.pool.remove_waiter(self.ticket);
        }
    }
}

struct FrameAdmission {
    pool: Arc<AdmissionPool>,
    claim: Option<u64>,
    allocated_units: usize,
}

impl FrameAdmission {
    async fn begin(
        pool: Arc<AdmissionPool>,
        maximum_frame_bytes: usize,
    ) -> Result<Self, TransportError> {
        let maximum_units = pool.units_for_bytes(maximum_frame_bytes);
        let claim = pool.register_claim(maximum_units)?;
        let mut registration = ClaimRegistration {
            pool: Arc::clone(&pool),
            claim: Some(claim),
        };
        pool.wait_for(claim, 1, WaitKind::Begin).await?;
        registration.claim = None;
        Ok(Self {
            pool,
            claim: Some(claim),
            allocated_units: 1,
        })
    }

    async fn ensure_bytes(&mut self, bytes: usize) -> Result<(), TransportError> {
        let target_units = self.pool.units_for_bytes(bytes.max(1));
        if target_units <= self.allocated_units {
            return Ok(());
        }
        self.pool
            .wait_for(
                self.claim.expect("unfinished SSE frame admission exists"),
                target_units,
                WaitKind::Growth,
            )
            .await?;
        self.allocated_units = target_units;
        Ok(())
    }

    fn seal(mut self, actual_bytes: usize) -> FrameLease {
        let units = self.pool.units_for_bytes(actual_bytes.max(1));
        let claim = self.claim.take().expect("unfinished SSE admission exists");
        self.pool.seal_claim(claim, units);
        FrameLease {
            pool: Arc::clone(&self.pool),
            units,
        }
    }
}

impl Drop for FrameAdmission {
    fn drop(&mut self) {
        if let Some(claim) = self.claim.take() {
            self.pool.remove_claim(claim);
        }
    }
}

struct ClaimRegistration {
    pool: Arc<AdmissionPool>,
    claim: Option<u64>,
}

impl Drop for ClaimRegistration {
    fn drop(&mut self) {
        if let Some(claim) = self.claim.take() {
            self.pool.remove_claim(claim);
        }
    }
}

struct FrameLease {
    pool: Arc<AdmissionPool>,
    units: usize,
}

impl Drop for FrameLease {
    fn drop(&mut self) {
        self.pool.release_fixed(self.units);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_state_rejects_partial_allocation_deadlock() {
        let mut state = AdmissionState {
            fixed_units: 0,
            ..AdmissionState::default()
        };
        state.claims.insert(
            1,
            Claim {
                maximum_units: 2,
                allocated_units: 1,
            },
        );
        state.claims.insert(
            2,
            Claim {
                maximum_units: 2,
                allocated_units: 1,
            },
        );
        state.declared_maximum_units = 4;
        assert!(safe_after_grant(
            &state,
            &Waiter {
                claim: 1,
                target_units: 2,
                kind: WaitKind::Growth,
                changed: Arc::new(Notify::new()),
            },
            3,
        ));
        state.fixed_units = 1;
        assert!(!safe_after_grant(
            &state,
            &Waiter {
                claim: 1,
                target_units: 2,
                kind: WaitKind::Growth,
                changed: Arc::new(Notify::new()),
            },
            3,
        ));
    }

    #[test]
    fn two_unit_pool_rejects_the_incremental_allocation_deadlock() {
        let mut state = AdmissionState::default();
        state.claims.insert(
            1,
            Claim {
                maximum_units: 2,
                allocated_units: 1,
            },
        );
        state.claims.insert(
            2,
            Claim {
                maximum_units: 2,
                allocated_units: 0,
            },
        );
        state.declared_maximum_units = 4;

        assert!(!safe_after_grant(
            &state,
            &Waiter {
                claim: 2,
                target_units: 1,
                kind: WaitKind::Begin,
                changed: Arc::new(Notify::new()),
            },
            2,
        ));
    }

    #[test]
    fn ungrantable_growth_does_not_block_a_safe_begin_waiter() {
        let mut state = AdmissionState::default();
        state.claims.insert(
            1,
            Claim {
                maximum_units: 1,
                allocated_units: 1,
            },
        );
        state.claims.insert(
            2,
            Claim {
                maximum_units: 3,
                allocated_units: 1,
            },
        );
        state.claims.insert(
            3,
            Claim {
                maximum_units: 1,
                allocated_units: 0,
            },
        );
        state.declared_maximum_units = 5;
        state.waiters.insert(
            1,
            Waiter {
                claim: 2,
                target_units: 3,
                kind: WaitKind::Growth,
                changed: Arc::new(Notify::new()),
            },
        );
        state.waiters.insert(
            2,
            Waiter {
                claim: 3,
                target_units: 1,
                kind: WaitKind::Begin,
                changed: Arc::new(Notify::new()),
            },
        );

        assert!(!safe_after_grant(&state, state.waiters.get(&1).unwrap(), 3));
        assert!(safe_after_grant(&state, state.waiters.get(&2).unwrap(), 3));
        assert_eq!(select_waiter(&state, 3), Some(2));
    }

    #[test]
    fn frame_compaction_preserves_multiline_data_without_a_second_payload_buffer() {
        let frame = b": comment\ndata: first\nevent: ignored\ndata: second\n".to_vec();
        assert_eq!(
            decode_sse_frame(frame).unwrap().as_deref(),
            Some("first\nsecond")
        );
    }

    #[test]
    fn frame_capacity_grows_geometrically_within_the_admitted_maximum() {
        let maximum = 8 * ADMISSION_UNIT_BYTES;
        assert_eq!(next_frame_capacity(0, 1, maximum), ADMISSION_UNIT_BYTES);
        assert_eq!(
            next_frame_capacity(
                2 * ADMISSION_UNIT_BYTES,
                2 * ADMISSION_UNIT_BYTES + 1,
                maximum
            ),
            4 * ADMISSION_UNIT_BYTES
        );
        assert_eq!(
            next_frame_capacity(
                4 * ADMISSION_UNIT_BYTES,
                4 * ADMISSION_UNIT_BYTES + 1,
                maximum
            ),
            8 * ADMISSION_UNIT_BYTES
        );
    }

    #[tokio::test]
    async fn cancelled_waiter_removes_its_ticket_and_claim() {
        let pool = Arc::new(AdmissionPool::new(1, 1));
        let first = FrameAdmission::begin(Arc::clone(&pool), 1).await.unwrap();
        let lease = first.seal(1);
        let waiting = tokio::spawn({
            let pool = Arc::clone(&pool);
            async move { FrameAdmission::begin(pool, 1).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while pool.lock().waiters.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("second frame did not register its waiter");
        waiting.abort();
        match waiting.await {
            Err(error) => assert!(error.is_cancelled()),
            Ok(_) => panic!("aborted admission waiter completed"),
        }
        assert!(pool.lock().waiters.is_empty());
        assert!(pool.lock().claims.is_empty());

        drop(lease);
        let replacement = FrameAdmission::begin(Arc::clone(&pool), 1)
            .await
            .expect("cancelled waiter retained admission");
        drop(replacement);
    }

    #[test]
    fn unfinished_claim_count_has_an_exact_bound() {
        let pool = AdmissionPool::new(1, MAXIMUM_ADMISSION_CLAIMS + 1);
        for _ in 0..MAXIMUM_ADMISSION_CLAIMS {
            pool.register_claim(1).unwrap();
        }
        let error = pool.register_claim(1).expect_err("claim capacity");
        assert_eq!(error.code(), "sse.admission_capacity");
    }

    #[tokio::test]
    async fn sealed_actual_units_remain_fixed_until_the_value_drops() {
        let pool = Arc::new(AdmissionPool::new(1, 3));
        let mut first = FrameAdmission::begin(Arc::clone(&pool), 2).await.unwrap();
        first.ensure_bytes(2).await.unwrap();
        let mut second = FrameAdmission::begin(Arc::clone(&pool), 2).await.unwrap();
        let lease = first.seal(2);
        assert_eq!(pool.lock().fixed_units, 2);
        assert_eq!(pool.lock().claims.len(), 1);
        let growth = tokio::spawn(async move {
            second.ensure_bytes(2).await?;
            Ok::<_, TransportError>(second)
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while pool.lock().waiters.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("growth did not wait behind the sealed value");
        assert!(!growth.is_finished());
        drop(lease);
        let second = growth.await.unwrap().unwrap();
        assert_eq!(pool.lock().fixed_units, 0);
        drop(second);
    }
}
