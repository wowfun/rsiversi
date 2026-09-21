use std::{
    fs,
    path::{Component, Path},
};

const MANIFEST: &str = include_str!("../../../../fixtures/rsi/addon-template/Cargo.toml");
const LOCK: &str = include_str!("../../../../fixtures/rsi/addon-template/Cargo.lock");
const SOURCE: &str = include_str!("../../../../fixtures/rsi/addon-template/src/lib.rs");
const LINKED_MANIFEST: &str =
    include_str!("../../../../fixtures/rsi/addon-linked-template/Cargo.toml");
const LINKED_LOCK: &str = include_str!("../../../../fixtures/rsi/addon-linked-template/Cargo.lock");
const LINKED_SOURCE: &str =
    include_str!("../../../../fixtures/rsi/addon-linked-template/src/lib.rs");
const LINKED_MAIN: &str =
    include_str!("../../../../fixtures/rsi/addon-linked-template/src/main.rs");

pub(crate) fn run(arguments: &[String]) -> Result<(), String> {
    if let [command, name, flag, directory, kind, linked] = arguments
        && command == "new"
        && flag == "--directory"
        && kind == "--kind"
        && linked == "linked"
    {
        let repository = std::env::current_dir().map_err(|e| e.to_string())?;
        super::repository_root::require(&repository, "addon new")?;
        return generate_linked(name, Path::new(directory));
    }
    let [command, name, flag, directory] = arguments else {
        return Err(
            "usage: cargo xtask addon new NAME --directory ABSOLUTE_NEW_DIRECTORY [--kind linked]"
                .into(),
        );
    };
    if command != "new" || flag != "--directory" {
        return Err("usage: cargo xtask addon new NAME --directory ABSOLUTE_NEW_DIRECTORY".into());
    }
    let repository = std::env::current_dir().map_err(|e| e.to_string())?;
    super::repository_root::require(&repository, "addon new")?;
    generate(&repository, name, Path::new(directory))
}

fn generate_linked(name: &str, directory: &Path) -> Result<(), String> {
    validate_destination(name, directory)?;
    let temporary = tempfile::Builder::new()
        .prefix(".rsi-addon-")
        .tempdir_in(directory.parent().ok_or("destination parent required")?)
        .map_err(|error| error.to_string())?;
    let root = temporary.path();
    let package = format!("rsi-addon-{name}");
    let mut manifest: toml::Value =
        toml::from_str(LINKED_MANIFEST).map_err(|error| error.to_string())?;
    *template_field(&mut manifest, &["package", "name"])? = toml::Value::String(package.clone());
    write(
        root,
        "Cargo.toml",
        &toml::to_string_pretty(&manifest).map_err(|error| error.to_string())?,
    )?;
    write(
        root,
        "Cargo.lock",
        &rename_lock_from(LINKED_LOCK, "rsi-addon-linked-template", &package)?,
    )?;
    fs::create_dir(root.join("src")).map_err(|error| error.to_string())?;
    write(
        root,
        "src/lib.rs",
        &LINKED_SOURCE.replace("addon.linked", &format!("addon.{name}")),
    )?;
    write(
        root,
        "src/main.rs",
        &LINKED_MAIN.replace("rsi_addon_linked_template", &package.replace('-', "_")),
    )?;
    write(
        root,
        "README.md",
        &format!(
            "# {package}\n\nAn independent source addon library and composition executable.\n\nRun `cargo test --locked` and `cargo run --locked`. Cargo fetches the one pinned\nRSI Git revision; later builds can use `--offline` with a populated cache.\nSource changes take effect on rebuild. The library demonstrates public addon\nroles and Local contracts; the executable owns its composition and shutdown.\nNo installation, user state or provider credentials are required.\n"
        ),
    )?;
    publish(root, directory)
}

