//! Opt-in cross-package resolution using admitted payloads, with native leases
//! held through the entire Node process. Never selects the staging npm tree.
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::Arc,
};

#[test]
#[ignore = "requires the separately built market fixtures, private PackageStore, and Node"]
fn market_profile_executes_with_exact_leases() {
    let path = |name| PathBuf::from(std::env::var_os(name).unwrap());
    let store = rsi_packages::PackageStore::open(&path("RSI_MARKET_STORE")).unwrap();
    let bundle = path("RSI_MARKET_BUNDLE");
    let receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(bundle.join("receipt.json")).unwrap()).unwrap();
    let mut roots = std::collections::BTreeMap::new();
    let selection = &receipt["selection"];
    let lease = Arc::new(
        store
            .acquire_exact(
                selection["name"].as_str().unwrap(),
                selection["digest"].as_str().unwrap(),
            )
            .unwrap(),
    );
    let profile = lease.payload("profile").unwrap();
    for row in receipt["artifacts"].as_array().unwrap() {
        let name = row["name"].as_str().unwrap();
        assert!(
            lease
                .manifest()
                .dependencies
                .iter()
                .any(|dep| dep.name == name
                    && Some(dep.digest.as_str()) == row["manifestDigest"].as_str())
        );
        let payload = lease.dependency_payload(name, "runtime").unwrap();
        roots.insert(name, payload.path().to_owned());
    }
    let layout: serde_json::Value =
        serde_json::from_slice(&std::fs::read(profile.path().join("layout.json")).unwrap())
            .unwrap();
    let mut child = Command::new(path("RSI_EXTENSION_NODE"))
        .arg(path("RSI_MARKET_PROBE"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            &serde_json::to_vec(&serde_json::json!({"layout":layout,"roots":roots})).unwrap(),
        )
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["runtime"], receipt["version"]);
    assert_eq!(report["sharedCordis"], true);
    assert_eq!(report["plugins"], receipt["plugins"]);
    assert_eq!(report["sidebarCalls"], 1);
    if let Some(output) = std::env::var_os("RSI_MARKET_PROBE_RECEIPT") {
        std::fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}
