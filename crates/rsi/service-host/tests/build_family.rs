#[path = "../build_family.rs"]
mod build_family;
use serde_json::json;
use sha2::{Digest, Sha256};

#[test]
fn family_manifest_rejects_stale_or_escaping_source() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("source.rs");
    std::fs::write(&file, b"original").unwrap();
    let manifest = json!({"format":1,"files":{"source.rs":{"kind":"file","bytes":8,"executable":false,"sha256":format!("{:x}", Sha256::digest(b"original"))}}});
    assert_eq!(
        build_family::validate(root.path(), &manifest).unwrap(),
        vec![file.canonicalize().unwrap()]
    );
    std::fs::write(&file, b"modified").unwrap();
    assert!(
        build_family::validate(root.path(), &manifest)
            .unwrap_err()
            .contains("digest changed")
    );
    std::fs::remove_file(file).unwrap();
    assert!(build_family::validate(root.path(), &manifest).is_err());
    let escaping = json!({"format":1,"files":{"../escape":{"kind":"file"}}});
    assert!(
        build_family::validate(root.path(), &escaping)
            .unwrap_err()
            .contains("invalid frozen input path")
    );
}

#[cfg(unix)]
#[test]
fn family_manifest_returns_canonical_sources_through_a_root_alias() {
    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("real");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(real.join("source.rs"), b"original").unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let manifest = json!({"format":1,"files":{"source.rs":{"kind":"file","bytes":8,"executable":false,"sha256":format!("{:x}", Sha256::digest(b"original"))}}});
    assert_eq!(
        build_family::validate(&alias, &manifest).unwrap(),
        vec![real.join("source.rs").canonicalize().unwrap()]
    );
}

#[cfg(unix)]
#[test]
fn family_manifest_checks_symlink_identity_and_executable_mode() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("alias");
    symlink("missing", &path).unwrap();
    let manifest = json!({"format":1,"files":{"alias":{"kind":"symlink","target":"missing"}}});
    build_family::validate(root.path(), &manifest).unwrap();
    std::fs::remove_file(&path).unwrap();
    symlink("another", &path).unwrap();
    assert!(build_family::validate(root.path(), &manifest).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"script").unwrap();
    let manifest = json!({"format":1,"files":{"alias":{"kind":"file","bytes":6,"executable":true,"sha256":format!("{:x}",Sha256::digest(b"script"))}}});
    assert!(
        build_family::validate(root.path(), &manifest)
            .unwrap_err()
            .contains("executable bit")
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o555)).unwrap();
    build_family::validate(root.path(), &manifest).unwrap();
}
