//! Black-box harness for driving a trusted plugin through its public C ABI.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use rsi_meta_plugin::{
    CALL_FAILED, CALL_OK, CallOutcome, HostApi, INIT_OK, Lane, POST_FRAME_ACCEPTED,
    POST_FRAME_CLOSED, POST_FRAME_WOULD_BLOCK, PluginApi, PluginEntryFn, PostFrameOutcome,
};
use rsi_meta_plugin::{Frame, FrameError};
use thiserror::Error;

const MAX_CAPTURED_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedFrame {
    pub lane: Lane,
    pub frame: Frame,
}

#[derive(Debug, Error)]
pub enum HarnessError {
    #[error("plugin initialization failed with status {0}")]
    PluginInit(u32),
    #[error("plugin returned an incompatible function table")]
    IncompatiblePlugin,
    #[error("plugin frame JSON failed: {0}")]
    Frame(#[from] FrameError),
    #[error("timed out waiting for a plugin frame")]
    Timeout,
    #[error("plugin frame channel disconnected")]
    Disconnected,
}

#[derive(Debug)]
struct PostedBytes {
    lane: Lane,
    bytes: Vec<u8>,
}

#[derive(Debug)]
struct HostState {
    sender: mpsc::Sender<PostedBytes>,
    post_status: AtomicU32,
    post_status_script: Mutex<VecDeque<u32>>,
}

unsafe extern "C" fn post_frame(
    host_handle: *mut c_void,
    lane: u32,
    data_ptr: *const u8,
    data_len: usize,
) -> u32 {
    if host_handle.is_null()
        || (data_ptr.is_null() && data_len != 0)
        || data_len > MAX_CAPTURED_FRAME_BYTES
    {
        return POST_FRAME_CLOSED;
    }
    // SAFETY: PluginHarness retains the Arc that owns this mutex-backed state
    // until after plugin destruction.
    let state = unsafe { &*host_handle.cast::<HostState>() };
    let configured_status = state
        .post_status_script
        .lock()
        .ok()
        .and_then(|mut script| script.pop_front())
        .unwrap_or_else(|| state.post_status.load(Ordering::SeqCst));
    if configured_status != POST_FRAME_ACCEPTED {
        return configured_status;
    }
    let Some(lane) = Lane::from_raw(lane) else {
        return POST_FRAME_CLOSED;
    };
    let bytes = if data_len == 0 {
        Vec::new()
    } else {
        // SAFETY: The callback contract provides a readable borrowed buffer and
        // the host copies it before returning.
        unsafe { std::slice::from_raw_parts(data_ptr, data_len) }.to_vec()
    };
    if state.sender.send(PostedBytes { lane, bytes }).is_ok() {
        POST_FRAME_ACCEPTED
    } else {
        POST_FRAME_CLOSED
    }
}

pub struct PluginHarness {
    host_state: Arc<HostState>,
    _host_api: Box<HostApi>,
    receiver: mpsc::Receiver<PostedBytes>,
    plugin: PluginApi,
    destroyed: bool,
}

impl fmt::Debug for PluginHarness {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PluginHarness")
            .field("plugin", &self.plugin)
            .field("destroyed", &self.destroyed)
            .finish_non_exhaustive()
    }
}

impl PluginHarness {
    /// Starts a plugin against the in-process test host.
    ///
    /// # Errors
    ///
    /// Returns an error when the plugin rejects initialization or publishes an
    /// incompatible ABI table.
    pub fn start(entry: PluginEntryFn) -> Result<Self, HarnessError> {
        let (sender, receiver) = mpsc::channel();
        let host_state = Arc::new(HostState {
            sender,
            post_status: AtomicU32::new(POST_FRAME_ACCEPTED),
            post_status_script: Mutex::new(VecDeque::new()),
        });
        let handle = Arc::as_ptr(&host_state).cast_mut().cast::<c_void>();
        // SAFETY: The Arc is retained by the harness, its Sender and AtomicU32
        // are safe for concurrent access, and post_frame never unwinds.
        let host = Box::new(unsafe { HostApi::new(handle, post_frame) });
        let mut plugin = PluginApi::EMPTY;
        // SAFETY: Both fixed tables remain readable/writable for this call. The
        // boxed host table has stable storage retained through destruction.
        let status = unsafe {
            entry(
                &raw const *host,
                &raw mut plugin,
                core::mem::size_of::<PluginApi>(),
            )
        };
        if status != INIT_OK {
            leak_rejected_host(host, host_state);
            return Err(HarnessError::PluginInit(status));
        }
        if !plugin.is_compatible() {
            leak_rejected_host(host, host_state);
            return Err(HarnessError::IncompatiblePlugin);
        }
        Ok(Self {
            host_state,
            _host_api: host,
            receiver,
            plugin,
            destroyed: false,
        })
    }

