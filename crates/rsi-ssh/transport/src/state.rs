use crate::{Error, Message, Result, budget::Budget, stream::Slot};
use rsi_ssh_protocol::frame::{Frame, FrameKind, MAXIMUM_FRAGMENT_BYTES};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};
use tokio::sync::oneshot;

pub(crate) const ORDINARY_CALLS: usize = 32;
pub(crate) const CONTROL_CALLS: usize = 16;
pub(crate) const STREAMS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Class {
    Ordinary,
    Control,
}
impl Class {
    pub fn limit(self) -> usize {
        match self {
            Self::Ordinary => ORDINARY_CALLS,
            Self::Control => CONTROL_CALLS,
        }
    }
    pub fn request(self) -> FrameKind {
        match self {
            Self::Ordinary => FrameKind::Request,
            Self::Control => FrameKind::ControlRequest,
        }
    }
    pub fn reply(self) -> FrameKind {
        match self {
            Self::Ordinary => FrameKind::Reply,
            Self::Control => FrameKind::ControlReply,
        }
    }
    pub fn index(self) -> usize {
        usize::from(self == Self::Control)
    }
}

#[derive(Debug)]
pub(crate) struct Pending {
    pub deadline: tokio::time::Instant,
    pub class: Class,
    pub dispatched: bool,
    pub final_fragment_selected: bool,
    pub reply: oneshot::Sender<Result<Message>>,
}
#[derive(Debug)]
pub(crate) struct Assembly {
    pub class: Class,
    pub next_sequence: u32,
    pub message: Message,
}
#[derive(Debug)]
pub(crate) struct Outgoing {
    pub identity: u64,
    pub class: Class,
    pub reply: bool,
    pub message: Message,
    offset: usize,
    sequence: u32,
}
impl Outgoing {
    pub fn new(identity: u64, class: Class, reply: bool, message: Message) -> Self {
        Self {
            identity,
            class,
            reply,
            message,
            offset: 0,
            sequence: 0,
        }
    }
    fn fragment(&mut self, epoch: u64) -> Result<Frame> {
        let bytes = self.message.as_bytes();
        let end = bytes.len().min(self.offset + MAXIMUM_FRAGMENT_BYTES);
        let frame = Frame::new(
            epoch,
            if self.reply {
                self.class.reply()
            } else {
                self.class.request()
            },
            self.identity,
            self.sequence,
            end == bytes.len(),
            bytes[self.offset..end].to_vec(),
        )?;
        self.sequence += 1;
        self.offset = end;
        Ok(frame)
    }
}

#[derive(Debug)]
pub(crate) struct Emission {
    pub frame: Frame,
}

