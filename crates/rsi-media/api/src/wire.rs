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
            id: OperationId::new("media", name, 1).expect("constant operation"),
            class: OperationClass::Data,
            effect,
            encoding,
            maximum_request_bytes: input,
            maximum_response_bytes: output,
        }
    }
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
