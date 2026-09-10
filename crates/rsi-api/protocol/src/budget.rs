use crate::{ApiError, MAXIMUM_API_BYTES, Result};
use bytes::Bytes;
use serde::Serialize;
use std::fmt;
use std::io::{self, Write};
use std::ops::{Bound, RangeBounds};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
struct Budget {
    limit: usize,
    used: AtomicUsize,
}

/// Non-queuing byte admission shared by one explicit resource owner.
#[derive(Clone, Debug)]
pub struct ByteBudget(Arc<Budget>);

impl Default for ByteBudget {
    /// Creates the standard 64 MiB retention pool without a fallible configuration.
    fn default() -> Self {
        Self(Arc::new(Budget {
            limit: MAXIMUM_API_BYTES,
            used: AtomicUsize::new(0),
        }))
    }
}

impl ByteBudget {
    /// Creates a bounded pool; zero-sized pools are useful for no-body operations.
    pub fn new(limit: usize) -> Result<Self> {
        if limit > MAXIMUM_API_BYTES {
            return Err(ApiError::Invalid("API byte budget exceeds 64 MiB".into()));
        }
        Ok(Self(Arc::new(Budget {
            limit,
            used: AtomicUsize::new(0),
        })))
    }
    /// Returns bytes still owned by reservations or retained buffers.
    pub fn used(&self) -> usize {
        self.0.used.load(Ordering::Acquire)
    }
    /// Returns the configured maximum.
    pub fn limit(&self) -> usize {
        self.0.limit
    }
    /// Acquires ownership before allocation; it never waits while holding input.
    pub fn reserve(&self, bytes: usize) -> Result<ByteReservation> {
        self.0
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|next| *next <= self.0.limit)
            })
            .map_err(|_| ApiError::Capacity)?;
        Ok(ByteReservation {
            budget: self.clone(),
            bytes,
        })
    }
    /// Reserves then copies a bounded external payload.
    pub fn copy(&self, source: &[u8]) -> Result<RetainedBytes> {
        self.reserve(source.len())?.copy(source)
    }
    /// Counts, reserves and encodes JSON without a speculative payload allocation.
    pub fn encode<T: Serialize + ?Sized>(
        &self,
        value: &T,
        maximum: usize,
    ) -> Result<RetainedBytes> {
        self.reserve(measure_json(value, maximum)?)?
            .encode_admitted(value)
    }
}

/// Exclusive byte ownership awaiting transfer into a retained buffer.
#[derive(Debug)]
pub struct ByteReservation {
    budget: ByteBudget,
    bytes: usize,
}

impl ByteReservation {
    fn grow(&mut self, bytes: usize) -> Result<()> {
        let mut additional = self.budget.reserve(bytes - self.bytes)?;
        self.bytes = bytes;
        additional.bytes = 0;
        Ok(())
    }
    /// Returns the admitted byte ceiling.
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    /// Splits off ownership without acquiring more bytes or allocating a payload.
    pub fn split(&mut self, bytes: usize) -> Result<Self> {
        if bytes > self.bytes {
            return Err(ApiError::Invalid("split exceeds byte reservation".into()));
        }
        self.bytes -= bytes;
        Ok(Self {
            budget: self.budget.clone(),
            bytes,
        })
    }
    /// Starts a bounded incoming body after admission, before allocating its storage.
    pub fn receive(self) -> ByteReceiver {
        ByteReceiver {
            data: Vec::with_capacity(self.bytes),
            reservation: self,
        }
    }
    /// Reduces a conservative reservation after the exact size is known.
    pub fn shrink(&mut self, bytes: usize) -> Result<()> {
        if bytes > self.bytes {
            return Err(ApiError::Invalid("cannot grow a byte reservation".into()));
        }
        self.budget
            .0
            .used
            .fetch_sub(self.bytes - bytes, Ordering::AcqRel);
        self.bytes = bytes;
        Ok(())
    }
    /// Copies within the admitted limit and transfers ownership to immutable bytes.
    pub fn copy(mut self, source: &[u8]) -> Result<RetainedBytes> {
        self.shrink(source.len())?;
        let data = source.to_vec();
        Ok(RetainedBytes(Bytes::from_owner(BufferOwner {
            data,
            _reservation: self,
        })))
    }
    /// Retains an allocation made after acquiring this reservation, without copying.
    ///
    /// The caller must reserve before allocating; capacity, including unused space,
    /// must fit the reservation and remains charged until the last owner drops.
    pub fn retain_vec(mut self, data: Vec<u8>) -> Result<RetainedBytes> {
        self.shrink(data.capacity())?;
        Ok(RetainedBytes(Bytes::from_owner(BufferOwner {
            data,
            _reservation: self,
        })))
    }
    /// Encodes within an already admitted maximum, then retains its exact byte charge.
    pub fn encode<T: Serialize + ?Sized>(mut self, value: &T) -> Result<RetainedBytes> {
        self.shrink(measure_json(value, self.bytes)?)?;
        self.encode_admitted(value)
    }
    fn encode_admitted<T: Serialize + ?Sized>(mut self, value: &T) -> Result<RetainedBytes> {
        let mut writer = Encoder {
            bytes: Vec::with_capacity(self.bytes),
            maximum: self.bytes,
        };
        serde_json::to_writer(&mut writer, value)
            .map_err(|error| ApiError::Invalid(error.to_string()))?;
        // Keep the reservation for the retained allocation, including unused capacity.
        // Exact measurement in ByteBudget::encode normally makes these equal.
        self.shrink(writer.bytes.capacity())?;
        Ok(RetainedBytes(Bytes::from_owner(BufferOwner {
            data: writer.bytes,
            _reservation: self,
        })))
    }
}