#[derive(Debug)]
pub(crate) struct State {
    pub closed: bool,
    pub outgoing_budgets: [Arc<Budget>; 2],
    pub incoming_budgets: [Arc<Budget>; 2],
    pub next_request: u64,
    pub pending: BTreeMap<u64, Pending>,
    pub inbound: BTreeMap<u64, Class>,
    pub last_inbound: [u64; 2],
    pub assemblies: BTreeMap<u64, Assembly>,
    pub ordinary: VecDeque<Outgoing>,
    pub controls: VecDeque<Outgoing>,
    pub streams: [Slot; STREAMS],
    credit_cursor: usize,
    data_cursor: usize,
    prefer_data: bool,
    pub last_heartbeat: u64,
    pub heartbeat_deadline: Option<tokio::time::Instant>,
    pub heartbeat_waiter: Option<oneshot::Sender<Result<()>>>,
    pub heartbeat_to_send: Option<u64>,
    pub heartbeat_ack: Option<u64>,
    pub heartbeat_dispatched: bool,
}
impl State {
    pub fn new() -> Self {
        Self {
            closed: false,
            outgoing_budgets: std::array::from_fn(|_| Arc::default()),
            incoming_budgets: std::array::from_fn(|_| Arc::default()),
            next_request: 1,
            pending: BTreeMap::new(),
            inbound: BTreeMap::new(),
            last_inbound: [0; 2],
            assemblies: BTreeMap::new(),
            ordinary: VecDeque::new(),
            controls: VecDeque::new(),
            streams: std::array::from_fn(|_| Slot::default()),
            credit_cursor: 0,
            data_cursor: 0,
            prefer_data: false,
            last_heartbeat: 0,
            heartbeat_deadline: None,
            heartbeat_waiter: None,
            heartbeat_to_send: None,
            heartbeat_ack: None,
            heartbeat_dispatched: false,
        }
    }
    pub fn ensure_open(&self) -> Result<()> {
        if self.closed {
            Err(Error::Closed)
        } else {
            Ok(())
        }
    }
    pub fn enqueue(&mut self, message: Outgoing) {
        match message.class {
            Class::Ordinary => self.ordinary.push_back(message),
            Class::Control => self.controls.push_back(message),
        }
    }
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        for (_, pending) in std::mem::take(&mut self.pending) {
            let error = if pending.dispatched {
                Error::OutcomeUnknown
            } else {
                Error::Closed
            };
            let _ = pending.reply.send(Err(error));
        }
        if let Some(waiter) = self.heartbeat_waiter.take() {
            let _ = waiter.send(Err(Error::Closed));
        }
        self.ordinary.clear();
        self.controls.clear();
        self.assemblies.clear();
        self.inbound.clear();
        for slot in &mut self.streams {
            slot.active = None;
        }
    }
    pub fn next_frame(&mut self, epoch: u64) -> Result<Option<Emission>> {
        if self.closed {
            return Ok(None);
        }
        if let Some(serial) = self.heartbeat_ack.take() {
            return Ok(Some(Emission {
                frame: Frame::new(epoch, FrameKind::HeartbeatAck, serial, 0, true, vec![])?,
            }));
        }
        if let Some(serial) = self.heartbeat_to_send.take() {
            self.heartbeat_dispatched = true;
            return Ok(Some(Emission {
                frame: Frame::new(epoch, FrameKind::Heartbeat, serial, 0, true, vec![])?,
            }));
        }
        if let Some(message) = self.controls.pop_front() {
            return self.emit_message(epoch, message);
        }
        for _ in 0..STREAMS {
            let index = self.credit_cursor;
            self.credit_cursor = (index + 1) % STREAMS;
            if let Some(frame) = self.streams[index].credit_frame(epoch)? {
                return Ok(Some(Emission { frame }));
            }
        }
        if self.prefer_data
            && let Some(frame) = self.data_frame(epoch)?
        {
            self.prefer_data = false;
            return Ok(Some(Emission { frame }));
        }
        if let Some(message) = self.ordinary.pop_front() {
            self.prefer_data = true;
            return self.emit_message(epoch, message);
        }
        Ok(self.data_frame(epoch)?.map(|frame| Emission { frame }))
    }
    fn emit_message(&mut self, epoch: u64, mut message: Outgoing) -> Result<Option<Emission>> {
        let frame = message.fragment(epoch)?;
        if !message.reply {
            let pending = self
                .pending
                .get_mut(&message.identity)
                .ok_or(Error::Invalid)?;
            pending.dispatched = true;
            pending.final_fragment_selected = frame.header().is_final();
        }
        if message.reply && frame.header().is_final() {
            self.inbound.remove(&message.identity);
        }
        if !frame.header().is_final() {
            self.enqueue(message);
        }
        Ok(Some(Emission { frame }))
    }
    fn data_frame(&mut self, epoch: u64) -> Result<Option<Frame>> {
        for _ in 0..STREAMS {
            let index = self.data_cursor;
            self.data_cursor = (index + 1) % STREAMS;
            if let Some(frame) = self.streams[index].data_frame(epoch)? {
                return Ok(Some(frame));
            }
        }
        Ok(None)
    }
}
