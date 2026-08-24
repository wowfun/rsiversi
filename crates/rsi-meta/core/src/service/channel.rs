use super::InvocationContext;
use super::byte_admission::{BufferedByteAdmission, BufferedBytePermit};
use crate::runtime::{ResourceLedger, ResourceReservation};
use crate::{MetaError, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// One bounded opaque byte frame in a service stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceFrame {
    bytes: Box<[u8]>,
}

impl ServiceFrame {
    /// Creates a frame; the owning send operation enforces its byte bound.
    pub fn new(bytes: impl Into<Box<[u8]>>) -> Self {
        Self {
            bytes: bytes.into(),
        }
    }

    /// Borrows the exact frame bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consumes the frame and returns its bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes.into_vec()
    }
}

#[derive(Debug)]
pub(crate) struct BufferedFrame {
    frame: ServiceFrame,
    _byte_reservation: Option<ResourceReservation>,
    _byte_lease: Option<BufferedBytePermit>,
}

pub(super) struct FrameBudget<'call> {
    pub(super) maximum_frame_bytes: usize,
    pub(super) byte_admission: &'call Arc<BufferedByteAdmission>,
    pub(super) byte_resources: &'call Arc<ResourceLedger>,
    pub(super) cancellation: &'call CancellationToken,
    pub(super) deadline: Instant,
}

impl BufferedFrame {
    async fn acquire(
        frame: ServiceFrame,
        byte_admission: &Arc<BufferedByteAdmission>,
        byte_resources: &Arc<ResourceLedger>,
        cancellation: &CancellationToken,
        deadline: Instant,
    ) -> Result<Self> {
        let bytes = frame.as_bytes().len();
        let (byte_lease, byte_reservation) = if bytes == 0 {
            (None, None)
        } else {
            let byte_lease = byte_admission
                .acquire(bytes, byte_resources, cancellation, deadline)
                .await?;
            let byte_reservation =
                byte_resources
                    .try_reserve(bytes)
                    .ok_or(MetaError::CapacityExhausted {
                        resource: "buffered service bytes",
                    })?;
            (Some(byte_lease), Some(byte_reservation))
        };
        Ok(Self {
            frame,
            _byte_reservation: byte_reservation,
            _byte_lease: byte_lease,
        })
    }

    pub(super) fn into_frame(self) -> ServiceFrame {
        self.frame
    }
}

pub(super) async fn send_frame<T: Send>(
    sender: &mpsc::Sender<T>,
    frame: ServiceFrame,
    wrap: impl FnOnce(BufferedFrame) -> T,
    budget: FrameBudget<'_>,
) -> Result<()> {
    if frame.as_bytes().len() > budget.maximum_frame_bytes {
        return Err(MetaError::PayloadTooLarge {
            maximum: budget.maximum_frame_bytes,
        });
    }
    if Instant::now() >= budget.deadline {
        budget.cancellation.cancel();
        return Err(MetaError::Timeout("service call"));
    }
    if budget.cancellation.is_cancelled() {
        return Err(MetaError::Cancelled);
    }
    let channel = tokio::select! {
        biased;
        () = tokio::time::sleep_until(budget.deadline) => {
            budget.cancellation.cancel();
            return Err(MetaError::Timeout("service call"));
        }
        () = budget.cancellation.cancelled() => return Err(MetaError::Cancelled),
        result = sender.reserve() => result.map_err(|_| MetaError::Cancelled)?,
    };
    let frame = BufferedFrame::acquire(
        frame,
        budget.byte_admission,
        budget.byte_resources,
        budget.cancellation,
        budget.deadline,
    )
    .await?;
    if Instant::now() >= budget.deadline {
        budget.cancellation.cancel();
        return Err(MetaError::Timeout("service call"));
    }
    if budget.cancellation.is_cancelled() {
        return Err(MetaError::Cancelled);
    }
    channel.send(wrap(frame));
    Ok(())
}

#[derive(Debug)]
pub(crate) enum ResponseMessage {
    Frame(BufferedFrame),
    Terminal(Result<()>),
}

/// Provider half of one bounded bidirectional service call.
///
/// The channel borrows the call driver's channel halves, so safe provider code
/// cannot detach them into a `'static` task:
///
/// ```compile_fail
/// use rsi_meta::ProviderChannel;
///
/// async fn detach(mut channel: ProviderChannel<'_>) {
///     tokio::spawn(async move {
///         let _ = channel.recv().await;
///     });
/// }
/// ```
#[derive(Debug)]
pub struct ProviderChannel<'call> {
    pub(crate) requests: &'call mut mpsc::Receiver<BufferedFrame>,
    pub(crate) responses: &'call mpsc::Sender<ResponseMessage>,
    pub(crate) byte_admission: &'call Arc<BufferedByteAdmission>,
    pub(crate) byte_resources: &'call Arc<ResourceLedger>,
    pub(crate) cancellation: &'call CancellationToken,
    pub(crate) deadline: Instant,
    pub(crate) maximum_frame_bytes: usize,
}

impl ProviderChannel<'_> {
    /// Receives the next caller frame, or `None` after finish or cancellation.
    pub async fn recv(&mut self) -> Option<ServiceFrame> {
        tokio::select! {
            biased;
            () = self.cancellation.cancelled() => None,
            () = tokio::time::sleep_until(self.deadline) => {
                self.cancellation.cancel();
                None
            }
            frame = self.requests.recv() => frame.map(BufferedFrame::into_frame),
        }
    }

    /// Sends one bounded successful response frame.
    pub async fn send(&self, frame: ServiceFrame) -> Result<()> {
        send_frame(
            self.responses,
            frame,
            ResponseMessage::Frame,
            FrameBudget {
                maximum_frame_bytes: self.maximum_frame_bytes,
                byte_admission: self.byte_admission,
                byte_resources: self.byte_resources,
                cancellation: self.cancellation,
                deadline: self.deadline,
            },
        )
        .await
    }

    /// Returns cooperative cancellation for this call.
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }
}

/// Async byte-stream provider implemented at the service seam.
#[async_trait]
pub trait ServiceEndpoint: std::fmt::Debug + Send + Sync + 'static {
    /// Serves one admitted generation-fenced call to completion.
    async fn serve(
        &self,
        invocation: InvocationContext,
        channel: ProviderChannel<'_>,
    ) -> Result<()>;
}
