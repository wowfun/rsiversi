use super::WebApplication;
use crate::application::{Result, error};
use bytes::Bytes;
use futures_util::future::BoxFuture;
use rsi_media_protocol::{MediaRef, StoredMedia};
use rsi_session_protocol::SessionInput;
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

#[derive(Debug, Default)]
pub(super) struct DraftInput {
    pub text: String,
    pub images: Vec<MediaRef>,
    pub revision: u64,
}
impl DraftInput {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.images.is_empty()
    }
    pub fn content(&self) -> Vec<SessionInput> {
        let mut content = Vec::with_capacity(self.images.len() + 1);
        if !self.text.is_empty() {
            content.push(SessionInput::Text {
                text: self.text.clone(),
            });
        }
        content.extend(
            self.images
                .iter()
                .cloned()
                .map(|media| SessionInput::Image { media }),
        );
        content
    }
    pub fn change_images(&mut self) -> Result<()> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("Image draft revision exhausted")?;
        Ok(())
    }
    pub fn clear_submitted(&mut self, content: &[SessionInput]) -> Result<()> {
        if content == self.content() {
            self.change_images()?;
            self.text.clear();
            self.images.clear();
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Preview {
    Draft {
        pane: u8,
        generation: String,
        index: u8,
        media: MediaRef,
    },
    Source {
        ticket: String,
    },
}

impl WebApplication {
    /// Imports one bounded image and attaches its canonical reference to the captured draft.
    /// The application owns admitted work even if this response waiter is dropped.
    ///
    /// # Panics
    /// Panics if an earlier application panic poisoned its state.
    pub fn import_image(
        self: &Arc<Self>,
        pane: u8,
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
            let submission = attached
                .draft
                .submissions
                .clone()
                .try_acquire_owned()
                .map_err(|_| "A submission is still awaiting its receipt")?;
            if attached
                .draft
                .input
                .lock()
                .expect("Web draft poisoned")
                .images
                .len()
                >= MAXIMUM_IMAGES
            {
                return Err("A draft can hold at most eight images".into());
            }
            Ok((media, work, attached, submission))
        })();
        let (media, work, attached, submission) = match admitted {
            Ok(value) => value,
            Err(error) => return Box::pin(async { Err(error) }),
        };
        self.admit(true, Some(pane), move |_| async move {
            let (_work, _submission) = (work, submission);
            let reference = media.import_image(source).await.map_err(error)?;
            let mut input = attached.draft.input.lock().expect("Web draft poisoned");
            input.change_images()?;
            input.images.push(reference.clone());
            Ok(reference)
        })
    }

    pub(super) fn edit_image(
        &self,
        pane: u8,
        generation: &str,
        revision: &str,
        from: u8,
        to: Option<u8>,
    ) -> Result<()> {
        let attached = self.pane(pane)?.attachment(generation)?;
        let mut input = attached.draft.input.lock().expect("Web draft poisoned");
        if input.revision.to_string() != revision {
            return Err("The image order changed; retry the current action".into());
        }
        let from = usize::from(from);
        let to = to.map(usize::from);
        if from >= input.images.len() || to.is_some_and(|to| to >= input.images.len()) {
            return Err("This image position is unavailable".into());
        }
        input.change_images()?;
        let image = input.images.remove(from);
        if let Some(to) = to {
            input.images.insert(to, image);
        }
        Ok(())
    }

    pub(super) fn inspect_image(
        &self,
        pane: u8,
        generation: &str,
        index: u8,
        media: MediaRef,
    ) -> Result<()> {
        let pane_ref = self.pane(pane)?;
        let current = pane_ref.current.lock().expect("Web pane poisoned");
        let attached = current
            .as_ref()
            .filter(|attached| attached.generation.to_string() == generation)
            .ok_or("This pane changed")?;
        if attached
            .draft
            .input
            .lock()
            .expect("Web draft poisoned")
            .images
            .get(usize::from(index))
            != Some(&media)
        {
            return Err("This draft image changed; select it again".into());
        }
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

    /// Reads an exact current draft or source-detail image selected by a closed JSON request.
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
                Preview::Draft {
                    pane,
                    generation,
                    index,
                    media,
                } => {
                    let attached = self.pane(pane)?.attachment(&generation)?;
                    if attached
                        .draft
                        .input
                        .lock()
                        .expect("Web draft poisoned")
                        .images
                        .get(usize::from(index))
                        != Some(&media)
                    {
                        return Err("This draft image changed; select it again".into());
                    }
                    (media, tokio_util::sync::CancellationToken::new())
                }
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
