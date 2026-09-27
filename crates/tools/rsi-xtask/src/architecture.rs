//! Source ownership checks, including maintained standalone Cargo workspaces.
use std::{fs, path::Path};

pub fn run(root: &Path) -> Result<(), String> {
    crate::require_repository_root(root)?;
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let apps = root
        .join("apps")
        .canonicalize()
        .map_err(|error| format!("{}: {error}", root.join("apps").display()))?;
    if !apps.starts_with(&root) {
        return Err("application root escapes repository".into());
    }
    let mut errors = Vec::new();
    let mut manifests = Vec::new();
    visit(
        &root.join("crates"),
        &root,
        &mut std::collections::BTreeSet::new(),
        &mut manifests,
        &mut errors,
    );
    for manifest in manifests {
        if let Err(error) = check_manifest(&root, &apps, &manifest, &mut errors) {
            errors.push(error);
        }
    }
    errors.sort();
    errors.dedup();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("verify-architecture:\n{}", errors.join("\n")))
    }
}

fn check_manifest(
    root: &Path,
    apps: &Path,
    manifest: &Path,
    errors: &mut Vec<String>,
) -> Result<(), String> {
    let value = read(manifest)?;
    let (workspace_root, workspace) = owning_workspace(manifest, root, &value)?;
    let inherited = workspace
        .get("workspace")
        .and_then(|v| v.get("dependencies"));
    let mut check_path = |base: &Path, name: &str, declaration: &toml::Value| {
        if let Some(path) = declaration.get("path").and_then(toml::Value::as_str) {
            let resolved = match base.join(path).canonicalize() {
                Ok(path) => path,
                Err(error) => {
                    errors.push(format!("{}: {name}: {error}", manifest.display()));
                    return Ok(());
                }
            };
            if resolved.starts_with(apps) {
                errors.push(format!(
                    "{}: `{name}` points from crates to apps",
                    manifest
                        .strip_prefix(root)
                        .expect("repository manifest")
                        .display()
                ));
            }
        }
        Ok(())
    };
    dependencies(&value, &mut |name, dependency| {
        let (base, declaration) =
            if dependency.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
                (
                    workspace_root.as_path(),
                    inherited.and_then(|v| v.get(name)).unwrap_or(dependency),
                )
            } else {
                (manifest.parent().expect("manifest parent"), dependency)
            };
        check_path(base, name, declaration)
    })?;
    if let Some(registries) = workspace.get("patch").and_then(toml::Value::as_table) {
        for (registry, patches) in registries {
            if let Some(patches) = patches.as_table() {
                for (name, declaration) in patches {
                    check_path(
                        &workspace_root,
                        &format!("patch.{registry}.{name}"),
                        declaration,
                    )?;
                }
            }
        }
    }
    if let Some(replacements) = workspace.get("replace").and_then(toml::Value::as_table) {
        for (name, declaration) in replacements {
            check_path(&workspace_root, &format!("replace.{name}"), declaration)?;
        }
    }
    let base = manifest.parent().expect("manifest parent");
    if let Some(lib) = value.get("lib") {
        check_path(base, "lib", lib)?;
    }
    for kind in ["bin", "test", "bench", "example"] {
        if let Some(targets) = value.get(kind).and_then(toml::Value::as_array) {
            for target in targets {
                check_path(base, kind, target)?;
            }
        }
    }
    if let Some(build) = value
        .get("package")
        .and_then(|v| v.get("build"))
        .and_then(toml::Value::as_str)
    {
        let declaration = toml::Value::Table(toml::map::Map::from_iter([(
            "path".into(),
            toml::Value::String(build.into()),
        )]));
        check_path(base, "package.build", &declaration)?;
    }
    Ok(())
}

fn owning_workspace(
    manifest: &Path,
    root: &Path,
    value: &toml::Value,
) -> Result<(std::path::PathBuf, toml::Value), String> {
    let directory = manifest.parent().expect("manifest parent");
    if let Some(explicit) = value
        .get("package")
        .and_then(|p| p.get("workspace"))
        .and_then(toml::Value::as_str)
    {
        let path = directory
            .join(explicit)
            .canonicalize()
            .map_err(|e| e.to_string())?;
        return Ok((path.clone(), read(&path.join("Cargo.toml"))?));
    }
    for ancestor in directory
        .ancestors()
        .take_while(|path| path.starts_with(root))
    {
        let path = ancestor.join("Cargo.toml");
        if path.is_file() {
            let value = read(&path)?;
            if value.get("workspace").is_some() {
                return Ok((ancestor.to_owned(), value));
            }
        }
    }
    Err(format!("{} has no owning workspace", manifest.display()))
}

