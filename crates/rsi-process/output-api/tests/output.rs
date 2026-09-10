use async_trait::async_trait;
use rsi_api_protocol::{
    ApiClient, ApiClientContract, ApiDispatch, ApiDispatchContract, ApiError, ApiMessage,
    ApiOutput, ByteBudget, CallOrigin, ConnectionDescription, EndpointId, HostEpoch,
    OperationClass, OperationSpec, RetainedBytes,
};
use rsi_meta::{
    ActivationPlan, PluginFactory, PreparedActivation, ResolvedFactory, Runtime, UpdateMode,
};
use rsi_process::{ProcessContract, ProcessError, ProcessOutputCache, ProcessOutputCacheContract};
use rsi_process_output_api::{OutputApiFactory, OutputClient, OutputClientFactory};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug)]
struct Connection {
    dispatch: Arc<dyn ApiDispatch>,
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
}
#[async_trait]
impl ApiClient for Connection {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        ByteBudget::default()
    }
    async fn call(
        &self,
        operation: &OperationSpec,
        input: RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        self.dispatch
            .admit(&operation.id, CallOrigin::Local)?
            .invoke(input)
            .await
    }
}
#[derive(Debug)]
struct ConnectionFactory(Arc<dyn ApiClient>);
#[async_trait]
impl PluginFactory for ConnectionFactory {
    fn prepare(&self, _: &Value) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<ApiClientContract>(self.0.clone())?;
        plan.defer(
            "withdraw fixture connection",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
fn linked(name: &str, factory: Arc<dyn PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(name, "test", UpdateMode::Replayable, factory)
}
fn description() -> ConnectionDescription {
    ConnectionDescription {
        wire_version: 1,
        endpoint_id: EndpointId::from_bytes([1; 16]),
        host_epoch: HostEpoch::from_bytes([2; 16]),
    }
}
async fn client_runtime(api: Arc<dyn ApiClient>) -> Runtime {
    let runtime = Runtime::default();
    for (name, factory) in [
        (
            "connection",
            Arc::new(ConnectionFactory(api)) as Arc<dyn PluginFactory>,
        ),
        ("output-client", Arc::new(OutputClientFactory)),
    ] {
        runtime
            .root()
            .apply(linked(name, factory), Value::Null)
            .await
            .unwrap();
    }
    runtime
}

#[cfg(unix)]
async fn local_capture(path: &std::path::Path) -> (Runtime, String, Vec<u8>) {
    use rsi_sandbox::{
        ConfinedProcess, EnforcementStamp, SandboxBackend, SandboxFileSystem, SandboxMode,
        SandboxNetwork, SandboxScratch,
    };
    let runtime = Runtime::default();
    for (name, factory, config) in [
        (
            "process",
            Arc::new(rsi_process_local::ProcessLocalFactory) as Arc<dyn PluginFactory>,
            json!({"output_cache":{"directory":path.canonicalize().unwrap().join("output")}}),
        ),
        ("api", Arc::new(rsi_api::ApiFactory), Value::Null),
        ("output-api", Arc::new(OutputApiFactory), Value::Null),
    ] {
        runtime
            .root()
            .apply(linked(name, factory), config)
            .await
            .unwrap();
    }
    let process = runtime.root().lookup_local::<ProcessContract>().unwrap();
    let mut source = vec![0; 65_537];
    source[..8].copy_from_slice(&[0, 255, 27, b'[', b'3', b'1', b'm', 128]);
    let managed = process
        .spawn(rsi_process::ProcessSpec {
            process: ConfinedProcess {
                program: "/bin/cat".into(),
                arguments: Vec::new(),
                cwd: path.into(),
                stamp: EnforcementStamp {
                    requested: SandboxMode::DangerFullAccess,
                    backend: SandboxBackend::Unconfined,
                    workspace: path.into(),
                    filesystem: SandboxFileSystem::Unconfined,
                    scratch: SandboxScratch::Host,
                    network: SandboxNetwork::Host,
                },
            },
            stdin: source.clone(),
            environment: Vec::new(),
            stdout_max_bytes: 32,
            stderr_max_bytes: 32,
            termination_grace_ms: 50,
        })
        .unwrap();
    assert_eq!(managed.wait().await.unwrap().exit_code, Some(0));
    let tail = managed.stdout().read_from(0).unwrap();
    assert!(tail.lossy);
    (runtime, tail.full_output.unwrap(), source)
}

#[tokio::test]
#[cfg(unix)]
async fn real_process_cache_pages_cross_independent_plugins_without_spawn_authority() {
    let temporary = tempfile::tempdir().unwrap();
    let (server, id, source) = local_capture(temporary.path()).await;
    let dispatch = server.root().lookup_local::<ApiDispatchContract>().unwrap();
    let api = Arc::new(Connection {
        operations: dispatch.operations(),
        dispatch: dispatch.clone(),
        description: description(),
    });
    let client = client_runtime(api.clone()).await;
    assert!(client.root().lookup_local::<ProcessContract>().is_none());
    let output = client
        .root()
        .lookup_local::<ProcessOutputCacheContract>()
        .unwrap();
    let first = output.read(&id, 0, 65_536).await.unwrap();
    assert_eq!(first.bytes.as_ref(), &source[..65_536]);
    let second = output.read(&id, first.next_offset, 65_536).await.unwrap();
    assert_eq!(second.bytes.as_ref(), &source[65_536..]);
    let eof = output.read(&id, second.next_offset, 1).await.unwrap();
    assert!(eof.bytes.is_empty());
    assert_eq!(eof.total_bytes, source.len() as u64);
    assert!(matches!(
        output.read(&id, u64::MAX, 1).await,
        Err(ProcessError::InvalidInput(_))
    ));
    assert!(matches!(
        output.read(&"f".repeat(32), 0, 1).await,
        Err(ProcessError::Io(_))
    ));
    let operation = &api.operations[0];
    let invocation = dispatch.admit(&operation.id, CallOrigin::Local).unwrap();
    let input = invocation
        .input_budget()
        .encode(
            &json!({"id":id,"offset":0,"limit":1,"session":"foreign"}),
            256,
        )
        .unwrap();
    assert!(matches!(
        invocation.invoke(input).await,
        Err(ApiError::Invalid(_))
    ));
    assert!(client.shutdown().await.is_clean());
    assert!(
        client
            .root()
            .lookup_local::<ProcessOutputCacheContract>()
            .is_none()
    );
    assert!(server.shutdown().await.is_clean());
    assert!(dispatch.operations().is_empty());
}

#[derive(Debug)]
struct Peer {
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    metadata: Value,
    binary: bool,
    budget: ByteBudget,
}
#[async_trait]
impl ApiClient for Peer {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        ByteBudget::default()
    }
    async fn call(
        &self,
        _: &OperationSpec,
        _: RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        Ok(ApiOutput::Reply(ApiMessage {
            json: self.budget.encode(&self.metadata, 1024)?,
            binary: self
                .binary
                .then(|| self.budget.copy(&[0, 255, 27, 128]))
                .transpose()?,
        }))
    }
}
async fn read_spec() -> OperationSpec {
    #[derive(Debug)]
    struct Unused;
    #[async_trait]
    impl ProcessOutputCache for Unused {
        async fn read(
            &self,
            _: &str,
            _: u64,
            _: usize,
        ) -> rsi_process::Result<rsi_process::OutputPage> {
            panic!("only registration metadata needed");
        }
    }
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(linked("api", Arc::new(rsi_api::ApiFactory)), Value::Null)
        .await
        .unwrap();
    let registrar = runtime
        .root()
        .lookup_local::<rsi_api_protocol::ApiRegistrarContract>()
        .unwrap();
    let lease =
        rsi_process_output_api::register_output(registrar.as_ref(), Arc::new(Unused)).unwrap();
    let spec = runtime
        .root()
        .lookup_local::<ApiDispatchContract>()
        .unwrap()
        .operations()
        .remove(0);
    lease.close().await;
    assert!(runtime.shutdown().await.is_clean());
    spec
}

#[tokio::test]
async fn byte_slices_keep_receive_admission_and_malformed_pages_never_escape() {
    let operation = read_spec().await;
    let id = "a".repeat(32);
    let valid = json!({"id":id,"offset":0,"next_offset":4,"total_bytes":4});
    let budget = ByteBudget::new(2048).unwrap();
    let peer = |metadata, binary| {
        Arc::new(Peer {
            description: description(),
            operations: vec![operation.clone()],
            metadata,
            binary,
            budget: budget.clone(),
        })
    };
    let client = OutputClient::new(peer(valid.clone(), true)).unwrap();
    let page = client.read(&id, 0, 4).await.unwrap();
    assert_eq!(budget.used(), 4);
    let slice = page.bytes.slice(1..2);
    let clone = slice.clone();
    drop(page);
    drop(slice);
    assert_eq!(budget.used(), 4);
    assert_eq!(clone.as_ref(), &[255]);
    drop(clone);
    assert_eq!(budget.used(), 0);
    for (key, value) in [
        ("id", json!("b".repeat(32))),
        ("offset", json!(1)),
        ("next_offset", json!(3)),
        ("total_bytes", json!(3)),
        ("total_bytes", json!(u64::MAX)),
        ("foreign", json!(true)),
    ] {
        let mut metadata = valid.clone();
        metadata[key] = value;
        let client = OutputClient::new(peer(metadata, true)).unwrap();
        assert!(matches!(
            client.read(&id, 0, 4).await,
            Err(ProcessError::Api(ApiError::Invalid(_)))
        ));
        assert_eq!(budget.used(), 0);
    }
    let missing = OutputClient::new(peer(valid, false)).unwrap();
    assert!(missing.read(&id, 0, 4).await.is_err());
    assert_eq!(budget.used(), 0);
    assert!(matches!(
        client.read(&id, 0, 3).await,
        Err(ProcessError::Api(ApiError::Invalid(_)))
    ));
    assert_eq!(budget.used(), 0);
}
