use super::{
    shared::{MultipartPart, OpenAiRequestBody, authorized_request, http_failure, multipart},
    *,
};

impl ImageAdapter for OpenAiImageAdapter {
    fn validate_request(&self, _model: &str, _request: &ImageRequest) -> Result<(), AiError> {
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // Generation and edit wire forms share one prepared effect.
    fn prepare(
        &self,
        context: PrepareContext,
        model: String,
        request: ImageRequest,
    ) -> AdapterFuture<Result<Prepared<ImageAdapterStream>, AiError>> {
        let snapshot = context.snapshot().clone();
        let config = self.config.clone();
        let transport = Arc::clone(&self.transport);
        Box::pin(async move {
            Ok(Prepared::new(snapshot, move |abort| {
                Box::pin(async move {
                    let (path, content_type, body) = if request.inputs().is_empty() {
                        (
                            "/v1/images/generations",
                            "application/json".to_owned(),
                            OpenAiRequestBody::Buffered(
                                serde_json::to_vec(&json!({
                                    "model":model, "prompt":request.prompt(), "n":request.count()
                                }))
                                .map_err(invalid_request_error)?,
                            ),
                        )
                    } else {
                        let mut parts: Vec<MultipartPart> = vec![
                            (
                                "model".to_owned(),
                                None,
                                None,
                                Arc::from(model.into_bytes()),
                            ),
                            (
                                "prompt".to_owned(),
                                None,
                                None,
                                Arc::from(request.prompt().as_bytes()),
                            ),
                            (
                                "n".to_owned(),
                                None,
                                None,
                                Arc::from(request.count().to_string().into_bytes()),
                            ),
                        ];
                        for (index, media) in request.inputs().iter().enumerate() {
                            let bytes = context.resolve_media(media, abort.clone()).await?;
                            parts.push((
                                "image[]".to_owned(),
                                Some(format!("image-{index}.bin")),
                                Some(media.mime_type().to_owned()),
                                bytes,
                            ));
                        }
                        if let Some(mask) = request.mask() {
                            let bytes = context.resolve_media(mask, abort.clone()).await?;
                            parts.push((
                                "mask".to_owned(),
                                Some("mask.bin".to_owned()),
                                Some(mask.mime_type().to_owned()),
                                bytes,
                            ));
                        }
                        let boundary =
                            format!("rsi-ai-{}", &context.snapshot().request_sha256[..24]);
                        let body = OpenAiRequestBody::Streaming(multipart(&boundary, parts)?);
                        (
                            "/v1/images/edits",
                            format!("multipart/form-data; boundary={boundary}"),
                            body,
                        )
                    };
                    let outgoing =
                        authorized_request(&context, config.url(path), &content_type, body)?;
                    let response = transport
                        .execute(outgoing, abort.cancellation_token())
                        .await
                        .map_err(transport_connect_error)?;
                    if !(200..300).contains(&response.status) {
                        return Err(http_failure(response.status, response.body).await);
                    }
                    Ok(openai_image_stream(response.body, request.count()))
                })
            }))
        })
    }
}

fn image_response_error(summary: impl Into<String>) -> AiError {
    ai_error(
        ErrorKind::Protocol,
        ErrorPhase::Assemble,
        DispatchStatus::Dispatched,
        summary.into(),
    )
}

fn image_output_validation_error(summary: &'static str) -> AiError {
    ai_error(
        ErrorKind::OutputValidation,
        ErrorPhase::Assemble,
        DispatchStatus::Dispatched,
        summary,
    )
}

#[derive(Debug, Deserialize)]
struct OpenAiImageData<'a> {
    #[serde(borrow)]
    b64_json: &'a str,
}

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

fn validate_png_prefix(encoded: &str) -> Result<(), AiError> {
    let Some(prefix) = encoded.as_bytes().get(..12) else {
        return Err(image_output_validation_error(
            "OpenAI Images item is missing the PNG signature",
        ));
    };
    let bytes = BASE64
        .decode(prefix)
        .map_err(|_| image_response_error("OpenAI Images item has invalid base64"))?;
    if !bytes.starts_with(PNG_SIGNATURE) {
        return Err(image_output_validation_error(
            "OpenAI Images item does not contain PNG bytes",
        ));
    }
    Ok(())
}

fn openai_image_stream(mut body: ByteStream, expected_items: u8) -> ImageAdapterStream {
    Box::pin(stream! {
        let limits = JsonExtractionLimits::new(
            MAX_JSON_BODY_BYTES,
            MAX_IMAGE_ENVELOPE_BYTES,
            MAX_IMAGE_ITEM_JSON_BYTES,
        ).expect("OpenAI image extraction limits are valid");
        let mut extractor = BoundedJsonExtractor::object_array("/data", limits)
            .expect("OpenAI image JSON pointer is valid");
        let mut index = 0_u32;
        while let Some(chunk) = body.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    yield Err(transport_body_error(error));
                    return;
                }
            };
            let mut offset = 0;
            while offset < chunk.len() {
                let progress = match extractor.push_bytes(&chunk[offset..]) {
                    Ok(progress) => progress,
                    Err(error) => {
                        yield Err(transport_json_response_error(error));
                        return;
                    }
                };
                offset += progress.consumed;
                let item = match progress.event {
                    Some(JsonExtractEvent::ArrayItem(item)) => item,
                    Some(JsonExtractEvent::TargetStarted | JsonExtractEvent::StringChunk(_)) | None => continue,
                };
                let Ok(item) = serde_json::from_slice::<OpenAiImageData<'_>>(&item) else {
                    yield Err(image_response_error("OpenAI Images returned malformed item JSON"));
                    return;
                };
                if let Err(error) = validate_png_prefix(item.b64_json) {
                    yield Err(error);
                    return;
                }
                yield Ok(ImageEvent::OutputStarted {
                    index,
                    mime_type: "image/png".to_owned(),
                });
                let mut sequence = 1_u32;
                let mut decoded_bytes = 0_u64;
                for encoded in item.b64_json.as_bytes().chunks(ENCODED_OUTPUT_CHUNK_BYTES) {
                    let Ok(bytes) = BASE64.decode(encoded) else {
                        yield Err(image_response_error("OpenAI Images item has invalid base64"));
                        return;
                    };
                    decoded_bytes = match decoded_bytes.checked_add(bytes.len() as u64) {
                        Some(total) if total <= rsi_ai_protocol::MAX_IMAGE_BYTES => total,
                        _ => {
                            yield Err(image_output_validation_error("OpenAI Images item exceeds its decoded byte bound"));
                            return;
                        }
                    };
                    if !bytes.is_empty() {
                        yield Ok(ImageEvent::OutputChunk { index, sequence, bytes });
                        sequence = sequence.saturating_add(1);
                    }
                }
                yield Ok(ImageEvent::OutputFinished { index });
                index = index.saturating_add(1);
            }
        }
        if let Err(error) = extractor.finish() {
            yield Err(transport_json_response_error(error));
            return;
        }
        if index != u32::from(expected_items) {
            yield Err(image_output_validation_error(
                "OpenAI Images output count differs from the request",
            ));
            return;
        }
        yield Ok(ImageEvent::Finished);
    })
}
