use super::{ProfileCandidate, ProfileError, ProfileLimits, Result, WatcherHealth};
use crate::native_source::read_file_bounded;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct WatchPlan {
    pub(super) fingerprints: BTreeMap<PathBuf, [u8; 32]>,
    stamps: BTreeMap<PathBuf, SourceStamp>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceStamp {
    length: u64,
    modified: Option<SystemTime>,
}

pub(super) enum WatchProbe {
    MetadataUnchanged,
    ContentVerified(WatchPlan),
}

impl WatchPlan {
    fn capture(paths: &[PathBuf], limits: &ProfileLimits) -> Result<Self> {
        let mut fingerprints = BTreeMap::new();
        let mut stamps = BTreeMap::new();
        let mut total = 0_usize;
        for path in paths {
            let bytes =
                read_file_bounded(path, limits.maximum_document_bytes).map_err(|error| {
                    if error.kind() == std::io::ErrorKind::InvalidData {
                        ProfileError::CapacityExceeded {
                            resource: "document bytes",
                            maximum: limits.maximum_document_bytes,
                        }
                    } else {
                        ProfileError::Source {
                            message: "cannot read a required watched source".to_owned(),
                        }
                    }
                })?;
            total = total
                .checked_add(bytes.len())
                .ok_or(ProfileError::CapacityExceeded {
                    resource: "source bytes",
                    maximum: limits.maximum_source_bytes,
                })?;
            if total > limits.maximum_source_bytes {
                return Err(ProfileError::CapacityExceeded {
                    resource: "source bytes",
                    maximum: limits.maximum_source_bytes,
                });
            }
            let digest: [u8; 32] = Sha256::digest(&bytes).into();
            fingerprints.insert(path.clone(), digest);
            stamps.insert(path.clone(), source_stamp(path, limits)?);
        }
        Ok(Self {
            fingerprints,
            stamps,
        })
    }

    pub(super) fn probe(
        baseline: &Self,
        limits: &ProfileLimits,
        force_content_audit: bool,
    ) -> Result<WatchProbe> {
        let paths = baseline.fingerprints.keys().cloned().collect::<Vec<_>>();
        let stamps = capture_stamps(&paths, limits)?;
        if !force_content_audit && stamps == baseline.stamps {
            return Ok(WatchProbe::MetadataUnchanged);
        }
        Self::capture(&paths, limits).map(WatchProbe::ContentVerified)
    }

    pub(super) fn health(&self) -> WatcherHealth {
        if self.fingerprints.is_empty() {
            WatcherHealth::Inactive
        } else {
            WatcherHealth::Healthy
        }
    }

    pub(super) fn establish(candidate: &ProfileCandidate, limits: &ProfileLimits) -> Result<Self> {
        let plan = Self::capture(candidate.watch_paths(), limits)?;
        if plan.fingerprints != candidate.source_fingerprints {
            return Err(ProfileError::Source {
                message: "a required source changed after Profile compilation".to_owned(),
            });
        }
        Ok(plan)
    }
}

fn source_stamp(path: &PathBuf, limits: &ProfileLimits) -> Result<SourceStamp> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ProfileError::Source {
        message: "cannot read a required watched source".to_owned(),
    })?;
    if !metadata.file_type().is_file() {
        return Err(ProfileError::Source {
            message: "a required watched source is not a regular file".to_owned(),
        });
    }
    let length = metadata.len();
    if length > limits.maximum_document_bytes as u64 {
        return Err(ProfileError::CapacityExceeded {
            resource: "document bytes",
            maximum: limits.maximum_document_bytes,
        });
    }
    Ok(SourceStamp {
        length,
        modified: metadata.modified().ok(),
    })
}

fn capture_stamps(
    paths: &[PathBuf],
    limits: &ProfileLimits,
) -> Result<BTreeMap<PathBuf, SourceStamp>> {
    let mut stamps = BTreeMap::new();
    let mut total = 0_usize;
    for path in paths {
        let stamp = source_stamp(path, limits)?;
        let length = usize::try_from(stamp.length).map_err(|_| ProfileError::CapacityExceeded {
            resource: "source bytes",
            maximum: limits.maximum_source_bytes,
        })?;
        total = total
            .checked_add(length)
            .ok_or(ProfileError::CapacityExceeded {
                resource: "source bytes",
                maximum: limits.maximum_source_bytes,
            })?;
        if total > limits.maximum_source_bytes {
            return Err(ProfileError::CapacityExceeded {
                resource: "source bytes",
                maximum: limits.maximum_source_bytes,
            });
        }
        stamps.insert(path.clone(), stamp);
    }
    Ok(stamps)
}

#[cfg(test)]
mod watch_tests {
    use super::*;

    #[test]
    fn metadata_fast_path_and_forced_content_audit_are_distinct() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("profile.toml");
        fs::write(&path, b"format = 1\n").unwrap();
        let limits = ProfileLimits::default();
        let baseline = WatchPlan::capture(std::slice::from_ref(&path), &limits).unwrap();

        assert!(matches!(
            WatchPlan::probe(&baseline, &limits, false).unwrap(),
            WatchProbe::MetadataUnchanged
        ));
        fs::write(&path, b"format = 2\n").unwrap();
        let WatchProbe::ContentVerified(changed) =
            WatchPlan::probe(&baseline, &limits, true).unwrap()
        else {
            panic!("forced audit must hash content");
        };
        assert_ne!(changed.fingerprints, baseline.fingerprints);
    }
}
