use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub(crate) fn run(repository: &Path, arguments: &[String]) -> Result<(), String> {
    crate::repository_root::require(repository, "verify-protocols")?;
    if !arguments.is_empty() {
        return Err("usage: verify-protocols".into());
    }
    verify(repository)
}
fn verify(repository: &Path) -> Result<(), String> {
    let mut owners = Vec::new();
    for root in ["crates", "apps", "fixtures"] {
        find(&repository.join(root), &mut owners)?;
    }
    owners.sort();
    if owners.is_empty() {
        return Err("no wire contracts were discovered".into());
    }
    let mut names = BTreeMap::new();
    let mut errors = Vec::new();
    for owner in owners {
        let contract = match rsi_api_contract_build::read(&owner) {
            Ok(contract) => contract,
            Err(error) => {
                errors.push(format!("{}: {error}", owner.display()));
                continue;
            }
        };
        if let Some(previous) = names.insert(contract.name.clone(), owner.clone()) {
            errors.push(format!(
                "duplicate wire contract {}: {} and {}",
                contract.name,
                previous.display(),
                owner.display()
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}
fn find(root: &Path, owners: &mut Vec<PathBuf>) -> Result<(), String> {
    if !root.exists() {
        return Ok(());
    }
    if root.join("wire-contract.json").is_file() {
        owners.push(root.to_owned());
    }
    for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_dir()
            && !matches!(
                entry.file_name().to_str(),
                Some("target" | "node_modules" | ".git" | "dist" | "release")
            )
        {
            find(&entry.path(), owners)?;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn nested_fixture_owners_and_invalid_owner_paths_are_checked() {
        let root = tempfile::tempdir().unwrap();
        for name in ["fixtures/outer", "fixtures/outer/nested"] {
            let owner = root.path().join(name);
            std::fs::create_dir_all(&owner).unwrap();
            std::fs::write(
                owner.join("wire-contract.json"),
                br#"{"name":"rsi.test","version":1}"#,
            )
            .unwrap();
        }
        assert!(
            super::verify(root.path())
                .unwrap_err()
                .contains("duplicate wire contract")
        );
        let nested = root.path().join("fixtures").join("outer").join("nested");
        std::fs::write(
            nested.join("wire-contract.json"),
            br#"{"name":"invalid","version":0}"#,
        )
        .unwrap();
        let error = super::verify(root.path()).unwrap_err();
        assert!(error.contains(nested.to_str().unwrap()), "{error}");
    }
    #[test]
    fn independent_errors_are_collected_and_generated_release_trees_are_ignored() {
        let root = tempfile::tempdir().unwrap();
        for (name, bytes) in [
            ("a", "bad json"),
            ("b", r#"{"name":"rsi.good","version":1}"#),
            ("c", r#"{"name":"rsi.good","version":1}"#),
            ("d", r#"{"name":"rsi.bad","version":0}"#),
            ("release/generated", "bad json"),
        ] {
            let owner = root.path().join("apps").join(name);
            std::fs::create_dir_all(&owner).unwrap();
            std::fs::write(owner.join("wire-contract.json"), bytes).unwrap();
        }
        let error = super::verify(root.path()).unwrap_err();
        let lines: Vec<_> = error.lines().collect();
        assert_eq!(lines.len(), 3, "{error}");
        assert!(lines[0].contains(root.path().join("apps").join("a").to_str().unwrap()));
        assert!(lines[1].contains("duplicate wire contract"));
        assert!(lines[2].contains(root.path().join("apps").join("d").to_str().unwrap()));
        assert!(!error.contains("generated"));
    }
    #[test]
    fn duplicate_names_have_one_owner() {
        let root = tempfile::tempdir().unwrap();
        for name in ["one", "two"] {
            let owner = root.path().join("apps").join(name);
            std::fs::create_dir_all(&owner).unwrap();
            std::fs::write(
                owner.join("wire-contract.json"),
                br#"{"name":"rsi.test","version":1}"#,
            )
            .unwrap();
        }
        assert!(
            super::verify(root.path())
                .unwrap_err()
                .contains("duplicate wire contract")
        );
    }
}
