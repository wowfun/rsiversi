use std::{collections::BTreeSet, env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=bundle.json");
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string("bundle.json").expect("bundle manifest")).unwrap();
    let bootstrap: Vec<_> = manifest["bootstrap"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| {
            assert!(matches!(
                file["stage"].as_str(),
                Some("document" | "preview" | "copy" | "worker" | "wasm")
            ));
            file["name"].as_str().unwrap()
        })
        .collect();
    let all: Vec<_> = bootstrap
        .iter()
        .copied()
        .chain(
            manifest["renderers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|name| name.as_str().unwrap()),
        )
        .collect();
    let mut unique = BTreeSet::new();
    assert!(all.len() <= 128);
    for name in &all {
        assert!(!name.is_empty() && name.len() <= 128 && !name.starts_with('.'));
        assert!(
            name.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        );
        assert!(unique.insert(name), "duplicate bundle file: {name}");
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("bundle.rs");
    fs::write(output, format!(
        "/// Fixed bootstrap files that renderer catalogs may never replace.\npub const BOOTSTRAP: &[&str] = &{bootstrap:?};\npub const DEFAULT_FILES: &[&str] = &{all:?};\n"
    )).unwrap();
}
