use async_trait::async_trait;
use rsi_agent_kernel::{ApplicationReceipt, ApplicationReceiptReader};
use rsi_host::HostPaths;
use rsi_tools_protocol::{ToolError, ToolResult};
use std::path::PathBuf;

/// Returns the product-owned directory for one application profile's tool outcomes.
pub fn application_tool_receipt_directory(
    paths: &HostPaths,
    source: &str,
) -> rsi_packages::Result<PathBuf> {
    rsi_packages::validate_identifier(source)?;
    Ok(paths.state().join("application-tool-outcomes").join(source))
}

#[derive(Debug)]
pub(crate) struct Receipts(pub HostPaths);

#[async_trait]
impl ApplicationReceiptReader for Receipts {
    async fn read(
        &self,
        receipt: ApplicationReceipt,
    ) -> rsi_tools_protocol::Result<Option<ToolResult>> {
        let directory = application_tool_receipt_directory(&self.0, &receipt.source)
            .map_err(|_| ToolError::InvalidInput("invalid application receipt source".into()))?;
        #[cfg(any(unix, windows))]
        return tokio::task::spawn_blocking(move || read_file(&directory, &receipt))
            .await
            .map_err(|_| ToolError::OutcomeUnknown)?;
        #[cfg(not(any(unix, windows)))]
        {
            let _ = directory;
            Ok(None)
        }
    }
}

#[cfg(any(unix, windows))]
fn read_file(
    directory: &std::path::Path,
    receipt: &ApplicationReceipt,
) -> rsi_tools_protocol::Result<Option<ToolResult>> {
    use rsi_tools_protocol::{ToolOutput, ToolResultIdentity};
    use serde::Deserialize;
    use sha2::{Digest, Sha256};
    use std::{
        io::{ErrorKind, Read},
        path::Path,
    };
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Record {
        session: rsi_agent_session_protocol::SessionId,
        effect: rsi_agent_session_protocol::EffectId,
        registration: String,
        identity: ToolResultIdentity,
        #[serde(rename = "signature")]
        _signature: String,
        state: String,
        result: Option<ToolOutput>,
    }
    const LIMIT: u64 = 32 * 1024 * 1024;
    let key = serde_json::to_vec(&(&receipt.session, &receipt.effect))
        .map_err(|_| ToolError::OutcomeUnknown)?;
    let filename = format!("{}.json", hex::encode(Sha256::digest(key)));
    let open = || {
        let root = rsi_files_native_fs::open_absolute_directory_no_follow(directory)?;
        rsi_files_native_fs::open_relative_file_no_follow(&root, Path::new(&filename))
    };
    let file = match open() {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ToolError::OutcomeUnknown),
    };
    let metadata = file.metadata().map_err(|_| ToolError::OutcomeUnknown)?;
    if !metadata.is_file() || metadata.len() > LIMIT {
        return Err(ToolError::OutcomeUnknown);
    }
    let mut data = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut data)
        .map_err(|_| ToolError::OutcomeUnknown)?;
    if data.len() as u64 > LIMIT {
        return Err(ToolError::OutcomeUnknown);
    }
    let record: Record = serde_json::from_slice(&data).map_err(|_| ToolError::OutcomeUnknown)?;
    if record.session != receipt.session
        || record.effect != receipt.effect
        || record.registration != receipt.registration
        || record.identity != receipt.identity
        || record.state != "settled"
    {
        return Ok(None);
    }
    let returned = record.result.ok_or(ToolError::OutcomeUnknown)?;
    returned.into_result().map(Some)
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;
    use serde_json::json;
    use sha2::{Digest, Sha256};
    #[test]
    fn cold_lookup_accepts_only_a_complete_exact_receipt_and_does_not_create_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let receipt = ApplicationReceipt {
            source: "market".into(),
            session: rsi_agent_session_protocol::SessionId::new("one").unwrap(),
            effect: rsi_agent_session_protocol::EffectId::new("effect").unwrap(),
            registration: "registration".into(),
            identity: rsi_tools_protocol::ToolResultIdentity::new(
                "owner",
                "effect",
                "call",
                "a".repeat(64),
            )
            .unwrap(),
        };
        let file = dir.path().join(format!(
            "{}.json",
            hex::encode(Sha256::digest(br#"["one","effect"]"#))
        ));
        assert!(read_file(dir.path(), &receipt).unwrap().is_none());
        assert!(!file.exists());
        let mut record = json!({"session":"one","effect":"effect","registration":"registration","identity":receipt.identity,
            "signature":"signature","state":"settled","result":{"value":"once","content":[],"is_error":false}});
        let save = |value: &serde_json::Value| {
            std::fs::write(&file, serde_json::to_vec(value).unwrap()).unwrap();
        };
        save(&record);
        assert_eq!(
            read_file(dir.path(), &receipt).unwrap().unwrap().value,
            "once"
        );
        for (key, value) in [
            ("session", "other"),
            ("effect", "other"),
            ("registration", "other"),
            ("state", "pending"),
        ] {
            let mut wrong = record.clone();
            wrong[key] = json!(value);
            save(&wrong);
            assert!(read_file(dir.path(), &receipt).unwrap().is_none());
        }
        record["identity"]["request_sha256"] = json!("b".repeat(64));
        save(&record);
        assert!(read_file(dir.path(), &receipt).unwrap().is_none());
        record["identity"] = json!(receipt.identity);
        record["result"]["enforcement"] = json!([]);
        save(&record);
        assert!(read_file(dir.path(), &receipt).is_err());
        std::fs::OpenOptions::new()
            .write(true)
            .open(&file)
            .unwrap()
            .set_len(33 * 1024 * 1024)
            .unwrap();
        assert!(read_file(dir.path(), &receipt).is_err());
    }
}
