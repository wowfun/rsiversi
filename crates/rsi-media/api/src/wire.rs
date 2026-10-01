use rsi_api_protocol::{
    ApiError, OperationClass, OperationEffect, OperationId, OperationSpec, RequestEncoding,
};
use rsi_media_protocol::{
    MAXIMUM_IMAGE_DESCRIPTOR_BYTES, MediaError, MediaId, MediaRef, StoredMedia,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Operation {
    Import,
    Read,
}
impl Operation {
    pub const ALL: [Self; 2] = [Self::Import, Self::Read];
    pub fn spec(self) -> OperationSpec {
        let (name, effect, encoding, input, output) = match self {
            Self::Import => (
                "import",
                OperationEffect::Mutation,
                RequestEncoding::Binary,
                64 * 1024 * 1024,
                1024,
            ),
            Self::Read => (
                "read",
                OperationEffect::Read,
                RequestEncoding::Json,
                1024,
                usize::try_from(MAXIMUM_IMAGE_DESCRIPTOR_BYTES).expect("canonical image bound")
                    + 1024,
            ),
        };
        OperationSpec {
            access: rsi_api_protocol::OperationAccess::Authenticated,
            id: OperationId::new(
                "media",
                name,
                if matches!(self, Self::Import) { 2 } else { 1 },
            )
            .expect("constant operation"),
            class: OperationClass::Data,
            effect,
            encoding,
            maximum_request_bytes: input,
            maximum_response_bytes: output,
        }
    }
}

pub(crate) fn import_frame(
    source: &[u8],
    options: &rsi_media_protocol::ImageImportOptions,
) -> rsi_media_protocol::Result<Vec<u8>> {
    options.validate()?;
    if source.is_empty() || source.len() > 64 * 1024 * 1024 {
        return Err(MediaError::InvalidInput(
            "API Media source must contain 1 byte through 64 MiB".into(),
        ));
    }
    let header = serde_json::to_vec(options)
        .map_err(|_| MediaError::InvalidInput("invalid Media import options".into()))?;
    if source.len() + header.len() + 2 > 64 * 1024 * 1024 {
        return Err(MediaError::InvalidInput(
            "API Media import frame exceeds 64 MiB".into(),
        ));
    }
    let mut frame = Vec::with_capacity(2 + header.len() + source.len());
    frame.extend_from_slice(
        &u16::try_from(header.len())
            .expect("bounded import options")
            .to_be_bytes(),
    );
    frame.extend_from_slice(&header);
    frame.extend_from_slice(source);
    Ok(frame)
}

pub(crate) fn parse_import(
    frame: &bytes::Bytes,
) -> rsi_api_protocol::Result<(bytes::Bytes, rsi_media_protocol::ImageImportOptions)> {
    let invalid = || ApiError::Invalid("invalid Media import frame".into());
    let prefix: [u8; 2] = frame
        .get(..2)
        .ok_or_else(invalid)?
        .try_into()
        .map_err(|_| invalid())?;
    let length = usize::from(u16::from_be_bytes(prefix));
    if length == 0 || length > 1024 || frame.len() <= length + 2 || frame.len() > 64 * 1024 * 1024 {
        return Err(invalid());
    }
    let options: rsi_media_protocol::ImageImportOptions =
        serde_json::from_slice(&frame[2..length + 2]).map_err(|_| invalid())?;
    options.validate().map_err(|_| invalid())?;
    Ok((frame.slice(length + 2..), options))
}
#[derive(Deserialize, Serialize)]
#[serde(tag = "code", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Failure {
    Invalid,
    Capacity,
    Codec,
    NotFound { id: MediaId },
    Corrupt,
}
pub(crate) fn map_failure(error: MediaError) -> rsi_api_protocol::Result<Failure> {
    Ok(match error {
        MediaError::Api(error) => return Err(error),
        MediaError::Io(_) => return Err(ApiError::Backend("Media backend failed".into())),
        MediaError::InvalidInput(_) => Failure::Invalid,
        MediaError::AdmissionFull(_) => Failure::Capacity,
        MediaError::Codec(_) => Failure::Codec,
        MediaError::NotFound(id) => Failure::NotFound { id },
        MediaError::Corrupt(_) => Failure::Corrupt,
    })
}
impl Failure {
    pub fn into_error(self) -> MediaError {
        match self {
            Self::Invalid => MediaError::InvalidInput("remote Media rejected input".into()),
            Self::Capacity => MediaError::AdmissionFull("remote Media admission".into()),
            Self::Codec => MediaError::Codec("remote Media codec rejected source".into()),
            Self::NotFound { id } => MediaError::NotFound(id),
            Self::Corrupt => MediaError::Corrupt("remote Media object is corrupt".into()),
        }
    }
}
pub(crate) fn validate_body(
    reference: &MediaRef,
    stored: &StoredMedia,
) -> rsi_media_protocol::Result<()> {
    if stored.reference != *reference
        || stored.bytes.len() as u64 != reference.bytes
        || hex::encode(Sha256::digest(&stored.bytes)) != reference.id.as_str()
    {
        return Err(MediaError::Corrupt(
            "Media body differs from exact reference".into(),
        ));
    }
    Ok(())
}
