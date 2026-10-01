use crate::{
    Control, Error, IncomingRequest, Message, RequestKind, Result, Role, Shared, StreamId,
    state::{Assembly, Class, State},
};
use rsi_ssh_protocol::frame::{Frame, FrameHeader, FrameKind, HEADER_BYTES, MAXIMUM_MESSAGE_BYTES};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub(crate) async fn read<R: AsyncRead + Unpin>(shared: Arc<Shared>, mut reader: R) {
    loop {
        let frame = tokio::select! {
            () = shared.stop.cancelled() => break,
            frame = read_frame(&mut reader, shared.epoch) => frame,
        };
        let Ok(frame) = frame else {
            break;
        };
        let accepted = { accept(&shared, &mut shared.lock(), frame) };
        match accepted {
            Ok(Some(request)) => {
                let queue = match request.kind {
                    RequestKind::Ordinary => &shared.ordinary,
                    RequestKind::Control(_) => &shared.controls,
                };
                if queue.try_send(request).is_err() {
                    break;
                }
            }
            Ok(None) => {}
            Err(_) => break,
        }
        shared.changed.notify_waiters();
    }
}
async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R, epoch: u64) -> Result<Frame> {
    let mut header = [0; HEADER_BYTES];
    reader
        .read_exact(&mut header)
        .await
        .map_err(|_| Error::Closed)?;
    let header = FrameHeader::decode(&header, epoch)?;
    let mut body = vec![0; header.body_len()];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|_| Error::Closed)?;
    Ok(header.with_body(body)?)
}
pub(crate) async fn write<W: AsyncWrite + Unpin>(shared: Arc<Shared>, mut writer: W) {
    loop {
        let changed = shared.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let next = { shared.lock().next_frame(shared.epoch) };
        match next {
            Ok(Some(emission)) => {
                let sent = tokio::select! {
                    () = shared.stop.cancelled() => break,
                    result = write_frame(&mut writer, shared.epoch, emission.frame) => result,
                };
                if sent.is_err() {
                    break;
                }
                shared.changed.notify_waiters();
            }
            Ok(None) => tokio::select! {
                () = shared.stop.cancelled() => break,
                () = &mut changed => {}
            },
            Err(_) => break,
        }
    }
}
async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    epoch: u64,
    frame: Frame,
) -> Result<()> {
    let header = frame.encode_header(epoch)?;
    writer.write_all(&header).await.map_err(|_| Error::Closed)?;
    writer
        .write_all(frame.body())
        .await
        .map_err(|_| Error::Closed)?;
    writer.flush().await.map_err(|_| Error::Closed)?;
    Ok(())
}

fn accept(
    shared: &Arc<Shared>,
    state: &mut State,
    frame: Frame,
) -> Result<Option<IncomingRequest>> {
    state.ensure_open()?;
    let header = frame.header();
    match header.kind() {
        FrameKind::Request
        | FrameKind::ControlRequest
        | FrameKind::Reply
        | FrameKind::ControlReply => accept_message(shared, state, frame),
        FrameKind::Credit | FrameKind::Data => {
            let id = StreamId::from_raw(header.identity())?;
            state.streams[(id.raw() & 63) as usize].accept(frame)?;
            Ok(None)
        }
        FrameKind::Heartbeat => {
            if shared.role != Role::Helper
                || state.heartbeat_ack.is_some()
                || state.last_heartbeat.checked_add(1) != Some(header.identity())
            {
                return Err(Error::Invalid);
            }
            state.last_heartbeat = header.identity();
            state.heartbeat_ack = Some(header.identity());
            shared.heartbeat.send_replace(header.identity());
            Ok(None)
        }
        FrameKind::HeartbeatAck => {
            if shared.role != Role::Client
                || !state.heartbeat_dispatched
                || state.last_heartbeat != header.identity()
            {
                return Err(Error::Invalid);
            }
            let waiter = state.heartbeat_waiter.take().ok_or(Error::Invalid)?;
            state.heartbeat_dispatched = false;
            state.heartbeat_deadline = None;
            shared.requests_changed.notify_waiters();
            let _ = waiter.send(Ok(()));
            Ok(None)
        }
        FrameKind::Close => Err(Error::Closed),
    }
}
fn accept_message(
    shared: &Arc<Shared>,
    state: &mut State,
    frame: Frame,
) -> Result<Option<IncomingRequest>> {
    let header = frame.header();
    let reply = matches!(header.kind(), FrameKind::Reply | FrameKind::ControlReply);
    let class = if matches!(
        header.kind(),
        FrameKind::ControlRequest | FrameKind::ControlReply
    ) {
        Class::Control
    } else {
        Class::Ordinary
    };
    let identity = header.identity();
    if identity & 1 != u64::from(class == Class::Control) {
        return Err(Error::Invalid);
    }
    if reply != (shared.role == Role::Client) {
        return Err(Error::Invalid);
    }
    if header.sequence() == 0 {
        if state.assemblies.contains_key(&identity) {
            return Err(Error::Invalid);
        }
        if reply {
            let pending = state.pending.get(&identity).ok_or(Error::Invalid)?;
            if pending.class != class || !pending.final_fragment_selected {
                return Err(Error::Invalid);
            }
        } else {
            if identity <= state.last_inbound[class.index()]
                || state.inbound.contains_key(&identity)
            {
                return Err(Error::Invalid);
            }
            if state
                .inbound
                .values()
                .filter(|stored| **stored == class)
                .count()
                >= class.limit()
            {
                return Err(Error::Capacity);
            }
            state.last_inbound[class.index()] = identity;
            state.inbound.insert(identity, class);
        }
        let message = Message::new(frame.into_body(), &state.incoming_budgets[class.index()])?;
        state.assemblies.insert(
            identity,
            Assembly {
                class,
                next_sequence: 1,
                message,
            },
        );
    } else {
        let assembly = state.assemblies.get_mut(&identity).ok_or(Error::Invalid)?;
        if assembly.class != class || assembly.next_sequence != header.sequence() {
            return Err(Error::Invalid);
        }
        if assembly.message.as_bytes().len() + frame.body().len() > MAXIMUM_MESSAGE_BYTES {
            return Err(Error::Capacity);
        }
        assembly.message.extend(frame.body())?;
        assembly.next_sequence = assembly
            .next_sequence
            .checked_add(1)
            .ok_or(Error::Capacity)?;
    }
    if !header.is_final() {
        return Ok(None);
    }
    let assembly = state.assemblies.remove(&identity).ok_or(Error::Invalid)?;
    if reply {
        let pending = state.pending.remove(&identity).ok_or(Error::Invalid)?;
        shared.requests_changed.notify_one();
        let _ = pending.reply.send(Ok(assembly.message));
        Ok(None)
    } else {
        let kind = match class {
            Class::Ordinary => RequestKind::Ordinary,
            Class::Control => {
                let control: Control = serde_json::from_slice(assembly.message.as_bytes())
                    .map_err(|_| Error::Invalid)?;
                control.validate()?;
                RequestKind::Control(control)
            }
        };
        Ok(Some(IncomingRequest {
            shared: shared.clone(),
            identity,
            kind,
            message: assembly.message,
            replied: false,
        }))
    }
}