fn read(path: &Path) -> Result<toml::Value, String> {
    toml::from_str(&fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?)
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn visit(
    path: &Path,
    root: &Path,
    seen: &mut std::collections::BTreeSet<std::path::PathBuf>,
    manifests: &mut Vec<std::path::PathBuf>,
    errors: &mut Vec<String>,
) {
    let physical = match path.canonicalize() {
        Ok(path) if path.starts_with(root) => path,
        Ok(_) => {
            errors.push(format!(
                "{}: source directory escapes repository",
                path.display()
            ));
            return;
        }
        Err(error) => {
            errors.push(format!("{}: {error}", path.display()));
            return;
        }
    };
    if !seen.insert(physical.clone()) {
        return;
    }
    let entries = match fs::read_dir(&physical) {
        Ok(entries) => entries,
        Err(error) => {
            errors.push(format!("{}: {error}", path.display()));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        if matches!(
            entry.file_name().to_str(),
            Some("target" | "node_modules" | ".git")
        ) {
            continue;
        }
        let metadata = match fs::metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) => {
                errors.push(format!("{}: {error}", entry.path().display()));
                continue;
            }
        };
        if metadata.is_dir() {
            visit(&entry.path(), root, seen, manifests, errors);
        } else if metadata.is_file() && entry.file_name() == "Cargo.toml" {
            manifests.push(entry.path());
        }
    }
}

