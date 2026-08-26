use super::message::{BufferedMessage, Message, PreparedMessage};
use super::message_admission::BufferedMessageAdmission;
use super::message_waiter::MessageChannel;
use super::{CancellationObserver, InvocationContext};
use crate::runtime::ResourceLedger;
use crate::{Context, MetaError, Result};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub(super) struct MessageBudget<'call> {
    pub(super) sender: &'call Context,
    pub(super) maximum_message_bytes: usize,
    pub(super) maximum_capabilities_per_message: usize,
    pub(super) message_admission: &'call Arc<BufferedMessageAdmission>,
    pub(super) channel: &'call Arc<MessageChannel>,
    pub(super) byte_resources: &'call Arc<ResourceLedger>,
    pub(super) capability_resources: &'call Arc<ResourceLedger>,
    pub(super) cancellation: &'call CancellationToken,
    pub(super) deadline: Instant,
}

pub(super) async fn send_message<T: Send>(
    sender: &mpsc::Sender<T>,
    message: Message,
    wrap: impl FnOnce(BufferedMessage) -> T,
    budget: MessageBudget<'_>,
) -> Result<()> {
    let message = PreparedMessage::validate_and_strip(
        message,
        budget.sender,
        budget.maximum_message_bytes,
        budget.maximum_capabilities_per_message,
    )?;
    if Instant::now() >= budget.deadline {
        budget.cancellation.cancel();
        return Err(MetaError::Timeout("service call"));
    }
    if budget.cancellation.is_cancelled() {
        return Err(MetaError::Cancelled);
    }
    let message = BufferedMessage::acquire(
        message,
        budget.message_admission,
        budget.channel,
        budget.byte_resources,
        budget.capability_resources,
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
    message.validate_transfer()?;
    if Instant::now() >= budget.deadline {
        budget.cancellation.cancel();
        return Err(MetaError::Timeout("service call"));
    }
    if budget.cancellation.is_cancelled() {
        return Err(MetaError::Cancelled);
    }
    let channel = match sender.try_reserve() {
        Ok(channel) => channel,
        Err(mpsc::error::TrySendError::Closed(())) => return Err(MetaError::Cancelled),
        Err(mpsc::error::TrySendError::Full(())) => {
            return Err(MetaError::Service(
                "message channel position admission lost synchronization".to_owned(),
            ));
        }
    };
    channel.send(wrap(message));
    Ok(())
}

#[derive(Debug)]
pub(crate) enum ResponseMessage {
    Message(BufferedMessage),
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
    pub(crate) context: &'call Context,
    pub(crate) requests: &'call mut mpsc::Receiver<BufferedMessage>,
    pub(crate) responses: &'call mpsc::Sender<ResponseMessage>,
    pub(crate) message_admission: &'call Arc<BufferedMessageAdmission>,
    pub(crate) response_channel: &'call Arc<MessageChannel>,
    pub(crate) byte_resources: &'call Arc<ResourceLedger>,
    pub(crate) capability_resources: &'call Arc<ResourceLedger>,
    pub(crate) cancellation: &'call CancellationToken,
    pub(crate) deadline: Instant,
    pub(crate) maximum_message_bytes: usize,
    pub(crate) maximum_capabilities_per_message: usize,
}

impl ProviderChannel<'_> {
    /// Receives the next caller Message, or `None` after finish or cancellation.
    pub async fn recv(&mut self) -> Option<Message> {
        tokio::select! {
            biased;
            () = self.cancellation.cancelled() => None,
            () = tokio::time::sleep_until(self.deadline) => {
                self.cancellation.cancel();
                None
            }
            message = self.requests.recv() => {
                message.map(|message| message.into_message(self.context))
            },
        }
    }

    /// Sends one bounded successful response Message.
    pub async fn send(&self, message: Message) -> Result<()> {
        send_message(
            self.responses,
            message,
            ResponseMessage::Message,
            MessageBudget {
                sender: self.context,
                maximum_message_bytes: self.maximum_message_bytes,
                maximum_capabilities_per_message: self.maximum_capabilities_per_message,
                message_admission: self.message_admission,
                channel: self.response_channel,
                byte_resources: self.byte_resources,
                capability_resources: self.capability_resources,
                cancellation: self.cancellation,
                deadline: self.deadline,
            },
        )
        .await
    }

    /// Returns an observation-only view of cooperative cancellation for this call.
    pub fn cancellation(&self) -> CancellationObserver {
        CancellationObserver::new(self.cancellation.clone())
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
