//! Finite raw process preview, independent of terminal decoding.
use crate::{Result, TurnError};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rsi_agent_session_protocol::{EffectId, SessionId, TurnId};
use serde::{Deserialize, Serialize};

/// Maximum encoded reply including all identities.
pub const MAXIMUM_JOB_PREVIEW_BYTES: usize = 60 * 1024;

/// One exact current-claim preview request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JobPreviewRequest {
    /// Current active Turn.
    pub turn_id: TurnId,
    /// Exact claim generation from Jobs status.
    pub generation: u64,
    /// Process-local job identity.
    pub job_id: String,
    /// Originating durable Tool effect, not model call ID.
    pub effect_id: EffectId,
    /// Newest stdout bytes requested, within 1..=32768.
    pub stdout_bytes: usize,
    /// Newest stderr bytes requested, within 1..=32768.
    pub stderr_bytes: usize,
}
impl JobPreviewRequest {
    /// Checks finite bounds before any lookup or allocation.
    pub fn validate(&self) -> Result<()> {
        rsi_jobs::validate_job_identifier("job identity", &self.job_id).map_err(invalid)?;
        if self.generation == 0
            || [self.stdout_bytes, self.stderr_bytes]
                .iter()
                .any(|n| !(1..=32 * 1024).contains(n))
            || self.stdout_bytes + self.stderr_bytes > 40 * 1024
        {
            return Err(invalid("invalid job preview bounds"));
        }
        Ok(())
    }
}

/// Exact newest raw bytes, encoded without JSON's per-byte numeric expansion.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JobPreviewStream {
    /// Canonical padded base64.
    pub base64: String,
    /// Whole-stream offset of the first returned byte.
    pub start: u64,
    /// Whole-stream offset after the last returned byte.
    pub end: u64,
    /// Whether the preview omits an earlier prefix.
    pub truncated: bool,
    /// Optional best-effort completed-output reference.
    pub full_output: Option<String>,
}
impl JobPreviewStream {
    /// Validates a raw producer tail before encoding it once.
    pub fn from_read(read: rsi_jobs::JobOutputRead, maximum: usize) -> Result<Self> {
        if !(1..=32 * 1024).contains(&maximum)
            || read.bytes.len() > maximum
            || read.oldest_offset.checked_add(read.bytes.len() as u64) != Some(read.next_offset)
            || read.lossy != (read.oldest_offset > 0)
            || read
                .full_output
                .as_ref()
                .is_some_and(|id| id.len() > 256 || id.chars().any(char::is_control))
        {
            return Err(invalid("invalid raw job preview stream"));
        }
        Ok(Self {
            base64: STANDARD.encode(read.bytes),
            start: read.oldest_offset,
            end: read.next_offset,
            truncated: read.lossy,
            full_output: read.full_output,
        })
    }
    /// Decodes and checks coordinates and exact canonical encoding.
    pub fn bytes(&self, maximum: usize) -> Result<Vec<u8>> {
        if !(1..=32 * 1024).contains(&maximum) {
            return Err(invalid("invalid preview decode bound"));
        }
        if self.base64.len() > 4 * maximum.div_ceil(3)
            || self
                .full_output
                .as_ref()
                .is_some_and(|id| id.len() > 256 || id.chars().any(char::is_control))
        {
            return Err(invalid("job preview stream exceeds bounds"));
        }
        let bytes = STANDARD.decode(&self.base64).map_err(invalid)?;
        if bytes.len() > maximum
            || self.start.checked_add(bytes.len() as u64) != Some(self.end)
            || self.truncated != (self.start > 0)
        {
            return Err(invalid("invalid job preview stream"));
        }
        Ok(bytes)
    }
}

/// One sampled process state; stream samples can occur at different instants.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JobPreview {
    /// Latest sampled status, without reporting.
    pub status: rsi_jobs::JobStatus,
    /// Newest raw stdout.
    pub stdout: JobPreviewStream,
    /// Newest raw stderr.
    pub stderr: JobPreviewStream,
}