fn dependencies(
    value: &toml::Value,
    check: &mut impl FnMut(&str, &toml::Value) -> Result<(), String>,
) -> Result<(), String> {
    if let Some(table) = value.as_table() {
        for (key, value) in table {
            if matches!(
                key.as_str(),
                "dependencies" | "dev-dependencies" | "build-dependencies"
            ) {
                if let Some(deps) = value.as_table() {
                    for (name, dependency) in deps {
                        check(name, dependency)?;
                    }
                }
            } else if key != "workspace" {
                dependencies(value, check)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn rejects_reverse_edges_through_a_symlinked_application_root() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("crates/product/core")).unwrap();
        fs::create_dir_all(root.path().join("actual-applications/catalog")).unwrap();
        std::os::unix::fs::symlink("actual-applications", root.path().join("apps")).unwrap();
        fs::write(root.path().join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
        let manifest = root.path().join("crates/product/core/Cargo.toml");
        fs::write(
            &manifest,
            "[dependencies]\napp={path='../../../apps/catalog'}\n",
        )
        .unwrap();
        assert!(
            run(root.path())
                .unwrap_err()
                .contains("points from crates to apps")
        );
        fs::create_dir_all(root.path().join("appsfoo/catalog")).unwrap();
        fs::write(
            manifest,
            "[dependencies]\napp={path='../../../appsfoo/catalog'}\n",
        )
        .unwrap();
        run(root.path()).unwrap();
    }
    #[test]
    fn rejects_invocation_below_the_repository_root() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".agents/notes")).unwrap();
        fs::create_dir_all(root.path().join("crates/product")).unwrap();
        fs::write(root.path().join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
        assert_eq!(
            run(&root.path().join("crates/product")).unwrap_err(),
            "command must run from the repository root"
        );
    }
    #[test]
    fn rejects_all_reverse_dependency_kinds_in_standalone_packages() {
        for section in [
            "dependencies",
            "dev-dependencies",
            "build-dependencies",
            "target.'cfg(unix)'.dependencies",
        ] {
            let root = tempfile::tempdir().unwrap();
            fs::create_dir_all(root.path().join("crates/product/standalone")).unwrap();
            fs::create_dir_all(root.path().join("apps/catalog")).unwrap();
            fs::write(root.path().join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
            fs::write(
                root.path().join("crates/product/standalone/Cargo.toml"),
                format!("[workspace]\n[{section}]\napp={{path='../../../apps/catalog'}}\n"),
            )
            .unwrap();
            assert!(
                run(root.path())
                    .unwrap_err()
                    .contains("points from crates to apps")
            );
        }
    }
    #[test]
    fn rejects_workspace_patch_and_replace_of_consumed_packages() {
        for override_table in [
            "patch.crates-io",
            "patch.'https://example.invalid/registry'",
            "replace",
        ] {
            let root = tempfile::tempdir().unwrap();
            fs::create_dir_all(root.path().join("crates/product/core")).unwrap();
            fs::create_dir_all(root.path().join("apps/catalog")).unwrap();
            let key = if override_table == "replace" {
                "'library:1.0.0'"
            } else {
                "library"
            };
            fs::write(
                root.path().join("Cargo.toml"),
                format!(
                    "[workspace]\nmembers=[]\n[{override_table}]\n{key}={{path='apps/catalog'}}\n"
                ),
            )
            .unwrap();
            fs::write(
                root.path().join("crates/product/core/Cargo.toml"),
                "[dependencies]\nrenamed={package='library',version='1'}\n",
            )
            .unwrap();
            assert!(
                run(root.path())
                    .unwrap_err()
                    .contains("points from crates to apps")
            );
        }
    }
    #[test]
    fn rejects_package_path_includes_into_apps() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("crates/product/core")).unwrap();
        fs::create_dir_all(root.path().join("apps/catalog")).unwrap();
        fs::write(root.path().join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
        fs::write(
            root.path().join("crates/product/core/Cargo.toml"),
            "[lib]\npath='../../../apps/catalog/lib.rs'\n",
        )
        .unwrap();
        fs::write(root.path().join("apps/catalog/lib.rs"), "").unwrap();
        assert!(
            run(root.path())
                .unwrap_err()
                .contains("points from crates to apps")
        );
    }
    #[test]
    fn resolves_inherited_dependencies_from_the_workspace_root() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("crates/product/core")).unwrap();
        fs::create_dir_all(root.path().join("apps/catalog")).unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace.dependencies]\napp={path='apps/catalog'}\n",
        )
        .unwrap();
        fs::write(
            root.path().join("crates/product/core/Cargo.toml"),
            "[dependencies]\napp.workspace=true\n",
        )
        .unwrap();
        assert!(
            run(root.path())
                .unwrap_err()
                .contains("points from crates to apps")
        );
    }
    #[test]
    fn standalone_workspace_inheritance_does_not_fall_back_to_root() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("crates/product/standalone/member")).unwrap();
        fs::create_dir_all(root.path().join("apps/catalog")).unwrap();
        fs::write(root.path().join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
        fs::write(
            root.path().join("crates/product/standalone/Cargo.toml"),
            "[workspace.dependencies]\napp={path='../../../apps/catalog'}\n",
        )
        .unwrap();
        fs::write(
            root.path()
                .join("crates/product/standalone/member/Cargo.toml"),
            "[build-dependencies]\napp.workspace=true\n",
        )
        .unwrap();
        assert!(
            run(root.path())
                .unwrap_err()
                .contains("points from crates to apps")
        );
    }
    #[test]
    fn collects_bad_paths_and_reverse_dependencies_in_the_same_manifest() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("crates/p/lib")).unwrap();
        fs::create_dir_all(root.path().join("apps/catalog")).unwrap();
        fs::write(root.path().join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
        fs::write(
            root.path().join("crates/p/lib/Cargo.toml"),
            "[dependencies]\na={path='missing'}\nz={path='../../../apps/catalog'}\n",
        )
        .unwrap();
        let errors = run(root.path()).unwrap_err();
        assert!(errors.contains(": a:"));
        assert!(errors.contains("`z` points from crates to apps"));
    }
    #[cfg(unix)]
    #[test]
    fn follows_in_repository_symlink_directories_without_looping() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("crates/p")).unwrap();
        fs::create_dir_all(root.path().join("shared")).unwrap();
        fs::create_dir_all(root.path().join("apps/catalog")).unwrap();
        fs::write(root.path().join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
        fs::write(
            root.path().join("shared/Cargo.toml"),
            "[dependencies]\napp={path='../apps/catalog'}\n",
        )
        .unwrap();
        std::os::unix::fs::symlink("../../shared", root.path().join("crates/p/link")).unwrap();
        std::os::unix::fs::symlink("../crates", root.path().join("shared/loop")).unwrap();
        assert!(
            run(root.path())
                .unwrap_err()
                .contains("points from crates to apps")
        );
    }
}
