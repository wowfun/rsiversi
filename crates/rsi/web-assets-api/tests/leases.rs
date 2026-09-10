use futures_util::{StreamExt as _, future::BoxFuture};
use rsi_api::ApiRegistry;
use rsi_api_http::HttpAssetsContract;
use rsi_api_protocol::{
    ApiDispatch as _, ApiError, ApiOutput, ApiStream, AuthenticatedDevice, CallOrigin, DeviceId,
};
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_web_assets::{WebAssetControl, WebAssetControlContract, WebAssetsFactory};
use rsi_web_assets_api::{Commit, Observe, Offer, WebAssetsApi};
use sha2::{Digest as _, Sha256};
use std::{path::Path, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

const APPLICATION: &str = "0123456789abcdef0123456789abcdef";
fn bundle(root: &Path, version: u8) -> Vec<String> {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("index.html"), b"static").unwrap();
    let body = vec![version; 32];
    for name in ["renderer.js", "lazy.js"] {
        std::fs::write(root.join(name), &body).unwrap();
    }
    let digest = format!("{:x}", Sha256::digest(body));
    std::fs::write(
        root.join("ui-renderers.json"),
        serde_json::to_vec(&serde_json::json!({
            "format":1,"renderers":[{"id":"fixture.renderer","abi":1,"entry":"renderer.js",
            "files":[{"name":"renderer.js","sha256":digest},{"name":"lazy.js","sha256":digest}],
            "schemas":[{"name":"fixture.model","version":1}],"capabilities":[],"surfaces":["pane"]}]
        }))
        .unwrap(),
    )
    .unwrap();
    ["index.html", "renderer.js", "lazy.js", "ui-renderers.json"]
        .map(Into::into)
        .to_vec()
}
fn device(id: u8) -> CallOrigin {
    CallOrigin::Device(AuthenticatedDevice {
        id: DeviceId::from_bytes([id; 16]),
        revoked: CancellationToken::new(),
    })
}
struct Fixture {
    root: tempfile::TempDir,
    runtime: Runtime,
    registry: ApiRegistry,
    api: WebAssetsApi,
    assets: Arc<WebAssetControl>,
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let files = bundle(&root.path().join("a"), b'a');
        let runtime = Runtime::default();
        let plugin = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "assets",
                    "1",
                    UpdateMode::RestartRequired,
                    Arc::new(WebAssetsFactory),
                ),
                serde_json::json!({"directory":root.path().join("a"),"files":files}),
            )
            .await
            .unwrap();
        assert_eq!(plugin.snapshot().state, rsi_meta::FiberState::Active);
        let assets = runtime
            .root()
            .lookup_local::<WebAssetControlContract>()
            .unwrap();
        let registry = ApiRegistry::new(runtime.execution().clone());
        let api =
            WebAssetsApi::register(&registry, runtime.execution().clone(), assets.clone()).unwrap();
        Self {
            root,
            runtime,
            registry,
            api,
            assets,
        }
    }
    fn call(
        &self,
        commit: bool,
        origin: CallOrigin,
        input: &impl serde::Serialize,
    ) -> BoxFuture<'static, rsi_api_protocol::Result<ApiOutput>> {
        let spec = &rsi_web_assets_api::operations()[usize::from(commit)];
        let invocation = self.registry.admit(&spec.id, origin).unwrap();
        let bytes = invocation
            .input_budget()
            .encode(input, spec.maximum_request_bytes)
            .unwrap();
        invocation.invoke(bytes)
    }
    async fn observe(&self, origin: CallOrigin, application: &str) -> ApiStream {
        let ApiOutput::Stream(stream) = self
            .call(
                false,
                origin,
                &Observe {
                    application: application.into(),
                },
            )
            .await
            .unwrap()
        else {
            panic!()
        };
        stream
    }
    async fn publish(&self, version: u8) -> String {
        let path = self.root.path().join(format!("{version}"));
        let files = bundle(&path, version);
        let expected = self.assets.revision().unwrap();
        self.assets
            .stage(path, files)
            .unwrap()
            .wait()
            .await
            .unwrap()
            .publish(&expected)
            .unwrap()
    }
    fn exists(&self, revision: &str) -> bool {
        self.runtime
            .root()
            .lookup_local::<HttpAssetsContract>()
            .unwrap()
            .get(&format!("/rsi-renderers/{revision}/lazy.js"))
            .unwrap()
            .is_some()
    }
    async fn commit(
        &self,
        origin: CallOrigin,
        revision: &str,
        accept: bool,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        self.call(
            true,
            origin,
            &Commit {
                application: APPLICATION.into(),
                revision: revision.into(),
                accept,
            },
        )
        .await
    }
    async fn close(self) {
        self.api.close().await;
        self.registry.close().await;
        assert!(self.runtime.shutdown().await.is_clean());
        assert_eq!(self.assets.retained_bytes(), 0);
    }
}
async fn next(stream: &mut ApiStream) -> Offer {
    let message = tokio::time::timeout(Duration::from_secs(3), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let offer: Offer = serde_json::from_slice(message.json.as_bytes()).unwrap();
    offer.validate().unwrap();
    offer
}
#[tokio::test]
async fn exact_commit_releases_old_lazy_imports_and_never_accepts_another_origin_or_duplicate() {
    let fixture = Fixture::new().await;
    let origin = device(1);
    let mut stream = fixture.observe(origin.clone(), APPLICATION).await;
    let a = next(&mut stream).await;
    fixture
        .commit(origin.clone(), &a.revision, true)
        .await
        .unwrap();
    let b = fixture.publish(b'b').await;
    let offer = next(&mut stream).await;
    assert_eq!(offer.revision, b);
    assert!(fixture.exists(&a.revision));
    assert!(fixture.exists(&b));
    assert!(fixture.commit(device(2), &b, true).await.is_err());
    assert!(
        fixture
            .commit(origin.clone(), &a.revision, true)
            .await
            .is_err()
    );
    assert!(fixture.exists(&a.revision));
    fixture.commit(origin.clone(), &b, true).await.unwrap();
    assert!(!fixture.exists(&a.revision));
    assert!(fixture.commit(origin, &b, true).await.is_err());
    drop(stream);
    fixture.close().await;
}
#[tokio::test]
async fn failed_mount_preserves_displayed_generation_until_observer_revocation() {
    let fixture = Fixture::new().await;
    let origin = device(3);
    let CallOrigin::Device(device) = &origin else {
        unreachable!()
    };
    let mut stream = fixture.observe(origin.clone(), APPLICATION).await;
    let a = next(&mut stream).await;
    fixture
        .commit(origin.clone(), &a.revision, true)
        .await
        .unwrap();
    fixture.publish(b'b').await;
    let b = next(&mut stream).await;
    fixture
        .commit(origin.clone(), &b.revision, false)
        .await
        .unwrap();
    assert!(fixture.exists(&a.revision));
    device.revoked.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .is_none_or(|result| result.is_err())
    );
    assert!(!fixture.exists(&a.revision));
    drop(stream);
    fixture.close().await;
}
#[tokio::test]
async fn invalid_and_duplicate_observers_do_not_consume_ownership_and_all_slots_are_bounded() {
    let fixture = Fixture::new().await;
    assert!(
        fixture
            .call(
                false,
                device(1),
                &Observe {
                    application: "invalid".into()
                }
            )
            .await
            .is_err()
    );
    let mut first = fixture.observe(device(0), APPLICATION).await;
    next(&mut first).await;
    assert!(
        matches!(
            fixture
                .call(
                    false,
                    device(0),
                    &Observe {
                        application: APPLICATION.into()
                    }
                )
                .await,
            Err(ApiError::Capacity)
        ),
        "duplicate must fail while 15 permits remain"
    );
    let mut streams = vec![first];
    for index in 1..16 {
        let mut stream = fixture.observe(device(index), APPLICATION).await;
        next(&mut stream).await;
        streams.push(stream);
    }
    assert!(matches!(
        fixture
            .call(
                false,
                device(20),
                &Observe {
                    application: APPLICATION.into()
                }
            )
            .await,
        Err(ApiError::Capacity)
    ));
    drop(streams);
    fixture.close().await;
}
#[tokio::test]
async fn publications_coalesce_while_the_first_offer_is_waiting_for_dom_commit() {
    let fixture = Fixture::new().await;
    let origin = device(4);
    let mut stream = fixture.observe(origin.clone(), APPLICATION).await;
    let a = next(&mut stream).await;
    let b = fixture.publish(b'b').await;
    assert!(
        tokio::time::timeout(Duration::from_millis(30), stream.next())
            .await
            .is_err()
    );
    fixture.commit(origin, &a.revision, true).await.unwrap();
    assert_eq!(next(&mut stream).await.revision, b);
    drop(stream);
    fixture.close().await;
}
