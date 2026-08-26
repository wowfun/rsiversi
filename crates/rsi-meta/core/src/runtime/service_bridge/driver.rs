#![allow(clippy::wildcard_imports)] // The driver is one implementation partition of Runtime calls.

use super::super::*;
use super::endpoint_driver::{CallTerminationSource, EndpointDriver};
use crate::service::{BufferedMessage, CallLease, LeaseGuard, MessageChannel, ResponseMessage};

pub(super) struct CallDriver {
    pub(super) provider_context: Context,
    pub(super) requests: mpsc::Receiver<BufferedMessage>,
    pub(super) responses: mpsc::Sender<ResponseMessage>,
    pub(super) terminal: mpsc::OwnedPermit<ResponseMessage>,
    pub(super) message_admission: Arc<BufferedMessageAdmission>,
    pub(super) response_channel: Arc<MessageChannel>,
    pub(super) byte_resources: Arc<ResourceLedger>,
    pub(super) capability_resources: Arc<ResourceLedger>,
    pub(super) cancellation: CancellationToken,
    pub(super) deadline: tokio::time::Instant,
    pub(super) maximum_message_bytes: usize,
    pub(super) maximum_capabilities_per_message: usize,
    pub(super) call_lease: Arc<CallLease>,
    pub(super) capability_use: CapabilityUse,
    pub(super) provider_lease: LeaseGuard,
    pub(super) runtime: Runtime,
}

impl CallDriver {
    pub(super) async fn run(
        self,
        endpoint: Arc<dyn ServiceEndpoint>,
        invocation: InvocationContext,
    ) {
        let Self {
            provider_context,
            mut requests,
            responses,
            terminal,
            message_admission,
            response_channel,
            byte_resources,
            capability_resources,
            cancellation,
            deadline,
            maximum_message_bytes,
            maximum_capabilities_per_message,
            call_lease,
            capability_use,
            provider_lease,
            runtime,
        } = self;
        let callback_lease = invocation.callback_lease();
        let outcome = {
            let provider_channel = crate::ProviderChannel {
                context: &provider_context,
                requests: &mut requests,
                responses: &responses,
                message_admission: &message_admission,
                response_channel: &response_channel,
                byte_resources: &byte_resources,
                capability_resources: &capability_resources,
                cancellation: &cancellation,
                deadline,
                maximum_message_bytes,
                maximum_capabilities_per_message,
            };
            EndpointDriver {
                endpoint: &endpoint,
                invocation,
                channel: provider_channel,
                callback_lease,
                runtime: &runtime,
                cancellation: &cancellation,
                deadline,
            }
            .run()
            .await
        };
        drop(requests);
        drop(responses);
        let endpoint_drop_panicked = drop_catching_unwind(endpoint);
        let terminal_result = if (outcome.cleanup_panicked || endpoint_drop_panicked)
            && matches!(
                outcome.source,
                CallTerminationSource::Endpoint | CallTerminationSource::Cancellation
            ) {
            Err(MetaError::ServiceEndpointPanicked)
        } else {
            outcome.selected
        };
        // Provider inboxes and callback-owned values are gone before the
        // unique terminal is published and generation leases are released.
        terminal.send(ResponseMessage::Terminal(terminal_result));
        drop(provider_lease);
        drop(capability_use);
        drop(call_lease);
        cancellation.cancel();
    }
}
