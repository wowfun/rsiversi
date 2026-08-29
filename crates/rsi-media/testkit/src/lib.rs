//! Deterministic in-memory Media backend plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_media_protocol::{
    MediaBackend, MediaBackendContract, MediaError, MediaId, Result, StoredMedia,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
struct MemoryBackend {
    objects: Mutex<HashMap<MediaId, StoredMedia>>,
}

#[async_trait]
impl MediaBackend for MemoryBackend {
    async fn put(&self, media: StoredMedia) -> Result<()> {
        validate_stored_media(&media)?;
        let mut objects = self
            .objects
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = objects.get(&media.reference.id) {
            if existing.reference != media.reference || existing.bytes != media.bytes {
                return Err(MediaError::Corrupt(
                    "same MediaId was published with different content".into(),
                ));
            }
            return Ok(());
        }
        objects.insert(media.reference.id.clone(), media);
        Ok(())
    }

    async fn get(&self, id: &MediaId) -> Result<StoredMedia> {
        objects_get(&self.objects, id)
    }
}

fn objects_get(
    objects: &Mutex<HashMap<MediaId, StoredMedia>>,
    id: &MediaId,
) -> Result<StoredMedia> {
    let media = objects
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(id)
        .cloned()
        .ok_or_else(|| MediaError::NotFound(id.clone()))?;
    validate_stored_media(&media)?;
    Ok(media)
}

fn validate_stored_media(media: &StoredMedia) -> Result<()> {
    media.reference.validate()?;
    if u64::try_from(media.bytes.len()).ok() != Some(media.reference.bytes) {
        return Err(MediaError::Corrupt(
            "media bytes do not match their declared length".into(),
        ));
    }
    if hex::encode(Sha256::digest(&media.bytes)) != media.reference.id.as_str() {
        return Err(MediaError::Corrupt(
            "media bytes do not match their SHA-256 identity".into(),
        ));
    }
    Ok(())
}

/// Ordinary factory for one memory Media backend generation.
#[derive(Clone, Debug, Default)]
pub struct MemoryMediaBackendFactory;

#[async_trait]
impl PluginFactory for MemoryMediaBackendFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() && !desired.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err(MetaError::InvalidInput(
                "memory Media backend configuration must be null or empty".into(),
            ));
        }
        Ok(PreparedActivation::new(Value::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let backend: Arc<dyn MediaBackend> = Arc::new(MemoryBackend::default());
        let supply = plan
            .context()
            .provide_local::<MediaBackendContract>(backend)?;
        plan.defer(
            "withdraw memory Media backend",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