fn generate(repository: &Path, name: &str, directory: &Path) -> Result<(), String> {
    validate_destination(name, directory)?;
    let parent = directory.parent().ok_or("destination parent required")?;
    let temporary = tempfile::Builder::new()
        .prefix(".rsi-addon-")
        .tempdir_in(parent)
        .map_err(|e| format!("create private sibling: {e}"))?;
    let root = temporary.path();
    let package = format!("rsi-addon-{name}");
    let plugin = format!("addon.{name}");
    let service = format!("{plugin}.tools");
    let mut manifest: toml::Value = toml::from_str(MANIFEST).map_err(|e| e.to_string())?;
    *template_field(&mut manifest, &["package", "name"])? = toml::Value::String(package.clone());
    for (dependency, relative) in [
        ("rsi-meta-native", "crates/rsi-meta/native"),
        ("rsi-tools-protocol", "crates/rsi-tools/protocol"),
    ] {
        let absolute = repository
            .join(relative)
            .canonicalize()
            .map_err(|e| e.to_string())?;
        *template_field(&mut manifest, &["dependencies", dependency, "path"])? =
            toml::Value::String(absolute.to_str().ok_or("SDK path must be UTF-8")?.into());
    }
    write(
        root,
        "Cargo.toml",
        &toml::to_string_pretty(&manifest).map_err(|e| e.to_string())?,
    )?;
    write(root, "Cargo.lock", &rename_lock(LOCK, &package)?)?;
    fs::create_dir(root.join("src")).map_err(|e| e.to_string())?;
    write(
        root,
        "src/lib.rs",
        &SOURCE
            .replace("addon.template", &plugin)
            .replace("addon_echo", &format!("{}_echo", name.replace('-', "_"))),
    )?;
    let artifact = format!(
        "target/debug/{}{}{}",
        std::env::consts::DLL_PREFIX,
        package.replace('-', "_"),
        std::env::consts::DLL_SUFFIX
    );
    write_toml(
        root,
        "addon.toml",
        &serde_json::json!({
            "format":2, "id":plugin, "plugin":plugin, "scope":"agent",
            "target":format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            "artifact":artifact, "portable_services":[service],
            "build":{"command":["cargo", "build", "--locked", "--target-dir", "target"],
                "watch":["Cargo.toml", "Cargo.lock", "src"], "timeout_seconds":600}
        }),
    )?;
    write_toml(
        root,
        "agent-profile.toml",
        &serde_json::json!({"steps":[
            {"kind":"plugin", "id":name, "plugin":plugin, "config":{"label":name}},
            {"kind":"plugin", "id":format!("{name}-bridge"), "plugin":"rsi.tools.portable", "config":{"service":service}}
        ]}),
    )?;
    write(
        root,
        "README.md",
        &format!(
            "# {package}\n\nA native Portable Tool addon using the RSI safe SDK.\n\nFrom this directory run `cargo build --locked --offline --target-dir target` with\ncached dependencies (omit `--offline` to fetch them). The addon manifest build\nallows fetching dependencies so installation also works with a cold Cargo cache. Then run\n`rsi addon install addon.toml` and `rsi addon enable {plugin}`.\nAppend the `[[steps]]` from `agent-profile.toml` to an Agent preset; the snippet\nis not a complete preset. Start a new Session using that preset.\n\nSDK dependency paths in Cargo.toml point to the checkout used for generation.\nMoving or deleting that checkout requires updating the paths. Generation itself\ndoes not install, enable, or edit a preset. The addon manifest build watch uses\nthe existing product watcher; a failed build retains the last usable generation.\n"
        ),
    )?;
    publish(root, directory)
}

fn rename_lock(source: &str, package: &str) -> Result<String, String> {
    rename_lock_from(source, "rsi-addon-template", package)
}

fn rename_lock_from(source: &str, original: &str, package: &str) -> Result<String, String> {
    let mut lock: toml::Value = toml::from_str(source).map_err(|e| e.to_string())?;
    let entries = lock
        .get_mut("package")
        .and_then(toml::Value::as_array_mut)
        .ok_or("template lock has no packages")?;
    let mut matches = entries
        .iter_mut()
        .filter(|entry| entry["name"].as_str() == Some(original) && entry.get("source").is_none());
    let root = matches.next().ok_or("template lock root is absent")?;
    if matches.next().is_some() {
        return Err("template lock has duplicate roots".into());
    }
    root["name"] = toml::Value::String(package.into());
    toml::to_string_pretty(&lock).map_err(|e| e.to_string())
}

fn validate_destination(name: &str, directory: &Path) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 48
        || !name.as_bytes()[0].is_ascii_lowercase()
        || name.split('-').any(str::is_empty)
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(
            "NAME must be 1..48 lowercase ASCII letters or digits in nonempty hyphen-separated segments, starting with a letter"
                .into(),
        );
    }
    if !directory.is_absolute()
        || directory.components().any(|c| c == Component::ParentDir)
        || directory.file_name().is_none()
        || directory
            .to_str()
            .is_none_or(|path| path.chars().any(char::is_control))
    {
        return Err(
            "destination must be an absolute UTF-8 new directory without parent traversal or control characters".into(),
        );
    }
    if !cfg!(target_os = "linux") {
        return Err("addon new requires Linux or WSL".into());
    }
    if directory.symlink_metadata().is_ok() {
        return Err("destination already exists".into());
    }
    Ok(())
}

