use async_trait::async_trait;
use rsi_api::ApiRegistry;
use rsi_api_device_api::{DeviceApi, DeviceClient};
use rsi_api_protocol::{
    ApiClient, ApiDispatch, ApiError, ApiOutput, ByteBudget, CallOrigin, ConnectionDescription,
    DeviceAdministration, DeviceId, DeviceRecord, EndpointId, HostEpoch, OperationClass,
    OperationSpec, RegisteredDevice, Result, RetainedBytes,
};
use rsi_credentials_protocol::SecretValue;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Debug, Default)]
struct Devices {
    records: Mutex<Vec<DeviceRecord>>,
    registrations: AtomicUsize,
}
#[async_trait]
impl DeviceAdministration for Devices {
    async fn register(&self, label: &str) -> Result<RegisteredDevice> {
        self.registrations.fetch_add(1, Ordering::SeqCst);
        let record = DeviceRecord {
            id: DeviceId::from_bytes([7; 16]),
            label: label.into(),
        };
        self.records.lock().unwrap().push(record.clone());
        Ok(RegisteredDevice {
            record,
            token: SecretValue::new("a".repeat(64)).unwrap(),
        })
    }
    async fn revoke(&self, id: &DeviceId) -> Result<bool> {
        let mut records = self.records.lock().unwrap();
        let before = records.len();
        records.retain(|record| &record.id != id);
        Ok(records.len() != before)
    }
    fn list(&self) -> Result<Vec<DeviceRecord>> {
        Ok(self.records.lock().unwrap().clone())
    }
}

#[derive(Debug)]
struct Local {
    registry: Arc<ApiRegistry>,
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    lose_registration: AtomicBool,
}
#[async_trait]
impl ApiClient for Local {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        ByteBudget::new(64 * 1024).unwrap()
    }
    async fn call(&self, operation: &OperationSpec, input: RetainedBytes) -> Result<ApiOutput> {
        let response = self
            .registry
            .admit(&operation.id, CallOrigin::Local)?
            .invoke(input)
            .await?;
        if operation.id == rsi_api_protocol::OperationId::new("devices", "register", 1).unwrap()
            && self.lose_registration.load(Ordering::SeqCst)
        {
            drop(response);
            return Err(ApiError::OutcomeUnknown);
        }
        Ok(response)
    }
}

struct Harness {
    devices: Arc<Devices>,
    api: DeviceApi,
    local: Arc<Local>,
    client: DeviceClient,
}
impl Harness {
    fn new() -> Self {
        let registry = Arc::new(ApiRegistry::new(rsi_meta::Execution::native(
            tokio::runtime::Handle::current(),
        )));
        let devices = Arc::new(Devices::default());
        let api = DeviceApi::register(registry.as_ref(), devices.clone()).unwrap();
        let local = Arc::new(Local {
            operations: registry.operations(),
            registry,
            description: ConnectionDescription {
                wire_version: 1,
                endpoint_id: EndpointId::from_bytes([2; 16]),
                host_epoch: HostEpoch::from_bytes([3; 16]),
            },
            lose_registration: AtomicBool::new(false),
        });
        let client = DeviceClient::new(local.clone()).unwrap();
        Self {
            devices,
            api,
            local,
            client,
        }
    }
    async fn close(self) {
        self.api.close().await;
        self.local.registry.close().await;
    }
}

#[tokio::test]
async fn one_time_receipt_round_trips_and_revoke_is_explicitly_idempotent() {
    let harness = Harness::new();
    assert!(harness.client.list().await.unwrap().is_empty());
    let issued = harness.client.register("浏览器 / laptop").await.unwrap();
    assert_eq!(issued.token.expose_secret(), "a".repeat(64));
    assert!(!format!("{issued:?}").contains(issued.token.expose_secret()));
    assert_eq!(
        harness.client.list().await.unwrap(),
        vec![issued.record.clone()]
    );
    assert!(harness.client.revoke(&issued.record.id).await.unwrap());
    assert!(!harness.client.revoke(&issued.record.id).await.unwrap());
    assert!(harness.client.list().await.unwrap().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn maximum_roster_round_trips_json_escaped_labels_and_can_be_revoked() {
    let harness = Harness::new();
    let records: Vec<_> = (0..64)
        .map(|index| DeviceRecord {
            id: DeviceId::from_bytes([index; 16]),
            label: "\"\\".repeat(64),
        })
        .collect();
    for record in &records {
        DeviceRecord::validate_label(&record.label).unwrap();
    }
    let encoded = serde_json::to_vec(&Ok::<_, ()>(&records)).unwrap();
    assert_eq!(encoded.len(), 19_784);
    harness.devices.records.lock().unwrap().clone_from(&records);
    assert_eq!(harness.client.list().await.unwrap(), records);
    harness.devices.records.lock().unwrap().push(DeviceRecord {
        id: DeviceId::from_bytes([64; 16]),
        label: "one device too many".into(),
    });
    assert!(matches!(
        harness.client.list().await,
        Err(ApiError::Invalid(_))
    ));
    harness.devices.records.lock().unwrap().pop();
    assert!(harness.client.revoke(&records[0].id).await.unwrap());
    assert_eq!(harness.client.list().await.unwrap(), records[1..]);
    harness.close().await;
}

#[tokio::test]
async fn unknown_registration_is_not_replayed_and_can_be_listed_then_revoked() {
    let harness = Harness::new();
    harness
        .local
        .lose_registration
        .store(true, Ordering::SeqCst);
    assert!(matches!(
        harness.client.register("lost receipt").await,
        Err(ApiError::OutcomeUnknown)
    ));
    assert_eq!(harness.devices.registrations.load(Ordering::SeqCst), 1);
    let records = harness.client.list().await.unwrap();
    assert_eq!(records.len(), 1);
    assert!(harness.client.revoke(&records[0].id).await.unwrap());
    harness.close().await;
}

#[tokio::test]
async fn validates_requests_and_untrusted_rosters_and_fences_escaped_clients() {
    let harness = Harness::new();
    for label in [String::new(), "a\n".into(), "x".repeat(129)] {
        assert!(matches!(
            harness.client.register(&label).await,
            Err(ApiError::Invalid(_))
        ));
    }
    assert_eq!(harness.devices.registrations.load(Ordering::SeqCst), 0);
    let issued = harness.client.register("duplicate").await.unwrap();
    harness.devices.records.lock().unwrap().push(issued.record);
    assert!(matches!(
        harness.client.list().await,
        Err(ApiError::Invalid(_))
    ));
    harness.api.close().await;
    assert!(matches!(
        harness.client.list().await,
        Err(ApiError::Unavailable | ApiError::ShuttingDown)
    ));
    harness.local.registry.close().await;
}
