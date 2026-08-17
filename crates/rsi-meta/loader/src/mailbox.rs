use rsi_meta_plugin::{
    LANE_CONTROL, LANE_DATA, Lane, POST_FRAME_ACCEPTED, POST_FRAME_CLOSED, POST_FRAME_WOULD_BLOCK,
};

use crate::LoaderError;

/// Capacity and frame bound for the safe host callback adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PluginMailboxOptions {
    pub control_capacity: usize,
    pub data_capacity: usize,
    pub max_frame_bytes: usize,
}

impl Default for PluginMailboxOptions {
    fn default() -> Self {
        Self {
            control_capacity: 64,
            data_capacity: 256,
            max_frame_bytes: 1024 * 1024,
        }
    }
}

impl PluginMailboxOptions {
    pub(super) fn validate(self) -> Result<Self, LoaderError> {
        if self.control_capacity == 0 {
            return Err(LoaderError::InvalidMailboxOptions(
                "control_capacity must be greater than zero",
            ));
        }
        if self.data_capacity == 0 {
            return Err(LoaderError::InvalidMailboxOptions(
                "data_capacity must be greater than zero",
            ));
        }
        if self.max_frame_bytes == 0 {
            return Err(LoaderError::InvalidMailboxOptions(
                "max_frame_bytes must be greater than zero",
            ));
        }
        Ok(self)
    }
}

/// One frame synchronously copied from an arbitrary plugin thread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PostedFrame {
    payload: Box<[u8]>,
}

impl PostedFrame {
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    pub fn into_payload(self) -> Box<[u8]> {
        self.payload
    }
}

/// Independent bounded receivers for lifecycle/control and service DATA.
#[derive(Debug)]
pub struct PluginMailbox {
    pub(super) control: tokio::sync::mpsc::Receiver<PostedFrame>,
    pub(super) data: tokio::sync::mpsc::Receiver<PostedFrame>,
}

impl PluginMailbox {
    pub async fn recv_control(&mut self) -> Option<PostedFrame> {
        self.control.recv().await
    }

    pub async fn recv_data(&mut self) -> Option<PostedFrame> {
        self.data.recv().await
    }

    /// Attempts to receive a control frame without waiting.
    ///
    /// # Errors
    ///
    /// Returns `Empty` or `Disconnected` from the bounded lane.
    pub fn try_recv_control(
        &mut self,
    ) -> Result<PostedFrame, tokio::sync::mpsc::error::TryRecvError> {
        self.control.try_recv()
    }

    /// Attempts to receive a DATA frame without waiting.
    ///
    /// # Errors
    ///
    /// Returns `Empty` or `Disconnected` from the bounded lane.
    pub fn try_recv_data(&mut self) -> Result<PostedFrame, tokio::sync::mpsc::error::TryRecvError> {
        self.data.try_recv()
    }

    /// Splits the mailbox into independently owned control and data receivers.
    ///
    /// The tuple order is `(control, data)`. Each receiver has a fixed lane, so
    /// frames do not duplicate that routing metadata.
    pub fn into_lanes(self) -> (PluginLaneReceiver, PluginLaneReceiver) {
        (
            PluginLaneReceiver {
                lane: Lane::Control,
                receiver: self.control,
            },
            PluginLaneReceiver {
                lane: Lane::Data,
                receiver: self.data,
            },
        )
    }
}

/// Independently owned receiver for exactly one plugin output lane.
#[derive(Debug)]
pub struct PluginLaneReceiver {
    lane: Lane,
    receiver: tokio::sync::mpsc::Receiver<PostedFrame>,
}

impl PluginLaneReceiver {
    /// Lane permanently associated with this receiver.
    pub const fn lane(&self) -> Lane {
        self.lane
    }

    /// Waits for the next frame on this lane.
    pub async fn recv(&mut self) -> Option<PostedFrame> {
        self.receiver.recv().await
    }

    /// Attempts to receive a frame without waiting.
    ///
    /// # Errors
    ///
    /// Returns `Empty` or `Disconnected` from this bounded lane.
    pub fn try_recv(&mut self) -> Result<PostedFrame, tokio::sync::mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }
}

#[derive(Debug)]
pub(super) struct QueueHostContext {
    pub(super) control: tokio::sync::mpsc::Sender<PostedFrame>,
    pub(super) data: tokio::sync::mpsc::Sender<PostedFrame>,
    pub(super) max_frame_bytes: usize,
}

pub(super) unsafe extern "C" fn queue_post_frame(
    host_handle: *mut core::ffi::c_void,
    lane: u32,
    data_ptr: *const u8,
    data_len: usize,
) -> u32 {
    if host_handle.is_null() || data_len > 0 && data_ptr.is_null() {
        return POST_FRAME_CLOSED;
    }
    // SAFETY: `load_queued` retains this context at a stable address until the
    // plugin destroy callback returns. Failed initialization leaks the context
    // because the loader cannot prove that plugin-created threads stopped.
    let context = unsafe { &*host_handle.cast::<QueueHostContext>() };
    if data_len > context.max_frame_bytes {
        return POST_FRAME_WOULD_BLOCK;
    }
    let sender = match lane {
        LANE_CONTROL => &context.control,
        LANE_DATA => &context.data,
        _ => return POST_FRAME_CLOSED,
    };
    let permit = match sender.try_reserve() {
        Ok(permit) => permit,
        Err(tokio::sync::mpsc::error::TrySendError::Full(())) => {
            return POST_FRAME_WOULD_BLOCK;
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(())) => return POST_FRAME_CLOSED,
    };
    let payload = if data_len == 0 {
        Box::default()
    } else {
        // SAFETY: The ABI requires an outgoing slice to be readable only for
        // this callback. Capacity is already reserved, so rejected attempts do
        // not dereference or copy the plugin buffer.
        copy_posted_payload(unsafe { std::slice::from_raw_parts(data_ptr, data_len) })
    };
    permit.send(PostedFrame { payload });
    POST_FRAME_ACCEPTED
}

fn copy_posted_payload(payload: &[u8]) -> Box<[u8]> {
    #[cfg(test)]
    POSTED_PAYLOAD_COPIES.with(|copies| copies.set(copies.get() + 1));
    Box::from(payload)
}

#[cfg(test)]
std::thread_local! {
    static POSTED_PAYLOAD_COPIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_posted_payload_copies() {
    POSTED_PAYLOAD_COPIES.with(|copies| copies.set(0));
}

#[cfg(test)]
pub(super) fn posted_payload_copies() -> usize {
    POSTED_PAYLOAD_COPIES.with(std::cell::Cell::get)
}
