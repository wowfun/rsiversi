use async_trait::async_trait;
use futures_util::StreamExt as _;
use rsi_api_protocol::{
    ApiClientContract, ApiError, ApiOutput, ConnectionDescription, EndpointId, HostEpoch,
    OperationAccess, OperationCatalog, OperationClass, OperationEffect, OperationId, OperationSpec,
    RequestEncoding, describe_operation, operations_operation,
    portable::{self, Description, Header},
};
use rsi_meta::{
    ActivationPlan, ConfigValue, ContractVersion, InvocationContext, Message, MetaError,
    PluginFactory, PreparedActivation, ProviderChannel, ResolvedFactory, Runtime, ServiceEndpoint,
    UpdateMode,
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug)]
enum Fault {
    Clean,
    WrongVersion,
    MissingDescribe,
    MissingOperations,
    Empty,
    Gap,
    OversizedFragment,
    OversizedLength,
    Overflow,
    Truncated,
    InvalidJson,
    Trailing,
    FailedTerminal,
    UnknownField,
    UnexpectedCapability,
    StreamNoEnd,
    StreamFailedTerminal,
    Huge,
}
fn operation(fault: Fault, effect: OperationEffect) -> OperationSpec {
    OperationSpec {
        id: OperationId::new("probe", "run", 1).unwrap(),
        access: OperationAccess::Authenticated,
        class: if matches!(fault, Fault::StreamNoEnd | Fault::StreamFailedTerminal) {
            OperationClass::Subscription
        } else {
            OperationClass::Data
        },
        effect,
        encoding: RequestEncoding::Binary,
        maximum_request_bytes: 1,
        maximum_response_bytes: if matches!(fault, Fault::Huge) {
            rsi_api_protocol::MAXIMUM_API_BYTES
        } else {
            128 * 1024
        },
    }
}
async fn header(channel: &mut ProviderChannel<'_>, header: &Header) -> rsi_meta::Result<()> {
    let mut bytes = vec![portable::HEADER_TAG];
    bytes.extend(serde_json::to_vec(header).unwrap());
    channel.send(Message::new(bytes)).await
}
async fn fragment(
    channel: &mut ProviderChannel<'_>,
    offset: u32,
    bytes: &[u8],
) -> rsi_meta::Result<()> {
    let mut frame = vec![portable::FRAGMENT_TAG];
    frame.extend(offset.to_le_bytes());
    frame.extend(bytes);
    channel.send(Message::new(frame)).await
}
#[derive(Debug)]
struct Provider {
    fault: Fault,
    effect: OperationEffect,
}
#[async_trait]
impl PluginFactory for Provider {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan.context().provide(
            "probe.api",
            portable::CONTRACT,
            ContractVersion(1),
            Arc::new(Self {
                fault: self.fault,
                effect: self.effect,
            }),
        )?;
        plan.defer(
            "probe API",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
impl Provider {
    fn description(&self) -> Description {
        let mut description = Description {
            connection: ConnectionDescription {
                wire_version: 1,
                endpoint_id: EndpointId::from_bytes([1; 16]),
                host_epoch: HostEpoch::from_bytes([2; 16]),
            },
            operations: OperationCatalog::new(vec![
                describe_operation(),
                operations_operation(),
                operation(self.fault, self.effect),
            ])
            .unwrap(),
        };
        if matches!(self.fault, Fault::WrongVersion) {
            description.connection.wire_version = 2;
        }
        if matches!(
            self.fault,
            Fault::MissingDescribe | Fault::MissingOperations
        ) {
            let excluded = if matches!(self.fault, Fault::MissingDescribe) {
                describe_operation()
            } else {
                operations_operation()
            };
            description.operations = OperationCatalog::new(
                description
                    .operations
                    .operations()
                    .iter()
                    .filter(|op| **op != excluded)
                    .cloned()
                    .collect(),
            )
            .unwrap();
        }
        description
    }
}
#[async_trait]
impl ServiceEndpoint for Provider {
    async fn serve(
        &self,
        invocation: InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> rsi_meta::Result<()> {
        let message = channel.recv().await.unwrap();
        let request: Header = serde_json::from_slice(&message.as_bytes()[1..]).unwrap();
        assert!(channel.recv().await.is_none());
        if matches!(request, Header::Describe {}) {
            let description = self.description();
            let bytes = serde_json::to_vec(&description).unwrap();
            header(&mut channel, &Header::Description { bytes: bytes.len() }).await?;
            return fragment(&mut channel, 0, &bytes).await;
        }
        if matches!(self.fault, Fault::UnknownField) {
            return channel
                .send(Message::new(
                    b"\0{\"kind\":\"reply\",\"json\":2,\"binary\":null,\"origin\":\"local\"}"
                        .as_slice(),
                ))
                .await;
        }
        if matches!(self.fault, Fault::UnexpectedCapability) {
            let capability = invocation.provider_context().service("probe.api")?;
            return channel
                .send(Message::from_parts(
                    b"\0{\"kind\":\"reply\",\"json\":2,\"binary\":null}".as_slice(),
                    vec![capability],
                ))
                .await;
        }
        if matches!(self.fault, Fault::StreamNoEnd | Fault::StreamFailedTerminal) {
            header(&mut channel, &Header::Stream {}).await?;
            if matches!(self.fault, Fault::StreamFailedTerminal) {
                header(&mut channel, &Header::End {}).await?;
                return Err(MetaError::Service("failed stream terminal".into()));
            }
            return Ok(());
        }
        let (json, binary) = match self.fault {
            Fault::OversizedLength => (128 * 1024 + 1, None),
            Fault::Overflow => (usize::MAX, Some(1)),
            Fault::Huge => (2, Some(rsi_api_protocol::MAXIMUM_API_BYTES - 2)),
            _ => (2, None),
        };
        header(&mut channel, &Header::Reply { json, binary }).await?;
        match self.fault {
            Fault::Empty => {
                fragment(&mut channel, 0, b"").await?;
                // Keep the reply open: EOF must not be what rejects this frame.
                std::future::pending::<()>().await;
            }
            Fault::Gap => fragment(&mut channel, 1, b"{}").await?,
            Fault::OversizedFragment => {
                fragment(
                    &mut channel,
                    0,
                    &vec![b' '; portable::MAXIMUM_FRAGMENT_BYTES + 1],
                )
                .await?;
            }
            Fault::Truncated => fragment(&mut channel, 0, b"{").await?,
            Fault::InvalidJson => fragment(&mut channel, 0, b"xx").await?,
            Fault::OversizedLength | Fault::Overflow => {}
            Fault::Huge => {
                let total = rsi_api_protocol::MAXIMUM_API_BYTES;
                for offset in (0..total).step_by(portable::MAXIMUM_FRAGMENT_BYTES) {
                    let mut bytes = vec![0; portable::MAXIMUM_FRAGMENT_BYTES.min(total - offset)];
                    if offset == 0 {
                        bytes[..2].copy_from_slice(b"{}");
                    }
                    fragment(&mut channel, u32::try_from(offset).unwrap(), &bytes).await?;
                }
            }
            _ => fragment(&mut channel, 0, b"{}").await?,
        }
        if matches!(self.fault, Fault::Trailing) {
            fragment(&mut channel, 2, b"extra").await?;
        }
        if matches!(self.fault, Fault::FailedTerminal) {
            return Err(MetaError::Service("failed finite terminal".into()));
        }
        Ok(())
    }
}
async fn fixture(
    fault: Fault,
    effect: OperationEffect,
) -> (Runtime, Arc<dyn rsi_api_protocol::ApiClient>) {
    let runtime = Runtime::default();
    let provider = ResolvedFactory::linked(
        "probe",
        "1",
        UpdateMode::Replayable,
        Arc::new(Provider { fault, effect }),
    );
    runtime
        .root()
        .apply(provider, ConfigValue::Null)
        .await
        .unwrap();
    runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "import",
                "1",
                UpdateMode::Replayable,
                Arc::new(rsi_api_portable::PortableApiClientFactory),
            ),
            serde_json::json!({"service":"probe.api"}),
        )
        .await
        .unwrap();
    let client = runtime.root().lookup_local::<ApiClientContract>().unwrap();
    (runtime, client)
}

#[tokio::test]
async fn malformed_payloads_and_terminals_never_publish_a_finite_success_or_replay() {
    for fault in [
        Fault::Clean,
        Fault::Empty,
        Fault::Gap,
        Fault::OversizedFragment,
        Fault::OversizedLength,
        Fault::Overflow,
        Fault::Truncated,
        Fault::InvalidJson,
        Fault::Trailing,
        Fault::FailedTerminal,
        Fault::UnknownField,
        Fault::UnexpectedCapability,
    ] {
        for effect in [OperationEffect::Read, OperationEffect::Mutation] {
            let (runtime, client) = fixture(fault, effect).await;
            let input = client.input_budget(OperationClass::Data).copy(b"").unwrap();
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                client.call(&operation(fault, effect), input),
            )
            .await
            .expect("malformed frame must fail without waiting for peer EOF");
            if matches!(fault, Fault::Clean) {
                assert!(result.is_ok(), "{fault:?} {effect:?}: {result:?}");
            } else if effect == OperationEffect::Mutation {
                assert!(
                    matches!(result, Err(ApiError::OutcomeUnknown)),
                    "{fault:?}: {result:?}"
                );
            } else {
                assert!(result.is_err(), "{fault:?}");
            }
            assert!(runtime.shutdown().await.is_clean());
        }
    }
}

