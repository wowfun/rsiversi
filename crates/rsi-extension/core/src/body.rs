//! Bounded values carried by a business request; the adapter owns their lifetime.

use crate::{Error, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::future::Future;

/// Larger outbound values use a request-owned paged body.
pub const INLINE_BYTES: usize = 256 * 1024;
/// Maximum decoded bytes in one fragment, well below the frame ceiling.
pub const FRAGMENT_BYTES: usize = 512 * 1024;

/// A value or a descriptor for immutable bytes owned by the originating request.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Body {
    /// A small value travels in the original request.
    Inline {
        /// The business value.
        value: Value,
    },
    /// The recipient reads the complete value through the business callback.
    Paged {
        /// Opaque owner-issued request token.
        token: String,
        /// Exact UTF-8 JSON byte length.
        bytes: usize,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fragment {
    offset: usize,
    data: String,
}

/// Encodes an exact range of an already authorized immutable body.
///
/// # Errors
/// Rejects an offset outside the retained body.
pub fn fragment(bytes: &[u8], offset: usize) -> Result<Value> {
    if offset >= bytes.len() {
        return Err(Error::Protocol);
    }
    let end = offset.saturating_add(FRAGMENT_BYTES).min(bytes.len());
    serde_json::to_value(Fragment {
        offset,
        data: STANDARD.encode(&bytes[offset..end]),
    })
    .map_err(|_| Error::Protocol)
}

impl Body {
    /// Reassembles a bounded body while the caller holds its capacity permit.
    /// Authorization, callback identity and cancellation belong to `read`.
    /// The enclosing frame must bound inline values below the domain's maximum.
    ///
    /// # Errors
    /// Rejects invalid lengths, tokens, offsets, encoding or truncated fragments.
    pub async fn resolve<T, F, Fut>(self, maximum: usize, mut read: F) -> Result<T>
    where
        T: DeserializeOwned,
        F: FnMut(&str, usize) -> Fut,
        Fut: Future<Output = Result<Value>>,
    {
        let (token, bytes) = match self {
            // The enclosing frame already bounds inline JSON below each owning
            // domain's native ceiling; it need not be serialized again here.
            Self::Inline { value } => {
                return serde_json::from_value(value).map_err(|_| Error::Protocol);
            }
            Self::Paged { token, bytes } => (token, bytes),
        };
        if bytes <= INLINE_BYTES || bytes > maximum || token.is_empty() || token.len() > 256 {
            return Err(Error::Protocol);
        }
        // Base64's decoded size estimate includes at most two padding bytes.
        let mut result = Vec::with_capacity(bytes.checked_add(2).ok_or(Error::Protocol)?);
        while result.len() < bytes {
            let part: Fragment = serde_json::from_value(read(&token, result.len()).await?)
                .map_err(|_| Error::Protocol)?;
            let maximum = FRAGMENT_BYTES.min(bytes - result.len());
            if part.offset != result.len() || part.data.len() > maximum.div_ceil(3) * 4 {
                return Err(Error::Protocol);
            }
            let start = result.len();
            STANDARD
                .decode_vec(part.data, &mut result)
                .map_err(|_| Error::Protocol)?;
            let decoded = result.len() - start;
            if decoded == 0 || decoded > maximum {
                return Err(Error::Protocol);
            }
        }
        serde_json::from_slice(&result).map_err(|_| Error::Protocol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn unicode_and_escaped_bodies_round_trip_at_native_limits() {
        for value in [
            serde_json::json!("界🚀\n\t\"\\".repeat(50_000)),
            serde_json::json!("x".repeat(INLINE_BYTES - 2)),
        ] {
            let bytes = serde_json::to_vec(&value).unwrap();
            let body = if bytes.len() <= INLINE_BYTES {
                Body::Inline {
                    value: value.clone(),
                }
            } else {
                Body::Paged {
                    token: "a".repeat(32),
                    bytes: bytes.len(),
                }
            };
            let restored: Value = body
                .resolve(4 * 1024 * 1024, |_, offset| {
                    std::future::ready(fragment(&bytes, offset))
                })
                .await
                .unwrap();
            assert_eq!(restored, value);
        }
    }
    #[tokio::test]
    async fn invalid_descriptors_and_fragments_never_reach_json_admission() {
        for value in [
            serde_json::json!({"offset": 1, "data": "eA=="}),
            serde_json::json!({"offset": 0, "data": ""}),
            serde_json::json!({"offset": 0, "data": "!"}),
        ] {
            let body = Body::Paged {
                token: "a".repeat(32),
                bytes: INLINE_BYTES + 1,
            };
            assert_eq!(
                body.resolve::<Value, _, _>(INLINE_BYTES + 1, |_, _| std::future::ready(Ok(
                    value.clone()
                )))
                .await,
                Err(Error::Protocol)
            );
        }
        let body = Body::Paged {
            token: "a".repeat(32),
            bytes: INLINE_BYTES + 2,
        };
        assert_eq!(
            body.resolve::<Value, _, _>(INLINE_BYTES + 1, |_, _| async {
                panic!("oversize body must not read")
            })
            .await,
            Err(Error::Protocol)
        );
        assert_eq!(
            Body::Paged {
                token: "t".into(),
                bytes: usize::MAX
            }
            .resolve::<Value, _, _>(usize::MAX, |_, _| async {
                panic!("overflow must not read")
            })
            .await,
            Err(Error::Protocol)
        );
    }

    #[tokio::test]
    async fn typed_resolution_validates_both_inline_and_paged_values() {
        #[derive(Debug, Deserialize, PartialEq)]
        #[serde(deny_unknown_fields)]
        struct Input {
            text: String,
        }
        let input = serde_json::json!({"text":"界".repeat(INLINE_BYTES)});
        let bytes = serde_json::to_vec(&input).unwrap();
        let value: Input = Body::Paged {
            token: "t".into(),
            bytes: bytes.len(),
        }
        .resolve(bytes.len(), |_, offset| {
            std::future::ready(fragment(&bytes, offset))
        })
        .await
        .unwrap();
        assert_eq!(value.text, input["text"]);
        assert_eq!(
            Body::Inline {
                value: serde_json::json!({"text":42})
            }
            .resolve::<Input, _, _>(1024, |_, _| async { panic!("inline must not read") })
            .await,
            Err(Error::Protocol)
        );
    }
}
