use crate::{ContractId, ContractVersion, FiberGeneration, FiberId, ServiceKey};
use std::sync::Arc;

mod admission;
mod byte_admission;
mod call;
mod channel;
mod invocation;

pub(crate) use admission::{AdmissionLease, LeaseGuard};
pub(crate) use byte_admission::BufferedByteAdmission;
pub(crate) use call::CallLease;
pub use call::{ServiceCall, ServiceHandle};
pub(crate) use channel::{BufferedFrame, ResponseMessage};
pub use channel::{ProviderChannel, ServiceEndpoint, ServiceFrame};
pub use invocation::InvocationContext;

#[derive(Debug)]
pub(crate) struct ProviderBinding {
    pub key: ServiceKey,
    pub contract: ContractId,
    pub version: ContractVersion,
    pub provider: FiberId,
    pub generation: FiberGeneration,
    pub endpoint: Arc<dyn ServiceEndpoint>,
    pub lease: Arc<AdmissionLease>,
}
