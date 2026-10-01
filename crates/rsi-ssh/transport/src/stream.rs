use crate::{Connection, Error, Result, state::STREAMS};
use rsi_ssh_protocol::frame::{Frame, FrameKind, MAXIMUM_FRAGMENT_BYTES, STREAM_WINDOW_FRAMES};
use std::collections::VecDeque;

/// A bounded slot and generation. It is routing data, not authority to a stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamId(u64);
impl StreamId {
    /// Validates a wire identity without allocating a stream or doing I/O.
    ///
    /// # Errors
    /// Rejects a zero generation.
    pub fn from_raw(value: u64) -> Result<Self> {
        if value >> 6 == 0 {
            Err(Error::Invalid)
        } else {
            Ok(Self(value))
        }
    }
    /// Constructs one of 64 slots with a nonzero generation of at most 58 bits.
    ///
    /// # Errors
    /// Rejects out-of-range slots or generations.
    pub fn new(slot: u8, generation: u64) -> Result<Self> {
        if usize::from(slot) >= STREAMS || generation == 0 || generation > u64::MAX >> 6 {
            return Err(Error::Invalid);
        }
        Ok(Self((generation << 6) | u64::from(slot)))
    }
    /// Returns the bounded wire representation.
    pub fn raw(self) -> u64 {
        self.0
    }
    fn slot(self) -> usize {
        (self.0 & 63) as usize
    }
    fn generation(self) -> u64 {
        self.0 >> 6
    }
}

