use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, Capability, ConfigValue, Context, ContractVersion, FactoryIdentity, FiberId,
    InvocationContext, Message, MetaError, PayloadLimits, PluginFactory, PreparedActivation,
    ProviderChannel, Requirement, Result, Runtime, RuntimeLimits, ServiceEndpoint, TopologyLimits,
};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

#[path = "support/resolver.rs"]
mod resolver;
#[path = "message_capabilities/support.rs"]
mod support;
use resolver::resolved;

#[path = "message_capabilities/accounting.rs"]
mod accounting;
#[path = "message_capabilities/admission.rs"]
mod admission;
#[path = "message_capabilities/identity.rs"]
mod identity;
#[path = "message_capabilities/transfer.rs"]
mod transfer;
#[path = "message_capabilities/unary.rs"]
mod unary;

#[test]
fn message_retains_exact_bytes_without_runtime_accounting() {
    let message = Message::new(b"foundation".as_slice());

    assert_eq!(message.as_bytes(), b"foundation");
    assert!(message.capabilities().is_empty());
    let (bytes, capabilities) = message.into_parts();
    assert_eq!(bytes, b"foundation");
    assert!(capabilities.is_empty());
}
