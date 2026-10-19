use rsi_ssh_client::HelperArtifact;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    format: u32,
    family_sha256: String,
    target: String,
    profile: String,
    kind: String,
    mode: String,
    helper_target: Option<String>,
    artifacts: BTreeMap<String, String>,
}
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Unavailable {
    NotIncluded,
    Invalid,
}
impl From<()> for Unavailable {
    fn from((): ()) -> Self {
        Self::Invalid
    }
}
/// The adjacent receipt is installed with the binary, never supplied by an API caller.
pub(super) async fn installed() -> Result<Option<HelperArtifact>, Unavailable> {
    let verified = tokio::task::spawn_blocking(|| {
        let executable = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map_err(|_| Unavailable::Invalid)?;
        receipt(
            &executable,
            Path::new("/proc/self/exe"),
            rsi_build_info::family(),
        )
    })
    .await
    .map_err(|_| Unavailable::Invalid)?;
    load_verified(verified).await
}
async fn load_verified(
    verified: Result<(PathBuf, [u8; 32]), Unavailable>,
) -> Result<Option<HelperArtifact>, Unavailable> {
    let (path, digest) = match verified {
        Ok(value) => value,
        Err(Unavailable::NotIncluded) => return Ok(None),
        Err(error) => return Err(error),
    };
    HelperArtifact::load(&path, digest)
        .await
        .map(Some)
        .map_err(|_| Unavailable::Invalid)
}
fn receipt(
    executable: &Path,
    running_image: &Path,
    compiled_family: Option<&str>,
) -> Result<(PathBuf, [u8; 32]), Unavailable> {
    let directory = executable.parent().ok_or(())?;
    let receipt: Receipt =
        serde_json::from_slice(&bounded(&directory.join("receipt.json"), 1024 * 1024)?)
            .map_err(|_| ())?;
    let cpu = std::env::consts::ARCH;
    if receipt.format != 2
        || receipt.mode != "published"
        || !matches!(receipt.profile.as_str(), "debug" | "release")
        || !matches!(receipt.kind.as_str(), "tui" | "web" | "desktop")
        || receipt.target != format!("{cpu}-unknown-linux-gnu")
        || receipt.artifacts.len() > 1024
    {
        return Err(Unavailable::Invalid);
    }
    for (name, digest) in &receipt.artifacts {
        if name.len() > 256 || name.chars().any(char::is_control) {
            return Err(Unavailable::Invalid);
        }
        hash(digest)?;
    }
    let family = bounded(&directory.join("build-family.json"), 16 * 1024 * 1024)?;
    if Sha256::digest(&family).as_slice() != hash(&receipt.family_sha256)? {
        return Err(Unavailable::Invalid);
    }
    if compiled_family != Some(receipt.family_sha256.as_str()) {
        return Err(Unavailable::Invalid);
    }
    let name = executable.file_name().and_then(|v| v.to_str()).ok_or(())?;
    if !matches!(name, "rsi" | "rsi-desktop") {
        return Err(Unavailable::Invalid);
    }
    let expected = hash(receipt.artifacts.get(name).ok_or(())?)?;
    let executable = std::fs::File::open(running_image).map_err(|_| ())?;
    let metadata = executable.metadata().map_err(|_| ())?;
    if !metadata.is_file() || metadata.len() > 2 * 1024 * 1024 * 1024 {
        return Err(Unavailable::Invalid);
    }
    let mut executable = executable.take(2 * 1024 * 1024 * 1024);
    let mut actual = Sha256::new();
    std::io::copy(&mut executable, &mut actual).map_err(|_| ())?;
    if actual.finalize().as_slice() != expected {
        return Err(Unavailable::Invalid);
    }
    if receipt.helper_target.is_none() {
        return if receipt.kind == "tui"
            && !receipt.artifacts.contains_key("rsi-ssh-helper")
            && !directory
                .join("rsi-ssh-helper")
                .try_exists()
                .map_err(|_| ())?
        {
            Err(Unavailable::NotIncluded)
        } else {
            Err(Unavailable::Invalid)
        };
    }
    if receipt.helper_target.as_deref() != Some(format!("{cpu}-unknown-linux-musl").as_str()) {
        return Err(Unavailable::Invalid);
    }
    Ok((
        directory.join("rsi-ssh-helper"),
        hash(receipt.artifacts.get("rsi-ssh-helper").ok_or(())?)?,
    ))
}
fn hash(value: &str) -> Result<[u8; 32], ()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
    {
        return Err(());
    }
    hex::decode(value)
        .map_err(|_| ())?
        .try_into()
        .map_err(|_| ())
}
fn bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, ()> {
    let file = std::fs::File::open(path).map_err(|_| ())?;
    if !file.metadata().map_err(|_| ())?.is_file() {
        return Err(());
    }
    let mut bytes = Vec::new();
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() as u64 > maximum {
        return Err(());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cache_retains_verified_omission_but_retries_invalid_publications() {
        let cache = tokio::sync::OnceCell::new();
        assert!(
            cache
                .get_or_try_init(|| load_verified(Err(Unavailable::Invalid)))
                .await
                .is_err()
        );
        assert!(cache.get().is_none());
        assert!(
            cache
                .get_or_try_init(|| load_verified(Err(Unavailable::NotIncluded)))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            cache
                .get_or_try_init(|| async { Err::<_, Unavailable>(Unavailable::Invalid) })
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn installed_receipt_binds_host_binary_family_target_and_helper_digest() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("rsi");
        std::fs::write(&executable, b"fixture-host-executable").unwrap();
        std::fs::write(root.path().join("build-family.json"), b"fixture-family").unwrap();
        let mut value = serde_json::json!({
            "format":2, "family_sha256":hex::encode(Sha256::digest(b"fixture-family")),
            "target":format!("{}-unknown-linux-gnu", std::env::consts::ARCH),
            "helper_target":format!("{}-unknown-linux-musl", std::env::consts::ARCH),
            "profile":"debug", "kind":"web", "mode":"published",
            "artifacts":{"rsi":hex::encode(Sha256::digest(b"fixture-host-executable")), "rsi-ssh-helper":"a".repeat(64)}
        });
        let publish = |value: &serde_json::Value| {
            std::fs::write(
                root.path().join("receipt.json"),
                serde_json::to_vec(value).unwrap(),
            )
            .unwrap();
        };
        publish(&value);
        assert_eq!(
            receipt(
                &executable,
                &executable,
                Some(&hex::encode(Sha256::digest(b"fixture-family")))
            )
            .unwrap(),
            (root.path().join("rsi-ssh-helper"), [0xaa; 32])
        );
        value["kind"] = "tui".into();
        value["helper_target"] = serde_json::Value::Null;
        let helper_hash = value["artifacts"]
            .as_object_mut()
            .unwrap()
            .remove("rsi-ssh-helper")
            .unwrap();
        publish(&value);
        let family = hex::encode(Sha256::digest(b"fixture-family"));
        assert_eq!(
            receipt(&executable, &executable, Some(&family)),
            Err(Unavailable::NotIncluded)
        );
        assert_eq!(
            receipt(&executable, &executable, None),
            Err(Unavailable::Invalid)
        );
        std::fs::write(root.path().join("rsi-ssh-helper"), b"unexpected").unwrap();
        assert_eq!(
            receipt(&executable, &executable, Some(&family)),
            Err(Unavailable::Invalid)
        );
        std::fs::remove_file(root.path().join("rsi-ssh-helper")).unwrap();
        std::fs::write(&executable, b"tampered").unwrap();
        assert_eq!(
            receipt(&executable, &executable, Some(&family)),
            Err(Unavailable::Invalid)
        );
        std::fs::write(&executable, b"fixture-host-executable").unwrap();
        value["kind"] = "web".into();
        publish(&value);
        assert_eq!(
            receipt(&executable, &executable, Some(&family)),
            Err(Unavailable::Invalid)
        );
        value["artifacts"]["rsi-ssh-helper"] = helper_hash;
        value["helper_target"] = "x86_64-unknown-linux-gnu".into();
        publish(&value);
        assert!(
            receipt(
                &executable,
                &executable,
                Some(&hex::encode(Sha256::digest(b"fixture-family")))
            )
            .is_err()
        );
        value["helper_target"] = format!("{}-unknown-linux-musl", std::env::consts::ARCH).into();
        publish(&value);
        std::fs::write(root.path().join("build-family.json"), b"replaced-family").unwrap();
        assert!(
            receipt(
                &executable,
                &executable,
                Some(&hex::encode(Sha256::digest(b"fixture-family")))
            )
            .is_err()
        );
        std::fs::write(root.path().join("build-family.json"), b"fixture-family").unwrap();
        std::fs::write(&executable, b"replaced-host-executable").unwrap();
        assert!(
            receipt(
                &executable,
                &executable,
                Some(&hex::encode(Sha256::digest(b"fixture-family")))
            )
            .is_err()
        );
        std::fs::write(&executable, b"fixture-host-executable").unwrap();
        value["artifacts"]["rsi-ssh-helper"] = "A".repeat(64).into();
        publish(&value);
        assert!(
            receipt(
                &executable,
                &executable,
                Some(&hex::encode(Sha256::digest(b"fixture-family")))
            )
            .is_err()
        );
    }

    #[test]
    fn receipt_hashes_the_running_image_and_rejects_oversize_before_reading() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("rsi");
        let running = root.path().join("running-image");
        std::fs::write(&executable, b"replacement").unwrap();
        std::fs::write(&running, b"running-image").unwrap();
        std::fs::write(root.path().join("build-family.json"), b"family").unwrap();
        let mut value = serde_json::json!({
            "format":2, "family_sha256":hex::encode(Sha256::digest(b"family")),
            "target":format!("{}-unknown-linux-gnu", std::env::consts::ARCH),
            "helper_target":format!("{}-unknown-linux-musl", std::env::consts::ARCH),
            "profile":"debug", "kind":"web", "mode":"published",
            "artifacts":{"rsi":hex::encode(Sha256::digest(b"replacement")), "rsi-ssh-helper":"a".repeat(64)}
        });
        let publish = |value: &serde_json::Value| {
            std::fs::write(
                root.path().join("receipt.json"),
                serde_json::to_vec(value).unwrap(),
            )
            .unwrap();
        };
        publish(&value);
        assert!(
            receipt(
                &executable,
                &running,
                Some(&hex::encode(Sha256::digest(b"family")))
            )
            .is_err()
        );
        value["artifacts"]["rsi"] = hex::encode(Sha256::digest(b"running-image")).into();
        publish(&value);
        assert!(
            receipt(
                &executable,
                &running,
                Some(&hex::encode(Sha256::digest(b"family")))
            )
            .is_ok()
        );
        std::fs::File::options()
            .write(true)
            .open(&running)
            .unwrap()
            .set_len(2 * 1024 * 1024 * 1024 + 1)
            .unwrap();
        assert!(
            receipt(
                &executable,
                &running,
                Some(&hex::encode(Sha256::digest(b"family")))
            )
            .is_err()
        );
    }
}
