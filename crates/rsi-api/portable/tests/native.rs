#![cfg(not(target_family = "wasm"))]
mod support;
use async_trait::async_trait;
use rsi_api_protocol::portable::{self, Header};
use rsi_meta::{
    ActivationPlan, Capability, CapabilityCall, ConfigValue, ContractVersion, LocalContract,
    Message, PluginFactory, PreparedActivation, Requirement,
};
use rsi_meta_native_loader::{CatalogOptions, NativeCatalog};
use std::{path::PathBuf, sync::Arc, time::Duration};
use support::{Fixture, apply, operation, until};

fn artifact() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap();
    let target = root.join("target/native-api-fixture-test");
    let output = std::process::Command::new(env!("CARGO"))
        .args(["build", "--locked", "--manifest-path"])
        .arg(root.join("fixtures/rsi/native-addon/Cargo.toml"))
        .arg("--target-dir")
        .arg(&target)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    target.join("debug").join(format!(
        "{}rsi_fixture_native_addon{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ))
}
#[derive(Debug)]
struct ProbeContract;
#[derive(Debug)]
struct ProbeGrant {
    probe: Capability,
    api: Capability,
}
impl LocalContract for ProbeContract {
    const KEY: &'static str = "fixture.api.probe";
    type Service = ProbeGrant;
}
#[derive(Debug)]
struct Capture;
#[async_trait]
impl PluginFactory for Capture {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone())
            .requiring(Requirement::new(
                "fixture.native.api-probe",
                "fixture.api-probe",
                ContractVersion(1),
            ))
            .requiring(Requirement::new(
                "fixture.api",
                portable::CONTRACT,
                ContractVersion(1),
            )))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<ProbeContract>(Arc::new(ProbeGrant {
                probe: plan.inject("fixture.native.api-probe").unwrap().clone(),
                api: plan.inject("fixture.api").unwrap().clone(),
            }))?;
        plan.defer(
            "probe caller",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
fn control(header: &Header) -> Vec<u8> {
    let mut bytes = vec![portable::HEADER_TAG];
    bytes.extend(serde_json::to_vec(header).unwrap());
    bytes
}
async fn request(
    probe: &Capability,
    grant: &Capability,
    name: &str,
    bytes: &[u8],
) -> CapabilityCall {
    let mut call = probe.open().unwrap();
    call.send(Message::from_parts(
        control(&Header::Call {
            operation: operation(name).id,
            bytes: bytes.len(),
        }),
        vec![grant.clone()],
    ))
    .await
    .unwrap();
    for (index, chunk) in bytes.chunks(portable::MAXIMUM_FRAGMENT_BYTES).enumerate() {
        let mut frame = vec![portable::FRAGMENT_TAG];
        frame.extend(
            u32::try_from(index * portable::MAXIMUM_FRAGMENT_BYTES)
                .unwrap()
                .to_le_bytes(),
        );
        frame.extend(chunk);
        call.send(Message::new(frame)).await.unwrap();
    }
    call.finish();
    call
}
async fn header(call: &mut CapabilityCall) -> Header {
    let message = call.recv().await.unwrap().unwrap();
    assert!(message.capabilities().is_empty());
    assert_eq!(message.as_bytes()[0], portable::HEADER_TAG);
    serde_json::from_slice(&message.as_bytes()[1..]).unwrap()
}
async fn payload(call: &mut CapabilityCall, total: usize) -> Vec<u8> {
    assert!(total <= 2 * 1024 * 1024);
    let mut bytes = Vec::with_capacity(total);
    while bytes.len() < total {
        let message = call.recv().await.unwrap().unwrap();
        assert!(message.capabilities().is_empty());
        let frame = message.as_bytes();
        assert_eq!(frame[0], portable::FRAGMENT_TAG);
        assert_eq!(
            usize::try_from(u32::from_le_bytes(frame[1..5].try_into().unwrap())).unwrap(),
            bytes.len()
        );
        bytes.extend(&frame[5..]);
    }
    assert_eq!(bytes.len(), total);
    bytes
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_sdk_transfers_api_authority_fragments_binary_and_drains_export_without_a_hard_edge() {
    let artifact = artifact();
    let fixture = Fixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let loader = NativeCatalog::new(CatalogOptions::new(directory.path())).unwrap();
    let native = fixture
        .runtime
        .root()
        .apply(
            loader.load(artifact).unwrap(),
            serde_json::json!({"label":"api","tools":false,"api_probe":true}),
        )
        .await
        .unwrap();
    assert_eq!(native.snapshot().state, rsi_meta::FiberState::Active);
    apply(
        &fixture.runtime.root(),
        "capture",
        Capture,
        ConfigValue::Null,
    )
    .await;
    let probe = fixture
        .runtime
        .root()
        .lookup_local::<ProbeContract>()
        .unwrap();
    let bytes: Vec<_> = (0..200_000)
        .map(|index| u8::try_from(index % 256).unwrap())
        .collect();
    let mut call = request(&probe.probe, &probe.api, "echo", &bytes).await;
    let Header::Reply { json, binary } = header(&mut call).await else {
        panic!("reply")
    };
    let reply = payload(&mut call, json + binary.unwrap()).await;
    assert_eq!(&reply[json..], bytes);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&reply[..json]).unwrap()["length"],
        200_000
    );
    assert!(call.recv().await.unwrap().is_none());
    drop(call);
    let mut call = request(&probe.probe, &probe.api, "domain", b"").await;
    let Header::Error { code, domain } = header(&mut call).await else {
        panic!("domain")
    };
    assert_eq!(code, portable::ErrorCode::Domain);
    let reply = payload(&mut call, domain.unwrap()).await;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&reply).unwrap()["rejected"]
            .as_str()
            .unwrap()
            .len(),
        20_000
    );
    assert!(call.recv().await.unwrap().is_none());
    drop(call);
    let mut call = request(&probe.probe, &probe.api, "stream", b"").await;
    assert!(matches!(header(&mut call).await, Header::Stream {}));
    let terminal = tokio::spawn(async move {
        loop {
            match call.recv().await {
                Ok(Some(_)) => {}
                terminal => break terminal,
            }
        }
    });
    assert!(
        tokio::time::timeout(Duration::from_secs(5), fixture.exporter.dispose())
            .await
            .unwrap()
            .is_clean()
    );
    // This native consumer received a transferred capability, with no API requirement edge.
    assert_eq!(native.snapshot().state, rsi_meta::FiberState::Active);
    let terminal = tokio::time::timeout(Duration::from_secs(5), terminal)
        .await
        .unwrap()
        .unwrap();
    assert!(terminal.is_err());
    drop(probe);
    fixture.close().await;
    until(|| loader.snapshot().staging_bytes == 0).await;
    let snapshot = loader.snapshot();
    assert_eq!(snapshot.active_instances, 0);
    assert_eq!(snapshot.active_callbacks, 0);
    assert_eq!(snapshot.host_capabilities, 0);
    assert_eq!(snapshot.host_outputs, 0);
    assert!(snapshot.peak_callbacks > 0);
}
