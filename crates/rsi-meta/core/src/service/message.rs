use super::message_admission::{BufferedMessageAdmission, BufferedMessagePermit};
use super::message_waiter::MessageChannel;
use crate::runtime::{CapabilityEntry, ResourceLedger, ResourceReservation};
use crate::{Capability, Context, MetaError, Result};
use std::sync::Arc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// One opaque byte payload with transferable generation-fenced capabilities.
#[derive(Clone, Debug)]
pub struct Message {
    bytes: Box<[u8]>,
    capabilities: Box<[Capability]>,
}

impl Message {
    /// Creates a byte-only message; the owning send operation enforces bounds.
    pub fn new(bytes: impl Into<Box<[u8]>>) -> Self {
        Self {
            bytes: bytes.into(),
            capabilities: Box::default(),
        }
    }

    /// Creates one message from exact bytes and transferable capabilities.
    pub fn from_parts(
        bytes: impl Into<Box<[u8]>>,
        capabilities: impl Into<Box<[Capability]>>,
    ) -> Self {
        Self {
            bytes: bytes.into(),
            capabilities: capabilities.into(),
        }
    }

    /// Borrows the exact message bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Borrows the transferable capabilities in wire order.
    pub fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    /// Consumes the message into its exact byte and capability values.
    pub fn into_parts(self) -> (Vec<u8>, Vec<Capability>) {
        (self.bytes.into_vec(), self.capabilities.into_vec())
    }
}

#[derive(Debug)]
pub(crate) struct BufferedMessage {
    bytes: Box<[u8]>,
    entries: Box<[Arc<CapabilityEntry>]>,
    byte_reservation: Option<ResourceReservation>,
    capability_reservation: Option<ResourceReservation>,
    message_permit: BufferedMessagePermit,
}

#[derive(Debug)]
pub(super) struct PreparedMessage {
    bytes: Box<[u8]>,
    entries: Box<[Arc<CapabilityEntry>]>,
}

impl BufferedMessage {
    pub(super) async fn acquire(
        message: PreparedMessage,
        message_admission: &Arc<BufferedMessageAdmission>,
        channel: &Arc<MessageChannel>,
        byte_resources: &Arc<ResourceLedger>,
        capability_resources: &Arc<ResourceLedger>,
        cancellation: &CancellationToken,
        deadline: Instant,
    ) -> Result<Self> {
        let PreparedMessage { bytes, entries } = message;
        let byte_count = bytes.len();
        let capability_count = entries.len();
        let message_permit = message_admission
            .acquire(
                channel,
                byte_count,
                capability_count,
                byte_resources,
                capability_resources,
                cancellation,
                deadline,
            )
            .await?;
        let byte_reservation =
            if byte_count == 0 {
                None
            } else {
                Some(byte_resources.try_reserve(byte_count).ok_or(
                    MetaError::CapacityExhausted {
                        resource: "buffered message bytes",
                    },
                )?)
            };
        let capability_reservation = if capability_count == 0 {
            None
        } else {
            Some(capability_resources.try_reserve(capability_count).ok_or(
                MetaError::CapacityExhausted {
                    resource: "queued capability references",
                },
            )?)
        };
        Ok(Self {
            bytes,
            entries,
            byte_reservation,
            capability_reservation,
            message_permit,
        })
    }

    pub(super) fn validate_transfer(&self) -> Result<()> {
        self.entries
            .iter()
            .try_for_each(|entry| entry.validate_transfer())
    }

    pub(super) fn into_message(self, receiver: &Context) -> Message {
        let Self {
            bytes,
            entries,
            byte_reservation,
            capability_reservation,
            message_permit,
        } = self;
        let capabilities = entries
            .into_vec()
            .into_iter()
            .map(|entry| Capability {
                holder: receiver.clone(),
                entry,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let message = Message {
            bytes,
            capabilities,
        };
        drop(byte_reservation);
        drop(capability_reservation);
        drop(message_permit);
        message
    }
}

impl PreparedMessage {
    pub(super) fn validate_and_strip(
        message: Message,
        sender: &Context,
        maximum_message_bytes: usize,
        maximum_capabilities_per_message: usize,
    ) -> Result<Self> {
        validate_message_bounds(
            &message,
            maximum_message_bytes,
            maximum_capabilities_per_message,
        )?;
        let Message {
            bytes,
            capabilities,
        } = message;
        for capability in &capabilities {
            capability.holder.ensure_same_authority(sender)?;
            capability.entry.validate_transfer()?;
        }
        let entries = capabilities
            .into_vec()
            .into_iter()
            .map(|capability| capability.entry)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Ok(Self { bytes, entries })
    }
}

pub(super) fn validate_message_bounds(
    message: &Message,
    maximum_message_bytes: usize,
    maximum_capabilities_per_message: usize,
) -> Result<()> {
    if message.as_bytes().len() > maximum_message_bytes {
        return Err(MetaError::PayloadTooLarge {
            maximum: maximum_message_bytes,
        });
    }
    if message.capabilities().len() > maximum_capabilities_per_message {
        return Err(MetaError::CapacityExhausted {
            resource: "capabilities per message",
        });
    }
    Ok(())
}
