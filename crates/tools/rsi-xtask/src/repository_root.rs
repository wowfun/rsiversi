use std::path::Path;

pub(crate) fn require(repository: &Path, command: &str) -> Result<(), String> {
    if repository.join("Cargo.toml").is_file()
        && repository
            .join("crates/tools/rsi-xtask/Cargo.toml")
            .is_file()
    {
        Ok(())
    } else {
        Err(format!("{command} must run from the repository root"))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn accepts_only_a_directory_with_both_repository_markers() {
        let repository = tempfile::tempdir().unwrap();
        fs::write(repository.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        assert_eq!(
            require(repository.path(), "product command"),
            Err("product command must run from the repository root".to_owned())
        );

        let tool_manifest = repository.path().join("crates/tools/rsi-xtask/Cargo.toml");
        fs::create_dir_all(tool_manifest.parent().unwrap()).unwrap();
        fs::write(tool_manifest, "[package]\nname = \"rsi-xtask\"\n").unwrap();
        assert_eq!(require(repository.path(), "product command"), Ok(()));
    }

    #[test]
    fn preserves_the_callers_command_name_in_failures() {
        let repository = tempfile::tempdir().unwrap();
        assert_eq!(
            require(repository.path(), "custom command"),
            Err("custom command must run from the repository root".to_owned())
        );
    }
}
