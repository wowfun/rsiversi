use async_trait::async_trait;
use rsi_ai_protocol::{MediaDescriptor, MediaKind};
use rsi_credentials_protocol::{CredentialRef, CredentialsAdminContract, SecretValue};
use rsi_media_protocol::{MediaBody, MediaRead, MediaReadContract};
use rsi_meta::{
    ActivationPlan, ConfigValue, PluginFactory, PreparedActivation, ResolvedFactory, Runtime,
    UpdateMode,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

pub fn linked(id: &str, factory: impl PluginFactory) -> ResolvedFactory {
    ResolvedFactory::linked(id, "test", UpdateMode::Replayable, Arc::new(factory))
}
pub fn config(service: &str) -> Value {
    json!({"service":service,"deployment":"native","provider_family":"fixture","protocol":"fixture-v1","endpoint_fingerprint":"local-fixture","credential":{"owner":"fixture","slot":"key"},"language":true,"image":true})
}
pub async fn base() -> (Runtime, Arc<AtomicUsize>) {
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(
            linked(
                "credentials",
                rsi_credentials_testkit::MemoryCredentialsFactory,
            ),
            Value::Null,
        )
        .await
        .unwrap();
    runtime
        .root()
        .lookup_local::<CredentialsAdminContract>()
        .unwrap()
        .set(
            &CredentialRef::new("fixture", "key").unwrap(),
            SecretValue::new("native-test-credential").unwrap(),
        )
        .await
        .unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    runtime
        .root()
        .apply(linked("media", Media(reads.clone())), Value::Null)
        .await
        .unwrap();
    runtime
        .root()
        .apply(
            linked("language", rsi_ai::LanguageRouterFactory),
            Value::Null,
        )
        .await
        .unwrap();
    runtime
        .root()
        .apply(
            linked("image", rsi_ai_image::ImageRouterFactory),
            Value::Null,
        )
        .await
        .unwrap();
    (runtime, reads)
}
pub fn descriptor() -> MediaDescriptor {
    MediaDescriptor::new(
        MediaKind::Image,
        "image/png",
        3,
        hex::encode(Sha256::digest(b"abc")),
    )
    .unwrap()
}
#[derive(Debug)]
struct Media(Arc<AtomicUsize>);
#[async_trait]
impl MediaRead for Media {
    async fn read_descriptor(
        &self,
        descriptor: &MediaDescriptor,
    ) -> rsi_media_protocol::Result<MediaBody> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(MediaBody {
            descriptor: descriptor.clone(),
            bytes: bytes::Bytes::from_static(b"abc"),
        })
    }
}
#[async_trait]
impl PluginFactory for Media {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        plan.context()
            .provide_local::<MediaReadContract>(Arc::new(Self(self.0.clone())))?;
        Ok(())
    }
}
