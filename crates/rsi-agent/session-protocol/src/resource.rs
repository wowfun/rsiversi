//! Finite resource requests and results; addresses are opaque within a contribution.

use crate::{ContributionId, Result, SessionError, SessionId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Maximum encoded resource response, including escaping and metadata.
pub const MAXIMUM_SESSION_RESOURCE_BYTES: usize = 2 * 1024 * 1024;
/// Maximum unencoded text in one resource.
pub const MAXIMUM_RESOURCE_TEXT_BYTES: usize = 256 * 1024;
/// Maximum entries returned by one finite catalog read.
pub const MAXIMUM_RESOURCE_ENTRIES: usize = 256;

/// One explicitly selected finite read. It grants no execution authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[allow(missing_docs)]
pub enum SessionResourceRequest {
    Sources,
    List { source: ContributionId },
    Read { source: ContributionId, id: String },
}
/// Immutable request admitted at the Session service or wire boundary.
#[derive(Clone, Debug)]
pub struct ValidatedResourceRequest(SessionResourceRequest);
impl ValidatedResourceRequest {
    /// Transfers the validated coordinates to the resource driver.
    pub fn into_request(self) -> SessionResourceRequest {
        self.0
    }
}
impl SessionResourceRequest {
    /// Validates external coordinates once before entering the Local resource driver.
    pub fn validated(self) -> Result<ValidatedResourceRequest> {
        self.validate()?;
        Ok(ValidatedResourceRequest(self))
    }

    /// Validates external resource coordinates before work is admitted.
    pub fn validate(&self) -> Result<()> {
        if let Self::Read { id, .. } = self {
            text(id, 4096, false)?;
        }
        Ok(())
    }
}

/// Human-readable metadata from the owning resource provider.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionResourceDescriptor {
    /// Opaque provider-local coordinate, never ambient filesystem authority.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Short description.
    pub description: String,
    /// Display-only source label.
    pub source: String,
    /// MIME type of the text representation.
    pub media_type: String,
    /// Whether the provider separately allows model reads.
    pub model_readable: bool,
}
impl SessionResourceDescriptor {
    /// Checks metadata before it is retained or sent to an application.
    pub fn validate(&self) -> Result<()> {
        text(&self.id, 4096, false)?;
        text(&self.name, 256, false)?;
        text(&self.description, 4096, true)?;
        text(&self.source, 16 * 1024, true)?;
        text(&self.media_type, 128, false)
    }
}

/// Complete, bounded result of one resource operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[allow(missing_docs)]
pub enum SessionResourceValue {
    Sources {
        sources: Vec<ContributionId>,
    },
    List {
        entries: Vec<SessionResourceDescriptor>,
    },
    Read {
        resource: SessionResourceDescriptor,
        text: String,
    },
}
impl SessionResourceValue {
    /// Validates counts, identities, text and the complete encoded result.
    pub fn validate(&self) -> Result<()> {
        self.validate_fields()?;
        bounded(self).map(|_| ())
    }
    fn validate_fields(&self) -> Result<()> {
        match self {
            Self::Sources { sources } => {
                if sources.len() > 64
                    || sources.iter().collect::<BTreeSet<_>>().len() != sources.len()
                {
                    return Err(SessionError::Invalid("invalid resource sources".into()));
                }
            }
            Self::List { entries } => {
                if entries.len() > MAXIMUM_RESOURCE_ENTRIES {
                    return Err(SessionError::Invalid(
                        "resource catalog exceeds limit".into(),
                    ));
                }
                let mut ids = BTreeSet::new();
                for entry in entries {
                    entry.validate()?;
                    if !ids.insert(&entry.id) {
                        return Err(SessionError::Invalid("duplicate resource identity".into()));
                    }
                }
            }
            Self::Read {
                resource,
                text: body,
            } => {
                resource.validate()?;
                text(body, MAXIMUM_RESOURCE_TEXT_BYTES, true)?;
            }
        }
        Ok(())
    }
}

/// Result bound to the actual selected Header and composition source.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionResourceResponse {
    /// Actual Session identity.
    pub session_id: SessionId,
    /// Exact Header fingerprint.
    pub header_sha256: String,
    /// Source identity, not a process-local generation capability.
    pub composition_sha256: String,
    /// Exact request answered by this result.
    pub request: SessionResourceRequest,
    /// Validated provider result.
    pub value: SessionResourceValue,
}
/// Immutable response and its once-measured canonical JSON size.
#[derive(Clone, Debug)]
pub struct ValidatedResourceResponse {
    response: SessionResourceResponse,
    encoded_bytes: usize,
}
impl ValidatedResourceResponse {
    /// Borrows the admitted response without mutation authority.
    pub fn response(&self) -> &SessionResourceResponse {
        &self.response
    }
    /// Transfers the value and its exact encoded size to a retention owner.
    pub fn into_parts(self) -> (SessionResourceResponse, usize) {
        (self.response, self.encoded_bytes)
    }
}
impl SessionResourceResponse {
    /// Validates and measures external data before trusted in-process use.
    pub fn validated(self) -> Result<ValidatedResourceResponse> {
        self.validate_fields()?;
        let encoded_bytes = bounded(&self)?;
        Ok(ValidatedResourceResponse {
            response: self,
            encoded_bytes,
        })
    }

    /// Verifies response shape, correlation and complete wire size.
    pub fn validate(&self) -> Result<()> {
        self.validate_fields()?;
        bounded(self).map(|_| ())
    }
    fn validate_fields(&self) -> Result<()> {
        self.request.validate()?;
        for digest in [&self.header_sha256, &self.composition_sha256] {
            if digest.len() != 64
                || !digest
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            {
                return Err(SessionError::Invalid(
                    "invalid resource binding digest".into(),
                ));
            }
        }
        self.value.validate_fields()?;
        match (&self.request, &self.value) {
            (SessionResourceRequest::Sources, SessionResourceValue::Sources { .. })
            | (SessionResourceRequest::List { .. }, SessionResourceValue::List { .. }) => {}
            (
                SessionResourceRequest::Read { id, .. },
                SessionResourceValue::Read { resource, .. },
            ) if id == &resource.id => {}
            _ => {
                return Err(SessionError::Invalid(
                    "resource response does not answer request".into(),
                ));
            }
        }
        Ok(())
    }
}

fn text(value: &str, maximum: usize, empty: bool) -> Result<()> {
    if (!empty && value.is_empty()) || value.len() > maximum || value.contains(['\0', '\u{7f}']) {
        return Err(SessionError::Invalid("invalid resource text".into()));
    }
    Ok(())
}

fn bounded(value: &impl Serialize) -> Result<usize> {
    let bytes = crate::compact_json_len(value)?;
    if bytes > MAXIMUM_SESSION_RESOURCE_BYTES {
        return Err(SessionError::Invalid(
            "resource response exceeds byte limit".into(),
        ));
    }
    Ok(bytes)
}
