mod common;
use common::{base, config};
use futures_util::StreamExt as _;
use rsi_ai_portable::PortableProviderFactory;
use rsi_ai_protocol::{
    ImageAssembler, ImageCallContract, ImageRequest, LanguageAssembler, LanguageCallContract,
    LanguageRequest, Message, ModelRef,
};
use rsi_meta_native_loader::{CatalogOptions, NativeCatalog};
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[allow(clippy::too_many_lines)] // One real Loader lifetime, from artifact load through final resource release.
async fn real_native_language_image_and_withdrawal_use_normal_routers_and_release_loader() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let target = root.join("target/native-addon-fixture-test");
    assert!(
        std::process::Command::new(env!("CARGO"))
            .args(["build", "--locked", "--manifest-path"])
            .arg(root.join("fixtures/rsi/native-addon/Cargo.toml"))
            .arg("--target-dir")
            .arg(&target)
            .status()
            .unwrap()
            .success()
    );
    let artifact = target.join("debug").join(format!(
        "{}rsi_fixture_native_addon{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    let directory = tempfile::tempdir().unwrap();
    let catalog = NativeCatalog::new(CatalogOptions::new(directory.path())).unwrap();
    let (runtime, reads) = base().await;
    runtime
        .root()
        .apply(
            catalog.load(artifact).unwrap(),
            serde_json::json!({"label":"native-v1","tools":false,"ai":true}),
        )
        .await
        .unwrap();
    let bridge = runtime
        .root()
        .apply(
            common::linked("bridge", PortableProviderFactory),
            config("fixture.native.ai"),
        )
        .await
        .unwrap();
    assert_eq!(
        bridge.snapshot().state,
        rsi_meta::FiberState::Active,
        "{:?}",
        bridge.snapshot()
    );
    let language = runtime
        .root()
        .lookup_local::<LanguageCallContract>()
        .unwrap();
    let model = ModelRef::new("native", "native-text").unwrap();
    assert_eq!(
        language.describe(&model).unwrap().context_window_tokens(),
        8192
    );
    let unsupported = LanguageRequest::new(vec![
        Message::user(vec![rsi_ai_protocol::MessageContent::Image(
            common::descriptor(),
        )])
        .unwrap(),
    ])
    .unwrap();
    assert_eq!(
        language
            .prepare(model.clone(), unsupported)
            .await
            .unwrap_err()
            .kind(),
        rsi_ai_protocol::ErrorKind::Unsupported
    );
    let prepared = language
        .prepare(
            model.clone(),
            LanguageRequest::new(vec![Message::user_text("hello").unwrap()]).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(prepared.snapshot().protocol, "fixture-v1");
    assert_eq!(prepared.snapshot().transport, "portable");
    let mut stream = prepared.start(CancellationToken::new()).await.unwrap();
    let mut assembler = LanguageAssembler::default();
    while let Some(event) = stream.next().await {
        assembler.push(&event.unwrap()).unwrap();
    }
    let output = assembler.finish().unwrap();
    assert!(format!("{output:?}").contains("native-v1: native Language"));
    assert!(!format!("{output:?}").contains("native-test-credential"));
    drop(stream);
    for cancel in [true, false] {
        let prepared = language
            .prepare(
                model.clone(),
                LanguageRequest::new(vec![Message::user_text("native-cancel").unwrap()]).unwrap(),
            )
            .await
            .unwrap();
        let cancellation = CancellationToken::new();
        let mut stream = prepared.start(cancellation.clone()).await.unwrap();
        assert!(matches!(
            stream.next().await.unwrap().unwrap(),
            rsi_ai_protocol::LanguageEvent::ContentStarted { .. }
        ));
        if cancel {
            cancellation.cancel();
            assert_eq!(
                stream.next().await.unwrap().unwrap_err().kind(),
                rsi_ai_protocol::ErrorKind::Cancelled
            );
        }
        drop(stream);
        wait_for_native_prepare(&language).await;
    }
    let image = runtime.root().lookup_local::<ImageCallContract>().unwrap();
    let prepared = image
        .prepare(
            ModelRef::new("native", "native-image").unwrap(),
            ImageRequest::new("one", 1).unwrap(),
        )
        .await
        .unwrap();
    let mut stream = prepared.start(CancellationToken::new()).await.unwrap();
    let mut assembler = ImageAssembler::default();
    while let Some(event) = stream.next().await {
        assembler.push(&event.unwrap()).unwrap();
    }
    assert_eq!(
        assembler.finish().unwrap().images[0].bytes,
        b"native-image-body"
    );
    drop(stream);
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(bridge.dispose().await.is_clean());
    assert!(language.describe(&model).is_err());
    drop(language);
    drop(image);
    drop(bridge);
    assert!(runtime.shutdown().await.is_clean());
    drop(runtime);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while catalog.snapshot().staging_bytes != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    let resources = catalog.snapshot();
    assert_eq!(resources.active_instances, 0);
    assert_eq!(resources.host_capabilities, 0);
    assert_eq!(resources.host_outputs, 0);
    assert_eq!(resources.retained_failed_finalizations, 0);
}

async fn wait_for_native_prepare(calls: &std::sync::Arc<dyn rsi_ai_protocol::LanguageCall>) {
    // Meta cancellation terminates its driver before a foreign callback must
    // have exited. This keyless fixture probes only pure Prepare, without Start.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let result = calls
                .prepare(
                    ModelRef::new("native", "native-text").unwrap(),
                    LanguageRequest::new(vec![Message::user_text("probe").unwrap()]).unwrap(),
                )
                .await;
            match result {
                Ok(prepared) => {
                    drop(prepared);
                    break;
                }
                Err(error)
                    if error.kind() == rsi_ai_protocol::ErrorKind::Transport
                        && error.dispatch_status()
                            == rsi_ai_protocol::DispatchStatus::NotDispatched =>
                {
                    tokio::task::yield_now().await;
                }
                Err(error) => panic!("native callback did not become reusable: {error}"),
            }
        }
    })
    .await
    .unwrap();
}
