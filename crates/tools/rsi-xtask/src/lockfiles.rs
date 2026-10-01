use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process::Command,
};

pub(crate) fn verify(repository: &Path, online: bool) -> Result<(), String> {
    let paths = cargo_paths(repository)?;
    let mut roots = BTreeSet::new();
    let mut locks = BTreeSet::new();
    let mut errors = Vec::new();
    for path in paths.split('\0').filter(|path| !path.is_empty()) {
        let path = repository.join(path);
        if path.file_name().is_some_and(|name| name == "Cargo.lock") {
            locks.insert(path);
            continue;
        }
        if path.file_name().is_none_or(|name| name != "Cargo.toml") {
            continue;
        }
        let output = Command::new("cargo")
            .args([
                "locate-project",
                "--workspace",
                "--message-format",
                "plain",
                "--manifest-path",
            ])
            .arg(&path)
            .current_dir(repository)
            .output()
            .map_err(|e| e.to_string())?;
        if output.status.success() {
            roots.insert(
                PathBuf::from(
                    String::from_utf8(output.stdout)
                        .map_err(|error| error.to_string())?
                        .trim(),
                )
                .with_file_name("Cargo.lock"),
            );
        } else {
            errors.push(format!(
                "{}: {}",
                path.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
    }
    for lock in locks.difference(&roots) {
        errors.push(format!(
            "{}: orphan lockfile without a discovered workspace",
            lock.display()
        ));
    }
    for lock in &roots {
        if !locks.contains(lock) || !lock.is_file() {
            errors.push(format!(
                "{}: missing Git-managed workspace lockfile",
                lock.display()
            ));
            continue;
        }
        let mut command = Command::new("cargo");
        command
            .args([
                "metadata",
                "--locked",
                "--format-version",
                "1",
                "--manifest-path",
            ])
            .arg(lock.with_file_name("Cargo.toml"));
        if !online {
            command.arg("--offline");
        }
        let output = command
            .current_dir(repository)
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            errors.push(format!(
                "{}: {}",
                lock.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
    }
    errors.sort();
    errors.dedup();
    if errors.is_empty() {
        println!("verified {} workspace lockfiles", roots.len());
        Ok(())
    } else {
        let hint = if online {
            ""
        } else {
            "\nDependency fetching is disabled. If Cargo reports an uncached dependency, rerun `cargo xtask verify-lockfiles --online`; --locked still prevents lockfile changes."
        };
        Err(format!("verify-lockfiles:\n{}{hint}", errors.join("\n")))
    }
}

fn cargo_paths(repository: &Path) -> Result<String, String> {
    let output = Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            "Cargo.toml",
            "Cargo.lock",
            "**/Cargo.toml",
            "**/Cargo.lock",
        ])
        .current_dir(repository)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    String::from_utf8(output.stdout).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn discovers_member_roots_and_aggregates_stale_missing_and_orphan_locks_without_writes() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .arg("init")
                .arg(root.path())
                .output()
                .unwrap()
                .status
                .success()
        );
        for name in ["one", "two"] {
            let workspace = root.path().join(name);
            fs::create_dir_all(workspace.join("member/src")).unwrap();
            fs::write(
                workspace.join("Cargo.toml"),
                "[workspace]\nresolver='2'\nmembers=['member']\n",
            )
            .unwrap();
            fs::write(
                workspace.join("member/Cargo.toml"),
                format!("[package]\nname='{name}'\nversion='0.1.0'\n"),
            )
            .unwrap();
            fs::write(workspace.join("member/src/lib.rs"), "").unwrap();
            assert!(
                Command::new("cargo")
                    .args(["generate-lockfile", "--offline"])
                    .current_dir(&workspace)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
        verify(root.path(), false).unwrap();
        #[cfg(unix)]
        {
            let aliases = tempfile::tempdir().unwrap();
            let alias = aliases.path().join("repository");
            std::os::unix::fs::symlink(root.path(), &alias).unwrap();
            verify(&alias, false).unwrap();
        }
        // Staging does not change discovery of the same authored working tree.
        assert!(
            Command::new("git")
                .args(["add", "."])
                .current_dir(root.path())
                .status()
                .unwrap()
                .success()
        );
        verify(root.path(), false).unwrap();
        for name in ["one", "two"] {
            let manifest = root.path().join(name).join("member/Cargo.toml");
            fs::write(
                &manifest,
                fs::read_to_string(&manifest)
                    .unwrap()
                    .replace("0.1.0", "0.2.0"),
            )
            .unwrap();
        }
        let before = fs::read(root.path().join("one/Cargo.lock")).unwrap();
        let error = verify(root.path(), false).unwrap_err();
        assert!(error.contains("one/Cargo.lock") && error.contains("two/Cargo.lock"));
        assert!(error.contains("verify-lockfiles --online"));
        assert_eq!(
            fs::read(root.path().join("one/Cargo.lock")).unwrap(),
            before
        );
        fs::remove_file(root.path().join("two/Cargo.lock")).unwrap();
        fs::create_dir(root.path().join("orphan")).unwrap();
        fs::write(root.path().join("orphan/Cargo.lock"), "").unwrap();
        let error = verify(root.path(), false).unwrap_err();
        assert!(error.contains("missing Git-managed workspace lockfile"));
        assert!(error.contains("orphan lockfile"));
        assert!(!root.path().join("two/Cargo.lock").exists());
    }
}
