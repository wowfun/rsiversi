use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub(crate) fn run(repository: &Path, arguments: &[String]) -> Result<(), String> {
    crate::repository_root::require(repository, "verify-protocols")?;
    let mut write = false;
    let mut base = None;
    let mut args = arguments.iter();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--write" if !write => write = true,
            "--base" if base.is_none() => {
                base = Some(args.next().ok_or("--base requires a Git ref")?.as_str());
            }
            _ => return Err("usage: verify-protocols [--write] [--base REF]".into()),
        }
    }
    let baseline = if let Some(base) = base {
        if base == "0000000000000000000000000000000000000000" {
            None
        } else {
            Some(
                git(repository, &["merge-base", "HEAD", base])?
                    .trim()
                    .to_owned(),
            )
        }
    } else {
        None
    };
    let mut previous = baseline
        .as_deref()
        .map(|base| baseline_contracts(repository, base))
        .transpose()?
        .unwrap_or_default();
    let mut owners = Vec::new();
    for root in ["crates", "apps"] {
        find(&repository.join(root), &mut owners)?;
    }
    owners.sort();
    if owners.is_empty() {
        return Err("no wire contracts were discovered".into());
    }
    let mut errors = Vec::new();
    let mut names = std::collections::BTreeMap::new();
    let mut generator = rsi_api_schema_build::Generator::default();
    let mut pending = Vec::new();
    for owner in owners {
        let result = (|| {
            let generated = generator.generate(&owner)?;
            unique_owner(&mut names, &generated.snapshot.contract.name, &owner)?;
            let snapshot = owner.join("wire-schema.json");
            let bytes = generated.bytes;
            let relative = snapshot
                .strip_prefix(repository)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            if let Some(before) = previous.remove(&generated.snapshot.contract.name) {
                require_version_increase(&before, &bytes)?;
            }
            if write {
                pending.push((snapshot, bytes));
            } else if std::fs::read(&snapshot).map_err(|e| format!("{relative}: {e}"))? != bytes {
                return Err(format!(
                    "{relative} differs from generated wire contract; review the change and its version before --write"
                ));
            }
            Ok(())
        })();
        if let Err(error) = result {
            errors.push(format!("{}: {error}", owner.display()));
        }
    }
    for name in previous.keys() {
        errors.push(format!("baseline wire contract {name} was removed; an explicit contract retirement policy is required"));
    }
    if errors.is_empty() {
        for (path, bytes) in pending {
            std::fs::write(path, bytes).map_err(|e| e.to_string())?;
        }
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}
fn baseline_contracts(
    repository: &Path,
    base: &str,
) -> Result<std::collections::BTreeMap<String, Vec<u8>>, String> {
    let paths = git(
        repository,
        &["ls-tree", "-r", "--name-only", base, "--", "apps", "crates"],
    )?;
    let mut contracts = std::collections::BTreeMap::new();
    for path in paths
        .lines()
        .filter(|path| path.ends_with("/wire-schema.json"))
    {
        let bytes = git(repository, &["show", &format!("{base}:{path}")])?.into_bytes();
        let snapshot: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let name = snapshot
            .pointer("/contract/name")
            .and_then(serde_json::Value::as_str)
            .ok_or("baseline snapshot has no contract name")?;
        if contracts.insert(name.into(), bytes).is_some() {
            return Err(format!("duplicate baseline wire contract {name}"));
        }
    }
    Ok(contracts)
}
fn unique_owner(
    names: &mut std::collections::BTreeMap<String, PathBuf>,
    name: &str,
    owner: &Path,
) -> Result<(), String> {
    if let Some(previous) = names.get(name) {
        return Err(format!(
            "duplicate wire contract {name}: {} and {}",
            previous.display(),
            owner.display()
        ));
    }
    names.insert(name.into(), owner.into());
    Ok(())
}
fn git(repository: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .current_dir(repository)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}
fn find(root: &Path, owners: &mut Vec<PathBuf>) -> Result<(), String> {
    if !root.exists() {
        return Ok(());
    }
    if root.join("wire-contract.toml").is_file() {
        owners.push(root.to_owned());
        return Ok(());
    }
    for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_dir()
            && !matches!(
                entry.file_name().to_str(),
                Some("target" | "node_modules" | ".git" | "dist")
            )
        {
            find(&entry.path(), owners)?;
        }
    }
    Ok(())
}
fn require_version_increase(previous: &[u8], current: &[u8]) -> Result<(), String> {
    let previous: serde_json::Value =
        serde_json::from_slice(previous).map_err(|e| e.to_string())?;
    let current: serde_json::Value = serde_json::from_slice(current).map_err(|e| e.to_string())?;
    for snapshot in [&previous, &current] {
        if !matches!(snapshot["format"].as_u64(), Some(1 | 2)) {
            return Err("unsupported wire snapshot format".into());
        }
    }
    let version = |value: &serde_json::Value| {
        value
            .pointer("/contract/version")
            .and_then(serde_json::Value::as_u64)
            .ok_or("snapshot has no model version")
    };
    if previous != current && version(&current)? <= version(&previous)? {
        return Err("wire contract changed without increasing its model version".into());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_owner_prevents_every_snapshot_write() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("crates/tools/rsi-xtask")).unwrap();
        std::fs::write(root.path().join("crates/tools/rsi-xtask/Cargo.toml"), "").unwrap();
        std::fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(root.path().join("Cargo.lock"), "version=4\n").unwrap();
        for (name, contract) in [
            ("first", "name='same'\nversion=0\n"),
            ("second", "name='valid'\nversion=1\n"),
        ] {
            let owner = root.path().join("apps").join(name);
            std::fs::create_dir_all(owner.join("src")).unwrap();
            std::fs::write(
                owner.join("Cargo.toml"),
                format!("[package]\nname='{name}'\nversion='0.1.0'\n"),
            )
            .unwrap();
            std::fs::write(owner.join("wire-contract.toml"), contract).unwrap();
            std::fs::write(owner.join("src/lib.rs"), "").unwrap();
            std::fs::write(owner.join("wire-schema.json"), b"reviewed").unwrap();
        }
        assert!(run(root.path(), &["--write".into()]).is_err());
        assert_eq!(
            std::fs::read(root.path().join("apps/second/wire-schema.json")).unwrap(),
            b"reviewed"
        );
    }
    #[test]
    fn contract_names_have_one_owner() {
        let mut names = std::collections::BTreeMap::new();
        unique_owner(&mut names, "example", Path::new("one")).unwrap();
        assert!(
            unique_owner(&mut names, "example", Path::new("two"))
                .unwrap_err()
                .contains("duplicate wire contract example")
        );
        unique_owner(&mut names, "another", Path::new("two")).unwrap();
    }
    #[test]
    fn moving_a_contract_preserves_baseline_checks_and_removal_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        std::fs::create_dir_all(root.join("crates/tools/rsi-xtask")).unwrap();
        std::fs::create_dir(root.join("hooks")).unwrap();
        std::fs::write(root.join("crates/tools/rsi-xtask/Cargo.toml"), "").unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(root.join("Cargo.lock"), "version=4\n").unwrap();
        for name in ["first", "second"] {
            let owner = root.join("apps").join(name);
            std::fs::create_dir_all(owner.join("src")).unwrap();
            std::fs::write(
                owner.join("Cargo.toml"),
                format!("[package]\nname='{name}'\nversion='0.1.0'\n"),
            )
            .unwrap();
            std::fs::write(owner.join("src/lib.rs"), "").unwrap();
            std::fs::write(
                owner.join("wire-contract.toml"),
                format!("name='{name}'\nversion=1\n"),
            )
            .unwrap();
        }
        run(root, &["--write".into()]).unwrap();
        git(root, &["init", "--quiet"]).unwrap();
        git(
            root,
            &[
                "config",
                "core.hooksPath",
                root.join("hooks").to_str().unwrap(),
            ],
        )
        .unwrap();
        git(root, &["add", "."]).unwrap();
        git(
            root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--no-gpg-sign",
                "-qm",
                "baseline",
            ],
        )
        .unwrap();
        std::fs::rename(root.join("apps/first"), root.join("apps/moved")).unwrap();
        run(root, &["--base".into(), "HEAD".into()]).unwrap();
        std::fs::write(
            root.join("apps/moved/src/lib.rs"),
            "pub const WIRE_LIMIT: u32 = 2;\n",
        )
        .unwrap();
        let error = run(root, &["--base".into(), "HEAD".into(), "--write".into()]).unwrap_err();
        assert!(error.contains("without increasing"), "{error}");
        std::fs::remove_file(root.join("apps/moved/wire-contract.toml")).unwrap();
        let error = run(root, &["--base".into(), "HEAD".into()]).unwrap_err();
        assert!(
            error.contains("baseline wire contract first was removed"),
            "{error}"
        );
    }
    #[test]
    fn schema_changes_require_a_strict_version_increase() {
        let snapshot = |version, model| {
            serde_json::to_vec(&serde_json::json!({"format":1,"contract":{"name":"example","version":version},"models":model})).unwrap()
        };
        let original = snapshot(1, "name: string");
        assert!(require_version_increase(&original, &original).is_ok());
        assert!(require_version_increase(&original, &snapshot(1, "name: integer")).is_err());
        assert!(require_version_increase(&original, &snapshot(2, "name: integer")).is_ok());
        assert!(require_version_increase(&snapshot(2, "name: integer"), &original).is_err());
        assert!(
            require_version_increase(&original, br#"{"format":3,"contract":{"version":2}}"#)
                .is_err()
        );
    }
}
