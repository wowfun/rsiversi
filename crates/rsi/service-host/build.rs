mod build_family;
use sha2::{Digest, Sha256};
use std::io::Read;

fn main() {
    println!("cargo:rerun-if-env-changed=RSI_BUILD_FAMILY_MANIFEST");
    let Some(path) = std::env::var_os("RSI_BUILD_FAMILY_MANIFEST") else {
        return;
    };
    let path = std::fs::canonicalize(path).expect("resolve paired build manifest");
    println!("cargo:rerun-if-changed={}", path.display());
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .expect("open paired build manifest")
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .expect("read paired build manifest");
    assert!(
        bytes.len() <= 16 * 1024 * 1024,
        "paired build manifest exceeds 16 MiB"
    );
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).expect("paired build manifest JSON");
    let package =
        std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("package root"));
    let root = package.ancestors().nth(3).expect("paired source root");
    for path in build_family::validate(root, &value).expect("paired build source verification") {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!(
        "cargo:rustc-env=RSI_COMPILED_BUILD_FAMILY={:x}",
        Sha256::digest(&bytes)
    );
}
