use super::*;

pub(super) fn authorized_json_request(
    context: &PrepareContext,
    url: String,
    body: JsonRequestBody,
) -> Result<HttpRequest, AiError> {
    let request = authorized_control_request(context, Method::POST, url)?
        .header(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )
        .map_err(invalid_request_error)?;
    Ok(request.json_body(body))
}

pub(super) fn authorized_control_request(
    context: &PrepareContext,
    method: Method,
    url: String,
) -> Result<HttpRequest, AiError> {
    let credential = context.credential().ok_or_else(|| {
        ai_error(
            ErrorKind::Authentication,
            ErrorPhase::Send,
            DispatchStatus::NotDispatched,
            "OpenAI credential is unavailable",
        )
    })?;
    HttpRequest::new(method, url)
        .map_err(invalid_request_error)?
        .bearer_auth(&credential.secret)
        .map_err(invalid_request_error)
}

pub(super) enum OpenAiRequestBody {
    Buffered(Vec<u8>),
    Streaming(ByteStream),
}

pub(super) fn authorized_request(
    context: &PrepareContext,
    url: String,
    content_type: &str,
    body: OpenAiRequestBody,
) -> Result<HttpRequest, AiError> {
    let request = authorized_control_request(context, Method::POST, url)?
        .header(
            http::header::CONTENT_TYPE,
            HeaderValue::from_str(content_type).map_err(|_| {
                ai_error(
                    ErrorKind::InvalidRequest,
                    ErrorPhase::Prepare,
                    DispatchStatus::NotStarted,
                    "invalid content type",
                )
            })?,
        )
        .map_err(invalid_request_error)?;
    Ok(match body {
        OpenAiRequestBody::Buffered(body) => request.body(body),
        OpenAiRequestBody::Streaming(body) => request.body_stream(body),
    })
}

pub(super) type MultipartPart = (String, Option<String>, Option<String>, Arc<[u8]>);

pub(super) fn multipart(boundary: &str, parts: Vec<MultipartPart>) -> Result<ByteStream, AiError> {
    multipart_with_limit(boundary, parts, MAX_PROVIDER_REQUEST_BODY_BYTES)
}

fn multipart_with_limit(
    boundary: &str,
    parts: Vec<MultipartPart>,
    maximum_body_bytes: usize,
) -> Result<ByteStream, AiError> {
    let mut encoded = Vec::with_capacity(parts.len());
    for (name, filename, mime, body) in parts {
        let mut header = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"");
        if let Some(filename) = filename {
            write!(header, "; filename=\"{filename}\"").expect("writing to String cannot fail");
        }
        header.push_str("\r\n");
        if let Some(mime) = mime {
            write!(header, "Content-Type: {mime}\r\n").expect("writing to String cannot fail");
        }
        header.push_str("\r\n");
        encoded.push((Bytes::from(header), body));
    }
    let closing = Bytes::from(format!("--{boundary}--\r\n"));
    let projected = encoded
        .iter()
        .try_fold(closing.len(), |total, (header, body)| {
            total
                .checked_add(header.len())?
                .checked_add(body.len())?
                .checked_add(2)
        });
    if projected.is_none_or(|projected| projected > maximum_body_bytes) {
        return Err(ai_error(
            ErrorKind::InvalidRequest,
            ErrorPhase::Send,
            DispatchStatus::NotDispatched,
            format!("multipart request body exceeds the {maximum_body_bytes}-byte transport limit"),
        ));
    }
    Ok(Box::pin(stream! {
        for (header, body) in encoded {
            yield Ok(header);
            yield Ok(Bytes::from_owner(body));
            yield Ok(Bytes::from_static(b"\r\n"));
        }
        yield Ok(closing);
    }))
}

pub(super) async fn http_failure(status: u16, body: ByteStream) -> AiError {
    http_failure_at(status, body, ErrorPhase::FirstEvent).await
}

pub(super) async fn http_failure_at(status: u16, body: ByteStream, phase: ErrorPhase) -> AiError {
    let error = provider_http_error(status, body, phase, "OpenAI rejected the request").await;
    reclassify_context_limit(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_projection_counts_framing_before_stream_construction() {
        let Err(error) = multipart_with_limit(
            "boundary",
            vec![("field".into(), None, None, Arc::from([]))],
            1,
        ) else {
            panic!("multipart framing alone exceeds the test limit");
        };

        assert_eq!(error.kind(), ErrorKind::InvalidRequest);
        assert_eq!(error.phase(), ErrorPhase::Send);
        assert_eq!(error.dispatch_status(), DispatchStatus::NotDispatched);
    }
}