/// Counts JSON bytes up to a bounded maximum without allocating an encoded payload.
pub fn measure_json<T: Serialize + ?Sized>(value: &T, maximum: usize) -> Result<usize> {
    if maximum > MAXIMUM_API_BYTES {
        return Err(ApiError::Invalid(
            "encoded API maximum exceeds 64 MiB".into(),
        ));
    }
    let mut count = Counter { bytes: 0, maximum };
    serde_json::to_writer(&mut count, value)
        .map_err(|error| ApiError::Invalid(error.to_string()))?;
    Ok(count.bytes)
}

/// Admitted body storage with no unbounded growth path.
#[derive(Debug)]
pub struct ByteReceiver {
    data: Vec<u8>,
    reservation: ByteReservation,
}

/// Incrementally admitted storage for an incoming payload without a known length.
#[derive(Debug)]
pub struct ByteAccumulator {
    receiver: ByteReceiver,
    maximum: usize,
}

impl ByteAccumulator {
    /// Starts empty, with independent per-payload and shared receiving bounds.
    pub fn new(budget: &ByteBudget, maximum: usize) -> Result<Self> {
        if maximum > MAXIMUM_API_BYTES {
            return Err(ApiError::Invalid(
                "API accumulator maximum exceeds 64 MiB".into(),
            ));
        }
        Ok(Self {
            receiver: budget.reserve(0)?.receive(),
            maximum,
        })
    }
    /// Admits growth before allocating or copying, without waiting for capacity.
    pub fn append(&mut self, chunk: &[u8]) -> Result<()> {
        let receiver = &mut self.receiver;
        if chunk.len() > self.maximum.saturating_sub(receiver.data.len()) {
            return Err(ApiError::Invalid(
                "received API payload exceeds its maximum".into(),
            ));
        }
        let required = receiver.data.len() + chunk.len();
        if required > receiver.reservation.bytes {
            let preferred = required
                .next_power_of_two()
                .min(self.maximum)
                .min(receiver.reservation.budget.limit());
            // Spare capacity is an optimization, never a reason to reject bytes
            // that still fit the shared receiving pool.
            if preferred < required || receiver.reservation.grow(preferred).is_err() {
                receiver.reservation.grow(required)?;
            }
            receiver
                .data
                .try_reserve_exact(receiver.reservation.bytes - receiver.data.len())
                .map_err(|_| ApiError::Capacity)?;
            if receiver.data.capacity() > receiver.reservation.bytes {
                return Err(ApiError::Capacity);
            }
        }
        receiver.append(chunk)
    }
    /// Compacts completed storage and transfers it under destination admission.
    pub fn finish_into(self, destination: &ByteBudget) -> Result<RetainedBytes> {
        self.receiver.finish_into(destination)
    }
    /// Completes storage in the receiving pool, releasing unused capacity.
    pub fn finish_compact(self) -> Result<RetainedBytes> {
        self.receiver.finish_compact()
    }
}