/// Reply bound to the exact request and immutable Session handle.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JobPreviewPage {
    /// Exact Session owner.
    pub session_id: SessionId,
    /// Immutable Header fingerprint.
    pub header_sha256: String,
    /// Echoed request including claim generation and effect.
    pub request: JobPreviewRequest,
    /// `None` means live output is gone or the producer declines preview.
    pub preview: Option<JobPreview>,
}
impl JobPreviewPage {
    /// Validates untrusted responses before presentation or retention.
    pub fn validate_for(
        &self,
        session: &SessionId,
        header: &str,
        request: &JobPreviewRequest,
    ) -> Result<()> {
        request.validate()?;
        if &self.session_id != session
            || self.header_sha256 != header
            || &self.request != request
            || serde_json::to_vec(self).map_err(invalid)?.len() > MAXIMUM_JOB_PREVIEW_BYTES
        {
            return Err(invalid("job preview binding or encoded bound differs"));
        }
        if let Some(preview) = &self.preview {
            preview.stdout.bytes(request.stdout_bytes)?;
            preview.stderr.bytes(request.stderr_bytes)?;
        }
        Ok(())
    }
}
fn invalid(error: impl std::fmt::Display) -> TurnError {
    TurnError::Invalid(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_preview_admission_checks_coordinates_before_encoding_and_decode_is_canonical() {
        let read = || rsi_jobs::JobOutputRead {
            bytes: b"f".to_vec(),
            oldest_offset: 0,
            next_offset: 1,
            lossy: false,
            full_output: None,
        };
        let stream = JobPreviewStream::from_read(read(), 1).unwrap();
        assert_eq!(stream.bytes(1).unwrap(), b"f");
        for base64 in ["Zh==", "Zg", "Zg=", "Zg===", "Zg==\n"] {
            let mut invalid = stream.clone();
            invalid.base64 = base64.into();
            assert!(
                invalid.bytes(1).is_err(),
                "{base64:?} must not be accepted as canonical base64"
            );
        }
        for case in 0..5 {
            let mut invalid = read();
            match case {
                0 => invalid.next_offset = 2,
                1 => invalid.lossy = true,
                2 => invalid.full_output = Some("bad\nreference".into()),
                3 => invalid.full_output = Some("x".repeat(257)),
                _ => invalid.bytes.push(0),
            }
            assert!(JobPreviewStream::from_read(invalid, 1).is_err());
        }
        assert!(JobPreviewStream::from_read(read(), 0).is_err());
    }

    #[test]
    fn binary_pages_preserve_offsets_and_reject_expansion_rebinding_and_invalid_base64() {
        let request = JobPreviewRequest {
            turn_id: TurnId::new("turn").unwrap(),
            generation: 1,
            job_id: "job-1".into(),
            effect_id: EffectId::new("effect").unwrap(),
            stdout_bytes: 32 * 1024,
            stderr_bytes: 8 * 1024,
        };
        let stream = |bytes: usize| {
            JobPreviewStream::from_read(
                rsi_jobs::JobOutputRead {
                    bytes: vec![255; bytes],
                    oldest_offset: 1,
                    next_offset: bytes as u64 + 1,
                    lossy: true,
                    full_output: Some("a".repeat(32)),
                },
                32 * 1024,
            )
            .unwrap()
        };
        let page = JobPreviewPage {
            session_id: SessionId::new("session").unwrap(),
            header_sha256: "a".repeat(64),
            request: request.clone(),
            preview: Some(JobPreview {
                status: rsi_jobs::JobStatus::Running,
                stdout: stream(request.stdout_bytes),
                stderr: stream(request.stderr_bytes),
            }),
        };
        page.validate_for(&page.session_id, &page.header_sha256, &request)
            .unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() < 60 * 1024);
        let mut rebound = request.clone();
        rebound.generation = 2;
        assert!(
            page.validate_for(&page.session_id, &page.header_sha256, &rebound)
                .is_err()
        );
        let mut excessive = request.clone();
        excessive.stderr_bytes = 32 * 1024;
        assert!(excessive.validate().is_err());
        let mut invalid = stream(3);
        invalid.base64 = "////\n".into();
        assert!(invalid.bytes(3).is_err());
        invalid = stream(3);
        invalid.end += 1;
        assert!(invalid.bytes(3).is_err());
        assert!(stream(4).bytes(3).is_err());
        assert!(stream(3).bytes(usize::MAX).is_err());
    }
}
