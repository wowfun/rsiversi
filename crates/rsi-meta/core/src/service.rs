use crate::{ContractId, ContractVersion, FiberGeneration, FiberId, ServiceKey, SupplyId};
use std::sync::{Arc, Mutex};

mod admission;
mod call;
mod channel;
mod invocation;
mod message;
mod message_admission;
mod message_scheduler;
mod message_waiter;

pub(crate) use admission::{AdmissionLease, LeaseGuard};
pub(crate) use call::CallLease;
pub use call::{CancellationObserver, Capability, CapabilityCall};
pub(crate) use channel::ResponseMessage;
pub use channel::{ProviderChannel, ServiceEndpoint};
pub use invocation::{CallerView, InvocationContext};
pub(crate) use message::BufferedMessage;
pub use message::Message;
pub(crate) use message_admission::BufferedMessageAdmission;
pub(crate) use message_waiter::MessageChannel;

#[derive(Debug)]
pub(crate) struct ProviderBinding {
    pub supply: SupplyId,
    pub key: ServiceKey,
    pub contract: ContractId,
    pub version: ContractVersion,
    pub provider: FiberId,
    pub generation: FiberGeneration,
    // Withdrawal takes the endpoint only after `lease` is closed, sealed, and
    // drained. Each cloned endpoint is therefore protected by the CallDriver's
    // corresponding provider LeaseGuard after this mutex is released.
    pub endpoint: Mutex<Option<Arc<dyn ServiceEndpoint>>>,
    pub lease: Arc<AdmissionLease>,
}
