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
    helper_target: String,
    artifacts: BTreeMap<String, String>,
}
/// The adjacent receipt is installed with the binary, never supplied by an API caller.
pub(super) async fn installed() -> Result<HelperArtifact, ()> {
    let (path, digest) = tokio::task::spawn_blocking(|| {
        let executable = std::env::current_exe()
            .map_err(|_| ())?
            .canonicalize()
            .map_err(|_| ())?;
        receipt(&executable, Path::new("/proc/self/exe"))
    })
    .await
    .map_err(|_| ())??;
    HelperArtifact::load(&path, digest).await.map_err(|_| ())
}
fn receipt(executable: &Path, running_image: &Path) -> Result<(PathBuf, [u8; 32]), ()> {
    let directory = executable.parent().ok_or(())?;
    let receipt: Receipt =
        serde_json::from_slice(&bounded(&directory.join("receipt.json"), 1024 * 1024)?)
            .map_err(|_| ())?;
    let cpu = std::env::consts::ARCH;
    if receipt.format != 1
        || receipt.mode != "published"
        || !matches!(receipt.profile.as_str(), "debug" | "release")
        || !matches!(receipt.kind.as_str(), "web" | "desktop")
        || receipt.helper_target != format!("{cpu}-unknown-linux-musl")
        || receipt.target != format!("{cpu}-unknown-linux-gnu")
        || receipt.artifacts.len() > 1024
    {
        return Err(());
    }
    for (name, digest) in &receipt.artifacts {
        if name.len() > 256 || name.chars().any(char::is_control) {
            return Err(());
        }
        hash(digest)?;
    }
    let family = bounded(&directory.join("build-family.json"), 16 * 1024 * 1024)?;
    if Sha256::digest(&family).as_slice() != hash(&receipt.family_sha256)? {
        return Err(());
    }
    let name = executable.file_name().and_then(|v| v.to_str()).ok_or(())?;
    if !matches!(name, "rsi" | "rsi-desktop") {
        return Err(());
    }
    let expected = hash(receipt.artifacts.get(name).ok_or(())?)?;
    let executable = std::fs::File::open(running_image).map_err(|_| ())?;
    let metadata = executable.metadata().map_err(|_| ())?;
    if !metadata.is_file() || metadata.len() > 2 * 1024 * 1024 * 1024 {
        return Err(());
    }
    let mut executable = executable.take(2 * 1024 * 1024 * 1024);
    let mut actual = Sha256::new();
    std::io::copy(&mut executable, &mut actual).map_err(|_| ())?;
    if actual.finalize().as_slice() != expected {
        return Err(());
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
    #[test]
    fn installed_receipt_binds_host_binary_family_target_and_helper_digest() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("rsi");
        std::fs::write(&executable, b"fixture-host-executable").unwrap();
        std::fs::write(root.path().join("build-family.json"), b"fixture-family").unwrap();
        let mut value = serde_json::json!({
            "format":1, "family_sha256":hex::encode(Sha256::digest(b"fixture-family")),
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
            receipt(&executable, &executable).unwrap(),
            (root.path().join("rsi-ssh-helper"), [0xaa; 32])
        );
        value["helper_target"] = "x86_64-unknown-linux-gnu".into();
        publish(&value);
        assert!(receipt(&executable, &executable).is_err());
        value["helper_target"] = format!("{}-unknown-linux-musl", std::env::consts::ARCH).into();
        publish(&value);
        std::fs::write(root.path().join("build-family.json"), b"replaced-family").unwrap();
        assert!(receipt(&executable, &executable).is_err());
        std::fs::write(root.path().join("build-family.json"), b"fixture-family").unwrap();
        std::fs::write(&executable, b"replaced-host-executable").unwrap();
        assert!(receipt(&executable, &executable).is_err());
        std::fs::write(&executable, b"fixture-host-executable").unwrap();
        value["artifacts"]["rsi-ssh-helper"] = "A".repeat(64).into();
        publish(&value);
        assert!(receipt(&executable, &executable).is_err());
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
            "format":1, "family_sha256":hex::encode(Sha256::digest(b"family")),
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
        assert!(receipt(&executable, &running).is_err());
        value["artifacts"]["rsi"] = hex::encode(Sha256::digest(b"running-image")).into();
        publish(&value);
        assert!(receipt(&executable, &running).is_ok());
        std::fs::File::options()
            .write(true)
            .open(&running)
            .unwrap()
            .set_len(2 * 1024 * 1024 * 1024 + 1)
            .unwrap();
        assert!(receipt(&executable, &running).is_err());
    }
}
