//! Small explicit wire declarations, independent of implementation source.
#![deny(unsafe_code)]
#![warn(missing_docs)]
use serde::{Deserialize, Serialize};
use std::{io::Read as _, path::Path};

/// One independently versioned protocol owner.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    /// Stable protocol name.
    pub name: String,
    /// Explicit semantic revision.
    pub version: u32,
}
impl Contract {
    /// Checks the same identity grammar as runtime peers.
    ///
    /// # Errors
    /// Returns an error for an invalid protocol name or zero version.
    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty()
            || self.name.len() > 128
            || !self.name.as_bytes()[0].is_ascii_lowercase()
            || !self
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
            || self.version == 0
        {
            return Err("invalid wire contract identity".into());
        }
        Ok(())
    }
}
/// Reads only the owner's bounded explicit declaration.
///
/// # Errors
/// Returns an error when the declaration cannot be read or is malformed or oversized.
pub fn read(root: &Path) -> Result<Contract, String> {
    let path = root.join("wire-contract.json");
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .take(1025)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 1024 {
        return Err("wire declaration exceeds 1 KiB".into());
    }
    let contract: Contract = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    contract.validate()?;
    Ok(contract)
}
/// Emits Cargo constants and observes only the declaration.
///
/// # Panics
/// Panics when Cargo's manifest directory or a valid declaration is absent.
pub fn build() {
    let root = std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory");
    let root = Path::new(&root);
    let contract = read(root).unwrap_or_else(|e| panic!("wire contract: {e}"));
    println!(
        "cargo:rerun-if-changed={}",
        root.join("wire-contract.json").display()
    );
    println!("cargo:rustc-env=RSI_WIRE_NAME={}", contract.name);
    println!("cargo:rustc-env=RSI_WIRE_VERSION={}", contract.version);
}
#[cfg(test)]
mod tests {
    #[test]
    fn declaration_file_bounds_and_closed_fields() {
        let root = tempfile::tempdir().unwrap();
        assert!(super::read(root.path()).is_err());
        let path = root.path().join("wire-contract.json");
        let json = r#"{"name":"fixture.wire","version":1}"#;
        std::fs::write(&path, format!("{json: <1024}")).unwrap();
        assert_eq!(super::read(root.path()).unwrap().name, "fixture.wire");
        std::fs::write(&path, format!("{json: <1025}")).unwrap();
        assert!(super::read(root.path()).unwrap_err().contains("1 KiB"));
        std::fs::write(&path, r#"{"name":"fixture.wire","version":1,"extra":true}"#).unwrap();
        assert!(super::read(root.path()).is_err());
    }
    #[test]
    fn implementation_changes_do_not_change_identity() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("wire-contract.json"),
            br#"{"name":"rsi.test","version":1}"#,
        )
        .unwrap();
        let before = super::read(root.path()).unwrap();
        std::fs::write(root.path().join("private.rs"), "arbitrary implementation").unwrap();
        std::fs::write(root.path().join("Cargo.lock"), "arbitrary dependencies").unwrap();
        assert_eq!(before, super::read(root.path()).unwrap());
    }
    #[test]
    fn identities_match_shared_corpus() {
        let cases: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../../protocol/descriptor-conformance.json"))
                .unwrap();
        for mut case in cases {
            let valid = case
                .as_object_mut()
                .unwrap()
                .remove("valid")
                .unwrap()
                .as_bool()
                .unwrap();
            let result = serde_json::from_value::<super::Contract>(case.clone());
            assert_eq!(result.is_ok_and(|c| c.validate().is_ok()), valid, "{case}");
        }
    }
}