#[tokio::test]
async fn a_stream_requires_its_explicit_end_and_clean_meta_terminal() {
    for fault in [Fault::StreamNoEnd, Fault::StreamFailedTerminal] {
        let (runtime, client) = fixture(fault, OperationEffect::Read).await;
        let input = client
            .input_budget(OperationClass::Subscription)
            .copy(b"")
            .unwrap();
        let ApiOutput::Stream(mut stream) = client
            .call(&operation(fault, OperationEffect::Read), input)
            .await
            .unwrap()
        else {
            panic!("stream")
        };
        assert!(stream.next().await.unwrap().is_err());
        assert!(stream.next().await.is_none());
        drop(stream);
        assert!(runtime.shutdown().await.is_clean());
    }
}

#[tokio::test]
async fn exact_64_mib_response_and_escaped_binary_slice_share_retained_admission() {
    let (runtime, client) = fixture(Fault::Huge, OperationEffect::Read).await;
    let operation = operation(Fault::Huge, OperationEffect::Read);
    let empty = || client.input_budget(OperationClass::Data).copy(b"").unwrap();
    let ApiOutput::Reply(reply) = client.call(&operation, empty()).await.unwrap() else {
        panic!("reply")
    };
    assert_eq!(reply.encoded_len(), rsi_api_protocol::MAXIMUM_API_BYTES);
    let escaped = reply.binary.as_ref().unwrap().slice(..1).unwrap();
    drop(reply);
    assert!(matches!(
        client.call(&operation, empty()).await,
        Err(ApiError::Capacity)
    ));
    drop(escaped);
    assert!(client.call(&operation, empty()).await.is_ok());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn import_rejects_unsupported_wire_version_and_incomplete_negotiation_grants() {
    for fault in [
        Fault::WrongVersion,
        Fault::MissingDescribe,
        Fault::MissingOperations,
    ] {
        let runtime = Runtime::default();
        let root = runtime.root();
        root.apply(
            ResolvedFactory::linked(
                "probe",
                "1",
                UpdateMode::Replayable,
                Arc::new(Provider {
                    fault,
                    effect: OperationEffect::Read,
                }),
            ),
            ConfigValue::Null,
        )
        .await
        .unwrap();
        let result = root
            .apply(
                ResolvedFactory::linked(
                    "import",
                    "1",
                    UpdateMode::Replayable,
                    Arc::new(rsi_api_portable::PortableApiClientFactory),
                ),
                serde_json::json!({"service":"probe.api"}),
            )
            .await;
        assert!(
            matches!(
                result.unwrap().snapshot().state,
                rsi_meta::FiberState::Failed(_)
            ),
            "{fault:?} grant admitted"
        );
        assert!(
            root.lookup_local::<ApiClientContract>().is_none(),
            "{fault:?} published an API client"
        );
        assert!(runtime.shutdown().await.is_clean());
    }
}
