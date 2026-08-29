use async_trait::async_trait;
use futures_util::FutureExt as _;
use rsi_meta::{
    ActivationPlan, Context, ContractVersion, DeadlineLimits, FactoryIdentity, FiberState,
    InvocationContext, LocalEventOptions, Message, MetaError, PluginFactory, PreparedActivation,
    ProviderChannel, Requirement, Result, Runtime, RuntimeLimits, ServiceEndpoint, SupplyHandle,
    TopologyLimits,
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::task::Poll;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[path = "support/resolver.rs"]
mod resolver;
mod support;
use resolver::resolved;

use support::{
    ContextCaptureFactory, Echo, EndpointFactory, FactorySpec, ListenerCaptureFactory, NoopEvent,
    NoopHandler, PassiveFactory,
};

const V1: ContractVersion = ContractVersion(1);

#[path = "lifecycle_invariants/failure_and_disposal.rs"]
mod failure_and_disposal;
#[path = "lifecycle_invariants/preparation_and_binding.rs"]
mod preparation_and_binding;
#[path = "lifecycle_invariants/runtime_ownership.rs"]
mod runtime_ownership;

#[path = "lifecycle_invariants/contract_invariants.rs"]
mod contract_invariants;
#[path = "lifecycle_invariants/foundation.rs"]
mod foundation;
