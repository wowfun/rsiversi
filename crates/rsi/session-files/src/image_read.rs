use crate::{SessionFilesError, server::Services, wire::Request};
use async_trait::async_trait;
use rsi_api_protocol::{
    ApiContext, ApiError, ApiHandler, ApiMessage, ApiOutput, ApiResponseCapacity, CallOrigin,
    RetainedBytes,
};
use rsi_files_protocol::{FilesError, MAXIMUM_FILE_BODY_BYTES, ReadPath};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(super) struct ImageHandler(pub(super) Services);

// The binary endpoint admits image content before returning any local bytes.
fn media_type(bytes: &[u8]) -> rsi_files_protocol::Result<&'static str> {
    use image::{ImageFormat, ImageReader};
    if let Ok(format) = image::guess_format(bytes) {
        let mime = match format {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Gif => "image/gif",
            ImageFormat::WebP => "image/webp",
            ImageFormat::Bmp => "image/bmp",
            ImageFormat::Ico => "image/x-icon",
            _ => return Err(FilesError::Invalid),
        };
        let (width, height) = ImageReader::with_format(std::io::Cursor::new(bytes), format)
            .into_dimensions()
            .map_err(|_| FilesError::Invalid)?;
        let pixels = u64::from(width) * u64::from(height);
        return (1..=16_777_216)
            .contains(&pixels)
            .then_some(mime)
            .ok_or(FilesError::Capacity);
    }
    if bytes.len() > 256 * 1024 {
        return Err(FilesError::Invalid);
    }
    use quick_xml::{NsReader, events::Event, name::ResolveResult};
    let mut reader = NsReader::from_reader(bytes);
    loop {
        match reader
            .read_resolved_event()
            .map_err(|_| FilesError::Invalid)?
        {
            (ResolveResult::Bound(ns), Event::Start(root) | Event::Empty(root))
                if root.local_name().as_ref() == "svg"
                    && ns.as_ref() == "http://www.w3.org/2000/svg" =>
            {
                return Ok("image/svg+xml");
            }
            (_, Event::Decl(_) | Event::Comment(_) | Event::PI(_)) => {}
            (_, Event::Text(text)) if text.as_ref().bytes().all(|b| b.is_ascii_whitespace()) => {}
            _ => return Err(FilesError::Invalid),
        }
    }
}

#[async_trait]
impl ApiHandler for ImageHandler {
    async fn invoke(
        &self,
        context: ApiContext,
        input: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        let request: Request<ReadPath> = serde_json::from_slice(input.as_bytes())
            .map_err(|_| ApiError::Invalid("invalid Session image request".into()))?;
        let mut capacity = output.into_finite()?;
        if !crate::is_local_image_path(request.input.as_bytes()) {
            return Err(ApiError::Domain(capacity.encode(&FilesError::Invalid)?));
        }
        let stop = context.retiring.child_token();
        let _cancel = stop.clone().drop_guard();
        let revoked = match &context.origin {
            CallOrigin::Device(device) => device.revoked.clone(),
            CallOrigin::Local => CancellationToken::new(),
        };
        let read = async {
            let (lease, execution, binding) =
                self.0.access(&context.origin, &request.target).await?;
            let files = execution.files().map_err(|_| FilesError::Cancelled)?;
            let fence = lease.fence();
            let scratch = tokio::select! {
                biased;
                () = fence.retiring().cancelled() => return Err(FilesError::Cancelled.into()),
                permit = self.0.scratch.clone().acquire_many_owned(MAXIMUM_FILE_BODY_BYTES as u32) => {
                    permit.map_err(|_| ApiError::ShuttingDown)?
                }
            };
            let work = tokio::spawn(async move {
                let _access = lease;
                let body = files
                    .read_body(binding, request.input, MAXIMUM_FILE_BODY_BYTES, stop)
                    .await?;
                let mime = media_type(&body)?;
                Ok::<_, FilesError>((mime, body, scratch))
            });
            tokio::select! {
                biased;
                () = fence.retiring().cancelled() => Err(FilesError::Cancelled.into()),
                result = work => Ok::<_, SessionFilesError>(result.map_err(|_| FilesError::Io)??),
            }
        };
        let result = tokio::select! {
            biased;
            () = revoked.cancelled() => return Err(ApiError::Unauthorized),
            () = context.retiring.cancelled() => return Err(ApiError::ShuttingDown),
            result = read => result,
        };
        match result {
            Ok((mime, body, _scratch)) => {
                // Transfer the already scratch-admitted allocation into the response pool.
                let binary = capacity.split(body.capacity())?.retain_vec(body)?;
                Ok(ApiOutput::Reply(ApiMessage {
                    json: capacity.encode(&mime)?,
                    binary: Some(binary),
                }))
            }
            Err(SessionFilesError::Files(error)) => Err(ApiError::Domain(capacity.encode(&error)?)),
            Err(SessionFilesError::Api(error)) => Err(error),
        }
    }
}
