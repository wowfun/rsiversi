use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use serde::Deserialize;

#[derive(Eq, PartialEq)]
struct Owner {
    package: String,
    contract: Contract,
}

#[derive(Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct Contract {
    #[serde(default)]
    target: Target,
    #[serde(default = "default_fixtures")]
    fixtures: PathBuf,
}

fn default_fixtures() -> PathBuf {
    "tests/fixtures/persistence".into()
}

impl Default for Target {
    fn default() -> Self {
        Self::Library {
            filter: "persistence".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Target {
    Integration { name: String },
    Library { filter: String },
}

impl Target {
    fn label(&self) -> &str {
        match self {
            Self::Integration { name } => name,
            Self::Library { filter } => filter,
        }
    }
    fn arguments(&self) -> [std::ffi::OsString; 2] {
        match self {
            Self::Integration { name } => ["--test".into(), name.into()],
            Self::Library { filter } => ["--lib".into(), filter.into()],
        }
    }
}

pub(crate) fn run(repository: &Path, contracts_only: bool) -> Result<(), String> {
    crate::repository_root::require(repository, "verify-persistence")?;
    if let Some(base) = std::env::var_os("RSI_PERSISTENCE_BASE") {
        retention(
            repository,
            base.to_str().ok_or("non-UTF-8 persistence base")?,
        )?;
    }
    let mut manifests = Vec::new();
    for root in ["crates", "apps"] {
        discover(&repository.join(root), &mut manifests)?;
    }
    manifests.sort();
    if manifests.is_empty() {
        return Err("no persistence owners found".into());
    }
    let mut failures = Vec::new();
    let mut targets: BTreeMap<Target, BTreeSet<String>> = BTreeMap::new();
    for manifest in manifests {
        let result = (|| {
            let owner = current_owner(&manifest)?;
            if !manifest
                .parent()
                .unwrap()
                .join(&owner.contract.fixtures)
                .is_dir()
            {
                return Err("missing retained fixture directory".to_owned());
            }
            targets
                .entry(owner.contract.target)
                .or_default()
                .insert(owner.package);
            Ok::<_, String>(())
        })();
        if let Err(error) = result {
            failures.push(format!("{}: {error}", manifest.display()));
        }
    }
    // One Cargo graph per target name avoids rebuilding common dependencies
    // under each owner's different dev-dependency feature selection.
    for (target, packages) in targets.into_iter().filter(|_| !contracts_only) {
        let mut arguments: Vec<std::ffi::OsString> = ["test", "--locked", "--no-fail-fast"]
            .map(Into::into)
            .into();
        for package in packages {
            arguments.extend(["-p".into(), package.into()]);
        }
        arguments.extend(target.arguments());
        if let Err(error) = crate::cargo_step::CargoStep::new(
            format!("durable formats ({})", target.label()),
            arguments,
        )
        .run(repository)
        {
            failures.push(error);
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

fn current_owner(manifest: &Path) -> Result<Owner, String> {
    read_owner(
        &std::fs::read(manifest).map_err(|e| e.to_string())?,
        &std::fs::read(manifest.with_file_name("Cargo.toml")).map_err(|e| e.to_string())?,
    )
}

fn read_owner(bytes: &[u8], cargo: &[u8]) -> Result<Owner, String> {
    let contract: Contract = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let manifest: toml::Value =
        toml::from_str(std::str::from_utf8(cargo).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let package = manifest
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(toml::Value::as_str)
        .ok_or("missing Cargo package name")?
        .to_owned();
    let owner = Owner { package, contract };
    if owner.package.is_empty()
        || !owner
            .package
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err("invalid persistence package identity".into());
    }
    let target = owner.contract.target.label();
    if target.is_empty()
        || !target
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':'))
    {
        return Err("invalid persistence test target".into());
    }
    if owner.contract.fixtures.as_os_str().is_empty()
        || owner
            .contract
            .fixtures
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err("fixture directory must be a relative descendant".into());
    }
    Ok(owner)
}

fn discover(root: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            if !matches!(
                entry.file_name().to_str(),
                Some("target" | "node_modules" | ".git" | "dist" | "release" | "fixtures")
            ) {
                discover(&entry.path(), out)?;
            }
        } else if entry.file_name() == "persistence-contract.json" {
            out.push(entry.path());
        }
    }
    Ok(())
}

fn git(repository: &Path, arguments: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .current_dir(repository)
        .args(arguments)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

struct GitObjects {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl GitObjects {
    fn open(repository: &Path) -> io::Result<Self> {
        let mut child = Command::new("git")
            .current_dir(repository)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        Ok(Self {
            input: child.stdin.take().expect("piped Git input"),
            output: BufReader::new(child.stdout.take().expect("piped Git output")),
            child,
        })
    }

    fn read(&mut self, object: &str) -> io::Result<Vec<u8>> {
        // Request object IDs, so filenames containing newlines cannot affect
        // batch framing. No checkout filters or text conversion run here.
        writeln!(self.input, "{object}")?;
        let mut header = String::new();
        self.output.read_line(&mut header)?;
        let mut fields = header.split_ascii_whitespace();
        if fields.next() != Some(object) || fields.next() != Some("blob") {
            return Err(io::Error::other(format!(
                "cannot read Git blob {object}: {header}"
            )));
        }
        let length: u64 = fields
            .next()
            .and_then(|size| size.parse().ok())
            .ok_or_else(|| io::Error::other("invalid Git blob length"))?;
        let mut bytes = Vec::new();
        self.output.by_ref().take(length).read_to_end(&mut bytes)?;
        let mut end = [0];
        self.output.read_exact(&mut end)?;
        if bytes.len() as u64 != length || end != *b"\n" {
            return Err(io::Error::other("incomplete Git blob"));
        }
        Ok(bytes)
    }
}

impl Drop for GitObjects {
    fn drop(&mut self) {
        // The interactive helper waits for another request; reap it on every
        // exit path, including malformed objects and missing worktree files.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn retention(repository: &Path, base: &str) -> Result<(), String> {
    if !matches!(base.len(), 40 | 64) || !base.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("RSI_PERSISTENCE_BASE must be an exact commit SHA".into());
    }
    // The initial push has no previous tree.
    if base.bytes().all(|b| b == b'0') {
        return Ok(());
    }
    let tree = git(repository, &["ls-tree", "-r", "-z", base])?;
    let entries: Vec<_> = tree
        .split(|b| *b == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let tab = entry
                .iter()
                .position(|b| *b == b'\t')
                .ok_or("invalid Git tree entry")?;
            let metadata = std::str::from_utf8(&entry[..tab]).map_err(|e| e.to_string())?;
            let object = metadata.rsplit(' ').next().ok_or("missing Git object ID")?;
            Ok::<_, String>((object, &entry[tab + 1..]))
        })
        .collect::<Result<_, _>>()?;
    let mut owners = entries
        .iter()
        .filter(|(_, path)| path.ends_with(b"/persistence-contract.json"))
        .peekable();
    if owners.peek().is_none() {
        return Ok(());
    }
    let mut objects = GitObjects::open(repository).map_err(|e| e.to_string())?;
    let mut failures = Vec::new();
    for (object, name) in owners {
        let name = std::str::from_utf8(name).map_err(|e| e.to_string())?;
        let cargo_path = Path::new(name)
            .with_file_name("Cargo.toml")
            .to_string_lossy()
            .replace('\\', "/");
        let cargo_object = entries
            .iter()
            .find(|(_, path)| *path == cargo_path.as_bytes())
            .ok_or_else(|| format!("missing retained Cargo manifest: {cargo_path}"))?
            .0;
        let old = read_owner(
            &objects.read(object).map_err(|e| e.to_string())?,
            &objects.read(cargo_object).map_err(|e| e.to_string())?,
        )?;
        let declaration = (|| {
            let current = current_owner(&repository.join(name))?;
            if old != current {
                return Err("package, target and fixture home must remain unchanged".into());
            }
            Ok::<_, String>(())
        })();
        if let Err(error) = declaration {
            failures.push(format!("retained persistence owner {name}: {error}"));
        }
        let prefix = Path::new(name)
            .parent()
            .unwrap()
            .join(old.contract.fixtures)
            .to_string_lossy()
            .replace('\\', "/")
            + "/";
        for (object, path) in entries
            .iter()
            .filter(|(_, path)| path.starts_with(prefix.as_bytes()))
        {
            let path = std::str::from_utf8(path).map_err(|e| e.to_string())?;
            if Path::new(path)
                .extension()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
            {
                continue;
            }
            let previous = objects.read(object).map_err(|e| e.to_string())?;
            match std::fs::read(repository.join(path)) {
                Ok(current) if previous != current => {
                    failures.push(format!("retained fixture changed: {path}"));
                }
                Err(error) => failures.push(format!("retained fixture {path}: {error}")),
                Ok(_) => {}
            }
        }
    }
    failures.sort();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_identity_comes_from_cargo_and_defaults_are_explicit() {
        let cargo = b"[package]\nname = 'store'\n";
        let owner = read_owner(b"{}", cargo).unwrap();
        assert_eq!(owner.package, "store");
        assert_eq!(owner.contract.target, Target::default());
        assert_eq!(owner.contract.fixtures, default_fixtures());
        for contract in [
            br#"{"package":"another-package"}"#.as_slice(),
            br#"{"fixtures":"../outside"}"#,
            br#"{"target":{"kind":"library","filter":""}}"#,
        ] {
            assert!(read_owner(contract, cargo).is_err());
        }
        assert!(read_owner(b"{}", b"[workspace]").is_err());
    }

    #[test]
    fn retention_allows_additions_and_rejects_replacement_or_removal() {
        for object_format in ["sha1", "sha256"] {
            verify_retention(object_format);
        }
    }

    fn verify_retention(object_format: &str) {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        git(
            root,
            &[
                "init",
                "--quiet",
                &format!("--object-format={object_format}"),
            ],
        )
        .unwrap();
        std::fs::create_dir_all(root.join("crates/store/corpus")).unwrap();
        let manifest = root.join("crates/store/persistence-contract.json");
        std::fs::write(
            &manifest,
            br#"{"target":{"kind":"integration","name":"persistence"},"fixtures":"corpus"}"#,
        )
        .unwrap();
        let cargo = root.join("crates/store/Cargo.toml");
        let cargo_bytes = "[package]\nname = 'store'\n";
        std::fs::write(&cargo, cargo_bytes).unwrap();
        let procedure = root.join("crates/store/corpus/writer.md");
        std::fs::write(&procedure, "Capture procedure").unwrap();
        let fixture = root.join("crates/store/corpus/old.bin");
        std::fs::write(&fixture, b"original").unwrap();
        // Multiple blobs and binary framing exercise the retained byte stream.
        let binary: Vec<u8> = (0..=255).cycle().take(256 * 1024).collect();
        let binary_path = root.join("crates/store/corpus/binary with spaces.bin");
        std::fs::write(&binary_path, &binary).unwrap();
        for index in 0..100 {
            std::fs::write(
                root.join(format!("crates/store/corpus/part-{index}.bin")),
                [0, index],
            )
            .unwrap();
        }
        #[cfg(unix)]
        std::fs::write(root.join("crates/store/corpus/line\nbreak.bin"), b"\0\r\n").unwrap();
        let base = commit_fixture(root);
        let base = base.trim();
        std::fs::write(&procedure, "Corrected capture procedure").unwrap();
        retention(root, base).expect("prose is not immutable writer evidence");
        std::fs::remove_file(&procedure).unwrap();
        retention(root, base).expect("procedures can move outside the corpus");
        std::fs::write(root.join("crates/store/corpus/new.bin"), b"additional").unwrap();
        retention(root, base).unwrap();
        std::fs::write(&cargo, cargo_bytes.replace("store", "elsewhere")).unwrap();
        assert!(
            retention(root, base)
                .unwrap_err()
                .contains("retained persistence owner")
        );
        std::fs::write(&cargo, cargo_bytes).unwrap();
        let declaration = std::fs::read_to_string(&manifest).unwrap();
        for (from, to) in [
            ("\"corpus\"", "\"elsewhere\""),
            ("\"persistence\"", "\"unrelated\""),
        ] {
            std::fs::write(&manifest, declaration.replace(from, to)).unwrap();
            assert!(
                retention(root, base)
                    .unwrap_err()
                    .contains("retained persistence owner")
            );
        }
        std::fs::write(&manifest, &declaration).unwrap();
        std::fs::write(&fixture, b"replacement").unwrap();
        let mut changed_binary = binary.clone();
        *changed_binary.last_mut().unwrap() ^= 1;
        std::fs::write(&binary_path, changed_binary).unwrap();
        std::fs::write(
            &manifest,
            declaration.replace("\"corpus\"", "\"elsewhere\""),
        )
        .unwrap();
        let failures = retention(root, base).unwrap_err();
        assert!(failures.contains("retained persistence owner"));
        assert!(failures.contains("retained fixture changed"));
        assert!(failures.contains("binary with spaces.bin"));
        std::fs::write(&binary_path, &binary).unwrap();
        std::fs::write(&manifest, declaration).unwrap();
        assert!(
            retention(root, base)
                .unwrap_err()
                .contains("retained fixture changed")
        );
        std::fs::remove_file(&fixture).unwrap();
        assert!(
            retention(root, base)
                .unwrap_err()
                .contains("retained fixture")
        );
        std::fs::write(&fixture, b"original").unwrap();
        std::fs::remove_file(manifest).unwrap();
        assert!(
            retention(root, base)
                .unwrap_err()
                .contains("retained persistence owner")
        );
    }

    fn commit_fixture(root: &Path) -> String {
        git(root, &["-c", "core.autocrlf=false", "add", "."]).unwrap();
        git(
            root,
            &[
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=",
                "commit",
                "-qm",
                "baseline",
            ],
        )
        .unwrap();
        String::from_utf8(git(root, &["rev-parse", "HEAD"]).unwrap()).unwrap()
    }
}