    /// Dispatches one typed frame to the plugin.
    ///
    /// # Errors
    ///
    /// Returns an error when the frame cannot be encoded.
    pub fn send(&mut self, lane: Lane, frame: &Frame) -> Result<CallOutcome, HarnessError> {
        let bytes = frame.encode()?;
        Ok(self.send_raw(lane, &bytes))
    }

    pub fn send_raw(&mut self, lane: Lane, bytes: &[u8]) -> CallOutcome {
        let Some(dispatch) = self.plugin.on_frame else {
            return CallOutcome::Failed;
        };
        // SAFETY: start validated this live handle/callback, and bytes remain
        // readable for the duration of the call.
        CallOutcome::from_raw(unsafe {
            dispatch(
                self.plugin.plugin_handle,
                lane.as_raw(),
                bytes.as_ptr(),
                bytes.len(),
            )
        })
    }

    /// Receives and decodes one plugin frame within `timeout`.
    ///
    /// # Errors
    ///
    /// Returns an error on timeout, channel closure, or an invalid frame.
    pub fn recv(&self, timeout: Duration) -> Result<CapturedFrame, HarnessError> {
        let posted = self
            .receiver
            .recv_timeout(timeout)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => HarnessError::Timeout,
                mpsc::RecvTimeoutError::Disconnected => HarnessError::Disconnected,
            })?;
        Ok(CapturedFrame {
            lane: posted.lane,
            frame: Frame::decode(&posted.bytes)?,
        })
    }

    /// Attempts to receive and decode one plugin frame without blocking.
    ///
    /// # Errors
    ///
    /// Returns an error when the channel is closed or the frame is invalid.
    pub fn try_recv(&self) -> Result<Option<CapturedFrame>, HarnessError> {
        let posted = match self.receiver.try_recv() {
            Ok(posted) => posted,
            Err(mpsc::TryRecvError::Empty) => return Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => return Err(HarnessError::Disconnected),
        };
        Ok(Some(CapturedFrame {
            lane: posted.lane,
            frame: Frame::decode(&posted.bytes)?,
        }))
    }

    pub fn set_post_outcome(&self, outcome: PostFrameOutcome) {
        let raw = raw_post_outcome(outcome);
        if let Ok(mut script) = self.host_state.post_status_script.lock() {
            script.clear();
        }
        self.host_state.post_status.store(raw, Ordering::SeqCst);
    }

    /// Overrides successive host callback results, then falls back to the
    /// outcome configured by [`Self::set_post_outcome`].
    pub fn set_post_outcomes(&self, outcomes: impl IntoIterator<Item = PostFrameOutcome>) {
        if let Ok(mut script) = self.host_state.post_status_script.lock() {
            *script = outcomes.into_iter().map(raw_post_outcome).collect();
        }
    }

    pub fn shutdown(&mut self) -> CallOutcome {
        let Some(shutdown) = self.plugin.shutdown else {
            return CallOutcome::Ok;
        };
        // SAFETY: The handle is live until destroy and &mut self serializes calls.
        CallOutcome::from_raw(unsafe { shutdown(self.plugin.plugin_handle) })
    }

    fn destroy(&mut self) {
        if self.destroyed {
            return;
        }
        if let Some(destroy) = self.plugin.destroy {
            // SAFETY: This is the harness's sole destroy call for the live handle.
            let status = unsafe { destroy(self.plugin.plugin_handle) };
            debug_assert!(matches!(
                status,
                CALL_OK | CALL_FAILED | rsi_meta_plugin::CALL_PANICKED
            ));
        }
        self.destroyed = true;
    }
}

fn leak_rejected_host(host: Box<HostApi>, host_state: Arc<HostState>) {
    // A rejected plugin has observed both pointers but offers no validated
    // destroy callback. Retaining these small allocations prevents callback
    // use-after-free if trusted fixture code spawned work before rejecting.
    let _ = Box::leak(host);
    let _ = Arc::into_raw(host_state);
}

fn raw_post_outcome(outcome: PostFrameOutcome) -> u32 {
    match outcome {
        PostFrameOutcome::Accepted => POST_FRAME_ACCEPTED,
        PostFrameOutcome::WouldBlock => POST_FRAME_WOULD_BLOCK,
        PostFrameOutcome::Closed => POST_FRAME_CLOSED,
        PostFrameOutcome::Unknown(raw) => raw,
    }
}

impl Drop for PluginHarness {
    fn drop(&mut self) {
        self.destroy();
    }
}