impl ByteReceiver {
    /// Appends one transport fragment within the originally admitted capacity.
    pub fn append(&mut self, chunk: &[u8]) -> Result<()> {
        if chunk.len() > self.reservation.bytes.saturating_sub(self.data.len()) {
            return Err(ApiError::Invalid(
                "received API body exceeds its reservation".into(),
            ));
        }
        self.data.extend_from_slice(chunk);
        Ok(())
    }
    /// Transfers storage without copying, retaining the complete allocated capacity.
    pub fn finish(self) -> RetainedBytes {
        RetainedBytes(Bytes::from_owner(BufferOwner {
            data: self.data,
            _reservation: self.reservation,
        }))
    }
    /// Releases unused allocation before handing off a completed variable-length body.
    ///
    /// The maximum reservation covers compaction; only the remaining capacity is
    /// transferred. This accounts retained storage, not allocator-internal RSS peaks.
    pub fn finish_compact(mut self) -> Result<RetainedBytes> {
        self.data.shrink_to_fit();
        self.reservation.shrink(self.data.capacity())?;
        Ok(self.finish())
    }
    /// Moves completed storage to a new owner after acquiring its bounded admission.
    /// Source ownership remains held during compaction and destination admission.
    pub fn finish_into(mut self, destination: &ByteBudget) -> Result<RetainedBytes> {
        self.data.shrink_to_fit();
        let reservation = destination.reserve(self.data.capacity())?;
        self.reservation = reservation;
        Ok(self.finish())
    }
}

impl Drop for ByteReservation {
    fn drop(&mut self) {
        self.budget.0.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

struct BufferOwner {
    data: Vec<u8>,
    _reservation: ByteReservation,
}
impl AsRef<[u8]> for BufferOwner {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

/// Immutable bytes whose last clone or slice owns their allocation's reservation.
#[derive(Clone, Eq, PartialEq)]
pub struct RetainedBytes(Bytes);

impl RetainedBytes {
    /// Retains an already acquired resource guard through byte clones, slices and
    /// transport transfer. Does not copy data or replace its byte reservation.
    #[must_use]
    pub fn with_retention<T: Send + 'static>(self, guard: T) -> Self {
        struct Retention<T> {
            data: RetainedBytes,
            _guard: T,
        }
        impl<T> AsRef<[u8]> for Retention<T> {
            fn as_ref(&self) -> &[u8] {
                self.data.as_bytes()
            }
        }
        Self(Bytes::from_owner(Retention {
            data: self,
            _guard: guard,
        }))
    }
    /// Returns a checked slice. Nonempty slices retain the complete allocation
    /// and attached guard; empty slices retain neither. Siblings are unaffected.
    pub fn slice(&self, range: impl RangeBounds<usize>) -> Result<Self> {
        let start = match range.start_bound() {
            Bound::Included(&start) => start,
            Bound::Excluded(&start) => start
                .checked_add(1)
                .ok_or_else(|| ApiError::Invalid("slice start overflow".into()))?,
            Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            Bound::Included(&end) => end
                .checked_add(1)
                .ok_or_else(|| ApiError::Invalid("slice end overflow".into()))?,
            Bound::Excluded(&end) => end,
            Bound::Unbounded => self.0.len(),
        };
        if start > end || end > self.0.len() {
            return Err(ApiError::Invalid("slice is outside retained bytes".into()));
        }
        Ok(Self(self.0.slice(start..end)))
    }
    /// Transfers the buffer to a byte-oriented transport without losing its owner.
    pub fn into_bytes(self) -> Bytes {
        self.0
    }
    /// Borrows the exact immutable payload.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    /// Returns payload length, which may be less than its retained allocation.
    pub fn len(&self) -> usize {
        self.0.len()
    }
    /// Whether this view is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
impl AsRef<[u8]> for RetainedBytes {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}
impl fmt::Debug for RetainedBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RetainedBytes")
            .field("bytes", &self.len())
            .finish()
    }
}

struct Counter {
    bytes: usize,
    maximum: usize,
}
impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .filter(|next| *next <= self.maximum)
            .ok_or_else(|| io::Error::other("encoded API payload exceeds its bound"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct Encoder {
    bytes: Vec<u8>,
    maximum: usize,
}
impl Write for Encoder {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.maximum.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other(
                "encoded API payload exceeds its reservation",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
