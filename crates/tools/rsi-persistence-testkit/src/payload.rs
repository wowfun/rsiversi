//! Immutable owner-local payload evidence, separate from runtime admission.
use crate::{structural, validation_schema};
use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::{io::Write as _, path::Path};

fn encoded<T: Serialize + JsonSchema>(version: u32, samples: &[T]) -> Value {
    assert!(
        !samples.is_empty(),
        "capture requires actual writer samples"
    );
    json!({
        "version":version,
        "schema":structural::<T>(),
        "samples":samples.iter().map(|sample| serde_json::to_string(sample).unwrap()).collect::<Vec<_>>(),
    })
}

/// Captures the current codec's actual output into an explicitly new file.
///
/// # Panics
/// Panics on encoding, I/O, schema mismatch, or an already existing destination.
pub fn capture<T: Serialize + DeserializeOwned + JsonSchema>(
    destination: &Path,
    version: u32,
    samples: &[T],
) {
    let payload = encoded(version, samples);
    check::<T>(&payload, version);
    let digest = hex::encode(Sha256::digest(serde_json::to_vec(&payload).unwrap()));
    let bytes = serde_json::to_vec_pretty(&json!({"payload":payload,"sha256":digest})).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .expect("capture never replaces retained evidence");
    file.write_all(&bytes).unwrap();
}

/// Checks the version, structural shape, field order and exact bytes of a current codec.
/// Older versions remain in the owner's corpus; runtime admission owns their refusal.
///
/// # Panics
/// Panics on changed or malformed evidence, or any same-version codec change.
pub fn verify<T: Serialize + DeserializeOwned + JsonSchema>(source: &Path, version: u32) {
    let payload = retained(source);
    check::<T>(&payload, version);
}

/// Verifies every retained version of the named codec. Owners must exercise their
/// actual reader or refusal for each older fixture in `legacy`.
///
/// # Panics
/// Panics on invalid evidence, unexpected versions or an owner's failed assertion.
pub fn verify_corpus<T: Serialize + DeserializeOwned + JsonSchema>(
    source: &Path,
    version: u32,
    legacy: impl Fn(&Path, u32),
) {
    verify::<T>(source, version);
    let stem = source.file_stem().unwrap().to_str().unwrap();
    let (name, current) = stem
        .rsplit_once("-v")
        .expect("codec fixture uses NAME-vVERSION.json");
    assert_eq!(current.parse::<u32>().unwrap(), version);
    let prefix = format!("{name}-v");
    let mut paths = std::fs::read_dir(source.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        if path == source {
            continue;
        }
        let filename = path.file_name().unwrap().to_str().unwrap();
        let Some(old) = filename
            .strip_prefix(&prefix)
            .and_then(|s| s.strip_suffix(".json"))
        else {
            continue;
        };
        let old: u32 = old.parse().expect("retained version is an integer");
        assert!(
            old < version,
            "unexpected retained codec version: {}",
            path.display()
        );
        let payload = retained(&path);
        assert_eq!(payload["version"], old);
        check_samples(&payload);
        legacy(&path, old);
    }
}

fn retained(source: &Path) -> Value {
    let mut fixture: Value = serde_json::from_slice(&std::fs::read(source).unwrap()).unwrap();
    let payload = &fixture["payload"];
    assert_eq!(
        fixture["sha256"],
        hex::encode(Sha256::digest(serde_json::to_vec(payload).unwrap())),
        "retained writer evidence changed"
    );
    fixture["payload"].take()
}

fn check<T: Serialize + DeserializeOwned + JsonSchema>(payload: &Value, version: u32) {
    assert_eq!(
        payload["version"], version,
        "owner version needs new retained evidence"
    );
    assert_eq!(
        serde_json::to_vec(&payload["schema"]).unwrap(),
        serde_json::to_vec(&structural::<T>()).unwrap(),
        "same-version schema or field order changed; retain old evidence and bump the owner's format"
    );
    check_samples(payload);
    for sample in payload["samples"].as_array().unwrap() {
        let bytes = sample.as_str().unwrap();
        let decoded: T = serde_json::from_str(bytes).unwrap();
        assert_eq!(
            serde_json::to_string(&decoded).unwrap(),
            bytes,
            "retained payload encoding changed"
        );
    }
}