/// One exact ordered data frame. EOF may carry final bytes.
#[derive(Eq, PartialEq)]
pub struct StreamChunk {
    pub bytes: Vec<u8>,
    pub eof: bool,
}
impl std::fmt::Debug for StreamChunk {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StreamChunk")
            .field("bytes", &self.bytes.len())
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
pub(crate) struct Slot {
    pub generation: u64,
    pub active: Option<Direction>,
}
#[derive(Debug)]
pub(crate) enum Direction {
    Send(Tx),
    Receive(Rx),
}
#[derive(Debug)]
pub(crate) struct Tx {
    id: StreamId,
    queue: VecDeque<(u32, StreamChunk)>,
    next_data: u32,
    next_credit: u32,
    credits: u32,
    debt: u32,
    eof: bool,
}
#[derive(Debug)]
pub(crate) struct Rx {
    id: StreamId,
    queue: VecDeque<StreamChunk>,
    next_data: u32,
    next_credit: u32,
    credits: u32,
    pending_credit: u32,
    eof_received: bool,
    eof_delivered: bool,
}
impl Slot {
    pub fn credit_frame(&mut self, epoch: u64) -> Result<Option<Frame>> {
        let Some(Direction::Receive(rx)) = &mut self.active else {
            return Ok(None);
        };
        if rx.pending_credit == 0 {
            return Ok(None);
        }
        let next_credit = rx.next_credit.checked_add(1).ok_or(Error::Capacity)?;
        let frame = Frame::new(
            epoch,
            FrameKind::Credit,
            rx.id.raw(),
            rx.next_credit,
            true,
            rx.pending_credit.to_be_bytes().to_vec(),
        )?;
        rx.next_credit = next_credit;
        rx.credits += rx.pending_credit;
        rx.pending_credit = 0;
        Ok(Some(frame))
    }
    pub fn data_frame(&mut self, epoch: u64) -> Result<Option<Frame>> {
        let Some(Direction::Send(tx)) = &mut self.active else {
            return Ok(None);
        };
        let Some((sequence, chunk)) = tx.queue.pop_front() else {
            return Ok(None);
        };
        tx.debt += 1;
        Ok(Some(Frame::new(
            epoch,
            FrameKind::Data,
            tx.id.raw(),
            sequence,
            chunk.eof,
            chunk.bytes,
        )?))
    }
    fn matching(&mut self, id: StreamId) -> Result<&mut Direction> {
        if self.generation != id.generation() {
            return Err(Error::Invalid);
        }
        self.active.as_mut().ok_or(Error::Invalid)
    }
    pub fn accept(&mut self, frame: Frame) -> Result<()> {
        let header = frame.header();
        let id = StreamId::from_raw(header.identity())?;
        if id.generation() < self.generation
            || (id.generation() == self.generation && self.active.is_none())
        {
            // Cleanup may overtake already admitted data or credit. Historical
            // frames cannot regain a slot or affect the current generation.
            return Ok(());
        }
        match (self.matching(id)?, header.kind()) {
            (Direction::Send(tx), FrameKind::Credit) => {
                let grant =
                    u32::from_be_bytes(frame.body().try_into().map_err(|_| Error::Invalid)?);
                if header.sequence() != tx.next_credit {
                    return Err(Error::Invalid);
                }
                if tx.next_credit == 1 {
                    if grant != STREAM_WINDOW_FRAMES {
                        return Err(Error::Invalid);
                    }
                } else {
                    if grant > tx.debt {
                        return Err(Error::Invalid);
                    }
                    tx.debt -= grant;
                }
                if tx.credits + grant > STREAM_WINDOW_FRAMES {
                    return Err(Error::Invalid);
                }
                tx.next_credit = tx.next_credit.checked_add(1).ok_or(Error::Capacity)?;
                tx.credits += grant;
            }
            (Direction::Receive(rx), FrameKind::Data) => {
                if rx.eof_received
                    || rx.credits == 0
                    || header.sequence() != rx.next_data
                    || rx.queue.len() >= STREAM_WINDOW_FRAMES as usize
                {
                    return Err(Error::Invalid);
                }
                rx.next_data = rx.next_data.checked_add(1).ok_or(Error::Capacity)?;
                rx.credits -= 1;
                rx.eof_received = header.is_final();
                rx.queue.push_back(StreamChunk {
                    bytes: frame.into_body(),
                    eof: header.is_final(),
                });
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
}

/// One sender retaining its connection. Submission retains bytes after waiter loss.
#[derive(Debug)]
pub struct SendStream {
    connection: Connection,
    id: StreamId,
}
/// One receiver retaining its connection. Full queues spend their finite credits.
#[derive(Debug)]
pub struct ReceiveStream {
    connection: Connection,
    id: StreamId,
}

impl Connection {
    fn register_stream(&self, id: StreamId, active: Direction) -> Result<()> {
        let mut state = self.shared().lock();
        state.ensure_open()?;
        let slot = &mut state.streams[id.slot()];
        if id.generation() <= slot.generation {
            return Err(Error::Invalid);
        }
        if slot.active.is_some() {
            return Err(Error::Capacity);
        }
        slot.generation = id.generation();
        slot.active = Some(active);
        drop(state);
        self.shared().changed.notify_waiters();
        Ok(())
    }
    /// Reserves a sending slot before the peer grants its initial credit window.
    ///
    /// # Errors
    /// Rejects a retired connection, occupied slot or previously used generation.
    pub fn open_sender(&self, id: StreamId) -> Result<SendStream> {
        self.register_stream(
            id,
            Direction::Send(Tx {
                id,
                queue: VecDeque::new(),
                next_data: 0,
                next_credit: 1,
                credits: 0,
                debt: 0,
                eof: false,
            }),
        )?;
        Ok(SendStream {
            connection: self.clone(),
            id,
        })
    }
    /// Reserves a receiving slot and schedules exactly four initial frame credits.
    ///
    /// # Errors
    /// Rejects a retired connection, occupied slot or previously used generation.
    pub fn open_receiver(&self, id: StreamId) -> Result<ReceiveStream> {
        self.register_stream(
            id,
            Direction::Receive(Rx {
                id,
                queue: VecDeque::new(),
                next_data: 0,
                next_credit: 1,
                credits: 0,
                pending_credit: STREAM_WINDOW_FRAMES,
                eof_received: false,
                eof_delivered: false,
            }),
        )?;
        Ok(ReceiveStream {
            connection: self.clone(),
            id,
        })
    }
    /// Releases a slot after the caller's cleanup barrier. Never reuses its generation.
    ///
    /// # Errors
    /// Rejects a retired connection or a missing/different generation.
    pub fn retire_stream(&self, id: StreamId) -> Result<()> {
        let mut state = self.shared().lock();
        state.ensure_open()?;
        let slot = &mut state.streams[id.slot()];
        slot.matching(id)?;
        slot.active = None;
        drop(state);
        self.shared().changed.notify_waiters();
        Ok(())
    }
}
impl SendStream {
    /// Returns routing data for helper operation schemas.
    pub fn id(&self) -> StreamId {
        self.id
    }
    /// Waits for credit, then atomically queues one bounded chunk. This is transport
    /// acceptance, not a remote application-level acknowledgement.
    ///
    /// # Errors
    /// Rejects oversized or empty nonterminal frames, writes after EOF, exhausted
    /// sequences, retired connections and stale stream identities.
    pub async fn send(&mut self, bytes: Vec<u8>, eof: bool) -> Result<()> {
        if bytes.len() > MAXIMUM_FRAGMENT_BYTES {
            return Err(Error::Capacity);
        }
        if bytes.is_empty() && !eof {
            return Err(Error::Invalid);
        }
        let bytes = bytes.into_boxed_slice().into_vec();
        let shared = self.connection.shared();
        loop {
            let changed = shared.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut state = shared.lock();
                state.ensure_open()?;
                let Direction::Send(tx) = state.streams[self.id.slot()].matching(self.id)? else {
                    return Err(Error::Invalid);
                };
                if tx.eof {
                    return Err(Error::Invalid);
                }
                if tx.credits > 0 {
                    let next = tx.next_data.checked_add(1).ok_or(Error::Capacity)?;
                    tx.credits -= 1;
                    tx.eof = eof;
                    tx.queue
                        .push_back((tx.next_data, StreamChunk { bytes, eof }));
                    tx.next_data = next;
                    drop(state);
                    shared.changed.notify_waiters();
                    return Ok(());
                }
            }
            tokio::select! { () = shared.stop.cancelled() => return Err(Error::Closed), () = &mut changed => {} }
        }
    }
}
impl ReceiveStream {
    /// Returns routing data for helper operation schemas.
    pub fn id(&self) -> StreamId {
        self.id
    }
    /// Receives one exact frame and returns its credit without awaiting the writer.
    ///
    /// # Errors
    /// Rejects a retired connection or stale stream identity. Disconnect is an error
    /// even when no explicit EOF was received.
    pub async fn next(&mut self) -> Result<Option<StreamChunk>> {
        let shared = self.connection.shared();
        loop {
            let changed = shared.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut state = shared.lock();
                state.ensure_open()?;
                let Direction::Receive(rx) = state.streams[self.id.slot()].matching(self.id)?
                else {
                    return Err(Error::Invalid);
                };
                if let Some(chunk) = rx.queue.pop_front() {
                    rx.eof_delivered = chunk.eof;
                    if !rx.eof_received {
                        rx.pending_credit += 1;
                    }
                    drop(state);
                    shared.changed.notify_waiters();
                    return Ok(Some(chunk));
                }
                if rx.eof_delivered {
                    return Ok(None);
                }
            }
            tokio::select! { () = shared.stop.cancelled() => return Err(Error::Closed), () = &mut changed => {} }
        }
    }
}
