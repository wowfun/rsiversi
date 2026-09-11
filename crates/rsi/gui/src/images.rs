use super::GuiApplication;
use crate::application::{Result, error};
use bytes::Bytes;
use futures_util::future::BoxFuture;
use rsi_media_protocol::{MediaRef, StoredMedia};
use serde::Deserialize;
use std::sync::Arc;

pub(crate) const MAXIMUM_IMAGES: usize = 8;
pub(crate) const MAXIMUM_UPLOAD_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_PREVIEW_OBJECTS: usize = 8;
const MAXIMUM_PREVIEW_BYTES: usize = 32 * 1024 * 1024;

pub(crate) fn limits() -> serde_json::Value {
    serde_json::json!({"images":MAXIMUM_IMAGES,"upload_bytes":MAXIMUM_UPLOAD_BYTES,
        "preview_objects":MAXIMUM_PREVIEW_OBJECTS,"preview_bytes":MAXIMUM_PREVIEW_BYTES})
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Preview {
    Source { ticket: String },
}

impl GuiApplication {
    /// Imports one bounded image and returns its canonical reference to the captured document caller.
    /// The application owns admitted work even if this response waiter is dropped.
    ///
    /// # Panics
    /// Panics if an earlier application panic poisoned its state.
    pub fn import_image(
        self: &Arc<Self>,
        pane: crate::SurfaceId,
        generation: &str,
        source: Bytes,
    ) -> BoxFuture<'static, Result<MediaRef>> {
        let admitted = (|| {
            if source.is_empty() || source.len() > MAXIMUM_UPLOAD_BYTES {
                return Err("Image source must contain 1 byte to 16 MiB".into());
            }
            let media = self.media.clone().ok_or("Image import is unavailable")?;
            let work = self
                .image_work
                .clone()
                .try_acquire_owned()
                .map_err(|_| "An image operation is still in progress")?;
            let attached = self.pane(pane)?.attachment(generation)?;
            Ok((media, work, attached))
        })();
        let (media, work, attached) = match admitted {
            Ok(value) => value,
            Err(error) => return Box::pin(async { Err(error) }),
        };
        self.admit(true, Some(pane), move |_| async move {
            let (_work, _attached) = (work, attached);
            let reference = media.import_image(source).await.map_err(error)?;
            Ok(reference)
        })
    }

    pub(super) fn inspect_image(
        &self,
        pane: crate::SurfaceId,
        generation: &str,
        media: MediaRef,
    ) -> Result<()> {
        let pane_ref = self.pane(pane)?;
        let current = pane_ref.current.lock().expect("Web pane poisoned");
        current
            .as_ref()
            .filter(|attached| attached.generation.to_string() == generation)
            .ok_or("This pane changed")?;
        media.validate().map_err(error)?;
        let mut details = self.details.lock().expect("Web details poisoned");
        let ticket = details.begin()?.to_string();
        details.image = Some(crate::details::ImageDetail {
            pane,
            generation: generation.into(),
            ticket,
            media,
        });
        Ok(())
    }

    /// Reads an image through its current exact-source detail ticket.
    /// Byte leases stay with the returned immutable object until its consumer releases them.
    ///
    /// # Panics
    /// Panics if an earlier application panic poisoned its state.
    pub fn read_image(self: &Arc<Self>, source: &str) -> BoxFuture<'static, Result<StoredMedia>> {
        let admitted = (|| {
            if source.len() > 2048 {
                return Err("Image selection exceeds its limit".into());
            }
            let selection: Preview =
                serde_json::from_str(source).map_err(|_| "Invalid image selection")?;
            let media = self.media.clone().ok_or("Image preview is unavailable")?;
            let work = self
                .image_work
                .clone()
                .try_acquire_owned()
                .map_err(|_| "An image operation is still in progress")?;
            let (reference, stop) = match selection {
                Preview::Source { ticket } => {
                    let details = self.details.lock().expect("Web details poisoned");
                    let reference = details
                        .image
                        .as_ref()
                        .filter(|image| image.ticket == ticket)
                        .map(|image| image.media.clone())
                        .or_else(|| {
                            details
                                .source
                                .as_ref()
                                .filter(|source| source.ticket == ticket)
                                .and_then(crate::details::SourceDetail::media)
                        })
                        .ok_or("This exact source is not an available image")?;
                    (reference, details.stop.clone())
                }
            };
            Ok((media, work, reference, stop))
        })();
        let (media, work, reference, stop) = match admitted {
            Ok(value) => value,
            Err(error) => return Box::pin(async { Err(error) }),
        };
        self.admit(false, None, move |_| async move {
            let _work = work;
            tokio::select! { biased;
                () = stop.cancelled() => Err("Image preview closed".into()),
                result = media.read(&reference) => result.map_err(error),
            }
        })
    }
}