fn check_samples(payload: &Value) {
    let schema = validation_schema(&payload["schema"]);
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft7)
        .build(&schema)
        .unwrap();
    let samples = payload["samples"].as_array().unwrap();
    assert!(!samples.is_empty());
    for sample in samples {
        let bytes = sample.as_str().unwrap();
        let value: Value = serde_json::from_str(bytes).unwrap();
        assert!(
            validator.is_valid(&value),
            "actual writer violates its serialization schema"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn corpus_requires_old_version_coverage_and_checks_retained_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let old = directory.path().join("record-v1.json");
        let current = directory.path().join("record-v2.json");
        let samples = [Original {
            first: "retained".into(),
            second: 3,
        }];
        capture(&old, 1, &samples);
        capture(&current, 2, &samples);
        // Other codec names are separate corpora in the same owner directory.
        std::fs::write(directory.path().join("other-v1.json"), b"unrelated").unwrap();
        assert!(
            std::panic::catch_unwind(|| verify_corpus::<Original>(&current, 2, |_, _| panic!(
                "missing owner coverage"
            ),))
            .is_err()
        );
        let seen = std::cell::Cell::new(0);
        verify_corpus::<Original>(&current, 2, |path, version| {
            assert_eq!(path, old);
            assert_eq!(version, 1);
            verify::<Original>(path, version);
            seen.set(seen.get() + 1);
        });
        assert_eq!(seen.get(), 1);
        let mut fixture: Value = serde_json::from_slice(&std::fs::read(&old).unwrap()).unwrap();
        fixture["sha256"] = Value::String("changed".into());
        std::fs::write(&old, serde_json::to_vec(&fixture).unwrap()).unwrap();
        assert!(
            std::panic::catch_unwind(|| verify_corpus::<Original>(&current, 2, |_, _| {},))
                .is_err()
        );
    }
    #[derive(Serialize, serde::Deserialize, JsonSchema)]
    struct Original {
        first: String,
        second: u64,
    }
    #[derive(Serialize, serde::Deserialize, JsonSchema)]
    struct Reordered {
        second: u64,
        first: String,
    }
    #[derive(Serialize, serde::Deserialize, JsonSchema)]
    struct Optional {
        first: Option<String>,
        second: u64,
    }
    #[derive(Serialize, serde::Deserialize, JsonSchema)]
    struct Renamed {
        renamed: String,
        second: u64,
    }
    #[derive(Serialize, serde::Deserialize, JsonSchema)]
    enum OriginalKind {
        First,
    }
    #[derive(Serialize, serde::Deserialize, JsonSchema)]
    enum ExtendedKind {
        First,
        Second,
    }
    #[test]
    fn gate_rejects_order_requiredness_and_version_changes() {
        let payload = encoded(
            1,
            &[Original {
                first: "中🦀\"\\".into(),
                second: 3,
            }],
        );
        check::<Original>(&payload, 1);
        assert!(std::panic::catch_unwind(|| check::<Reordered>(&payload, 1)).is_err());
        assert!(std::panic::catch_unwind(|| check::<Optional>(&payload, 1)).is_err());
        assert!(std::panic::catch_unwind(|| check::<Renamed>(&payload, 1)).is_err());
        assert!(std::panic::catch_unwind(|| check::<Original>(&payload, 2)).is_err());
        let original = encoded(1, &[OriginalKind::First]);
        let extended = encoded(1, &[ExtendedKind::First]);
        // Even an unsampled variant is part of the durable shape.
        assert!(std::panic::catch_unwind(|| check::<ExtendedKind>(&original, 1)).is_err());
        assert!(std::panic::catch_unwind(|| check::<OriginalKind>(&extended, 1)).is_err());
    }
}