fn write(root: &Path, name: &str, contents: &str) -> Result<(), String> {
    fs::write(root.join(name), contents).map_err(|e| format!("write {name}: {e}"))
}
fn write_toml(root: &Path, name: &str, value: &serde_json::Value) -> Result<(), String> {
    write(
        root,
        name,
        &toml::to_string_pretty(value).map_err(|e| e.to_string())?,
    )
}
#[cfg(target_os = "linux")]
fn publish(source: &Path, destination: &Path) -> Result<(), String> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        source,
        rustix::fs::CWD,
        destination,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(|e| format!("publish new addon: {e}"))
}
#[cfg(not(target_os = "linux"))]
fn publish(_: &Path, _: &Path) -> Result<(), String> {
    Err("addon new requires Linux or WSL".into())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn linked_scaffold_uses_one_immutable_git_sdk_and_its_own_launcher() {
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("linked 空间");
        generate_linked("linked-probe", &destination).unwrap();
        let manifest: toml::Value =
            toml::from_str(&fs::read_to_string(destination.join("Cargo.toml")).unwrap()).unwrap();
        let dependencies = manifest["dependencies"].as_table().unwrap();
        for (name, dependency) in dependencies
            .iter()
            .filter(|(name, _)| name.starts_with("rsi"))
        {
            assert_eq!(
                dependency["git"].as_str(),
                Some("https://github.com/wowfun/rsiversi.git"),
                "{name}"
            );
            let revision = dependency["rev"].as_str().unwrap();
            assert_eq!(revision.len(), 40);
            assert!(revision.bytes().all(|byte| byte.is_ascii_hexdigit()));
            assert!(dependency.get("path").is_none());
        }
        assert!(
            fs::read_to_string(destination.join("src/main.rs"))
                .unwrap()
                .contains("rsi_addon_linked_probe")
        );
        assert!(
            !fs::read_to_string(destination.join("src/lib.rs"))
                .unwrap()
                .contains("#[path")
        );
        let lock: toml::Value =
            toml::from_str(&fs::read_to_string(destination.join("Cargo.lock")).unwrap()).unwrap();
        assert!(
            lock["package"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["name"].as_str() == Some("rsi-addon-linked-probe"))
        );
        assert!(generate_linked("linked-probe", &destination).is_err());
    }

    #[test]
    fn lock_rewrite_requires_one_root_and_preserves_other_values() {
        assert!(rename_lock("version = 4", "new").is_err());
        assert!(rename_lock("[[package]]\nname = 'rsi-addon-template'\n[[package]]\nname = 'rsi-addon-template'", "new").is_err());
        let result = rename_lock("[[package]]\nname = 'rsi-addon-template'\n[[package]]\nname = 'rsi-addon-template'\nsource = 'registry+https://example.test'", "new").unwrap();
        let value: toml::Value = toml::from_str(&result).unwrap();
        assert_eq!(value["package"][0]["name"].as_str(), Some("new"));
        assert_eq!(
            value["package"][1]["name"].as_str(),
            Some("rsi-addon-template")
        );
    }
    #[test]
    fn complete_workspace_is_published_once_with_literal_paths_and_pinned_graph() {
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("space 引号 \" $(`x`) addon");
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()
            .unwrap();
        generate(&repository, "example-1", &destination).unwrap();
        let manifest: toml::Value =
            toml::from_str(&fs::read_to_string(destination.join("Cargo.toml")).unwrap()).unwrap();
        assert_eq!(
            manifest["package"]["name"].as_str(),
            Some("rsi-addon-example-1")
        );
        assert_eq!(
            manifest["dependencies"]["rsi-meta-native"]["path"].as_str(),
            repository.join("crates/rsi-meta/native").to_str()
        );
        let generated = fs::read_to_string(destination.join("Cargo.lock")).unwrap();
        assert_eq!(
            toml::from_str::<toml::Value>(
                &generated.replace("rsi-addon-example-1", "rsi-addon-template")
            )
            .unwrap(),
            toml::from_str::<toml::Value>(LOCK).unwrap()
        );
        assert!(
            fs::read_to_string(destination.join("agent-profile.toml"))
                .unwrap()
                .contains("[[steps]]")
        );
        assert!(generate(&repository, "example-1", &destination).is_err());
        for invalid in ["", "../../x", "Upper", "x_y", "a\n", "a.b", "foo-", "a--b"] {
            assert!(generate(&repository, invalid, &parent.path().join("invalid")).is_err());
        }
        for path in ["line\nfeed", "tab\tname", "escape\x1b"] {
            assert!(generate(&repository, "example", &parent.path().join(path)).is_err());
        }
        let link = parent.path().join("link");
        std::os::unix::fs::symlink(parent.path().join("absent"), &link).unwrap();
        assert!(generate(&repository, "example", &link).is_err());
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert!(!fs::read_dir(parent.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".rsi-addon-")
        }));
    }

    #[test]
    fn publication_cannot_replace_a_destination_created_after_preflight() {
        let parent = tempfile::tempdir().unwrap();
        let source = parent.path().join("private");
        let destination = parent.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        assert!(publish(&source, &destination).is_err());
        assert!(source.is_dir());
        assert!(destination.is_dir());
    }
}

fn template_field<'a>(
    manifest: &'a mut toml::Value,
    path: &[&str],
) -> Result<&'a mut toml::Value, String> {
    let mut current = manifest;
    for key in path {
        current = current
            .get_mut(*key)
            .ok_or_else(|| format!("addon template is missing {}", path.join(".")))?;
    }
    if !current.is_str() {
        return Err(format!(
            "addon template {} must be a string",
            path.join(".")
        ));
    }
    Ok(current)
}

#[cfg(test)]
mod template_tests {
    #[test]
    fn changed_template_shape_is_a_diagnostic_not_a_panic() {
        for source in [
            "",
            "[dependencies]\nrsi-meta-native = '1'",
            "[dependencies.rsi-meta-native]\npath = 42",
        ] {
            let mut manifest = toml::from_str(source).unwrap();
            assert!(
                super::template_field(&mut manifest, &["dependencies", "rsi-meta-native", "path"])
                    .unwrap_err()
                    .contains("addon template")
            );
        }
    }
}
