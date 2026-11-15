//! Current product executable identity; no historical reconstruction payload.
use super::{NativeAddonRecord, NativeAddonUpdateError, Result};
use serde::Serialize;
use std::sync::Arc;

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct Selection<T> {
    format: u32,
    linked: String,
    native: T,
}

pub(super) fn encode(native: &[NativeAddonRecord]) -> Result<Arc<[u8]>> {
    serde_json::to_vec(&Selection {
        format: 1,
        linked: linked()?.into(),
        native,
    })
    .map(Arc::from)
    .map_err(|_| NativeAddonUpdateError::Selection("code selection encoding"))
}

fn linked() -> Result<&'static str> {
    crate::integration_source::linked_identity().map_err(NativeAddonUpdateError::Selection)
}
