use super::{Path, Result, Value, fs, hash, hash_file, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
};

const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

// Outputs created inside the capture by the paired Web/Desktop stages.
const GENERATED: [&str; 2] = ["apps/web/node_modules", "apps/desktop/gen"];

pub(super) fn capture(root: &Path, destination: &Path, previous: Option<&Value>) -> Result<Value> {
    let result = std::process::Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .current_dir(root)
        .output()?;
    if !result.status.success() {
        return Err("source publication requires a Git checkout".into());
    }
    let mut names = String::from_utf8(result.stdout)?
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    let mut present = Vec::with_capacity(names.len());
    for name in names {
        match fs::symlink_metadata(root.join(&name)) {
            Ok(_) => present.push(name),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    capture_names(root, destination, &present, previous)
}
fn capture_names(
    root: &Path,
    destination: &Path,
    names: &[String],
    previous: Option<&Value>,
) -> Result<Value> {
    let root = root.canonicalize()?;
    fs::create_dir_all(destination)?;
    if fs::symlink_metadata(destination)?.is_symlink() {
        return Err("capture root cannot be a link".into());
    }
    if let Some(previous) = previous.and_then(Value::as_object) {
        let retained: BTreeSet<_> = names.iter().collect();
        for name in previous.keys().filter(|n| !retained.contains(n)) {
            valid_name(name)?;
            parents(destination, name)?;
            let path = destination.join(name);
            if fs::symlink_metadata(&path).is_ok() {
                writable(&path)?;
                fs::remove_file(path)?;
            }
        }
    }
    let mut records = BTreeMap::new();
    let mut stamps = BTreeMap::new();
    for name in names {
        valid_name(name)?;
        stamps.insert(name, stamp(&fs::symlink_metadata(root.join(name))?));
    }
    for name in names {
        valid_name(name)?;
        let path = root.join(name);
        let target = destination.join(name);
        if !path
            .parent()
            .ok_or("source parent missing")?
            .canonicalize()?
            .starts_with(&root)
        {
            return Err(format!("source parent escapes checkout: {name}").into());
        }
        let before = fs::symlink_metadata(&path)?;
        if stamps.get(name) != Some(&stamp(&before)) {
            return Err(format!("source changed before capture: {name}").into());
        }
        parents(destination, name)?;
        if before.is_symlink() {
            let link = fs::read_link(&path)?;
            if link.is_absolute() || !link_destination(&path)?.starts_with(&root) {
                return Err(format!("source symlink escapes capture: {name}").into());
            }
            if fs::read_link(&target).ok().as_ref() != Some(&link) {
                if fs::symlink_metadata(&target).is_ok() {
                    writable(&target)?;
                    fs::remove_file(&target)?;
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(&link, &target)?;
                #[cfg(windows)]
                {
                    if path.is_dir() {
                        std::os::windows::fs::symlink_dir(&link, &target)?;
                    } else {
                        std::os::windows::fs::symlink_file(&link, &target)?;
                    }
                }
            }
            records.insert(name.clone(), json!({"kind":"symlink","target":link}));
        } else if before.is_file() {
            if before.len() > MAX_SOURCE_BYTES {
                return Err(format!("source file exceeds 64 MiB: {name}").into());
            }
            let mut input = fs::File::open(&path)?;
            if stamp(&input.metadata()?) != stamp(&before) {
                return Err(format!("source changed while opening: {name}").into());
            }
            let (digest, bytes) = copy_source(&mut input, &target)?;
            let executable = is_executable(&before);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    &target,
                    fs::Permissions::from_mode(if executable { 0o555 } else { 0o444 }),
                )?;
            }
            records.insert(
                name.clone(),
                json!({"kind":"file","sha256":digest,"bytes":bytes,"executable":executable}),
            );
        } else {
            return Err(format!("unsupported source object: {name}").into());
        }
    }
    for (name, before) in stamps {
        if stamp(&fs::symlink_metadata(root.join(name))?) != before {
            return Err(format!("source changed during capture: {name}").into());
        }
        if records[name]["kind"] == "file"
            && hash_file(&root.join(name))? != records[name]["sha256"]
        {
            return Err(format!("source bytes changed during capture: {name}").into());
        }
    }
    Ok(serde_json::to_value(records)?)
}
fn copy_source(input: &mut impl Read, target: &Path) -> Result<(String, u64)> {
    let mut temporary =
        tempfile::NamedTempFile::new_in(target.parent().ok_or("missing capture parent")?)?;
    let mut digest = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let count = match input.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        bytes += count as u64;
        if bytes > MAX_SOURCE_BYTES {
            return Err(format!("source file exceeds 64 MiB: {}", target.display()).into());
        }
        digest.update(&buffer[..count]);
        temporary.write_all(&buffer[..count])?;
    }
    let digest = format!("{:x}", digest.finalize());
    if let Ok(info) = fs::symlink_metadata(target) {
        if info.is_symlink() {
            fs::remove_file(target)?;
        } else {
            if info.is_file() && info.len() == bytes && hash_file(target)? == digest {
                return Ok((digest, bytes));
            }
            writable(target)?;
        }
    }
    temporary.persist(target)?;
    Ok((digest, bytes))
}
fn link_destination(path: &Path) -> Result<std::path::PathBuf> {
    if let Ok(resolved) = path.canonicalize() {
        return Ok(resolved);
    }
    let link = fs::read_link(path)?;
    let mut unresolved = path.parent().ok_or("link parent missing")?.join(link);
    let mut suffix = Vec::new();
    loop {
        if let Ok(mut resolved) = unresolved.canonicalize() {
            for part in suffix.into_iter().rev() {
                resolved.push(part);
            }
            return Ok(resolved);
        }
        suffix.push(
            unresolved
                .file_name()
                .ok_or("unresolvable source link")?
                .to_owned(),
        );
        if !unresolved.pop() {
            return Err("unresolvable source link".into());
        }
    }
}
fn parents(root: &Path, name: &str) -> Result<()> {
    let mut parent = root.to_owned();
    for part in Path::new(name)
        .parent()
        .ok_or("capture parent missing")?
        .components()
    {
        parent.push(part);
        match fs::symlink_metadata(&parent) {
            Ok(info) if info.is_dir() && !info.is_symlink() => (),
            Ok(_) => return Err("capture parent must be an ordinary directory".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&parent)?,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
fn valid_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !Path::new(name)
            .components()
            .all(|p| matches!(p, std::path::Component::Normal(_)))
    {
        return Err("invalid captured source path".into());
    }
    if GENERATED
        .iter()
        .any(|generated| Path::new(name).starts_with(generated))
    {
        return Err(format!("source input uses reserved generated directory: {name}").into());
    }
    Ok(())
}
fn stamp(info: &fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        format!(
            "{}:{}:{}:{}:{}:{}:{}:{}",
            info.dev(),
            info.ino(),
            info.len(),
            info.mtime(),
            info.mtime_nsec(),
            info.ctime(),
            info.ctime_nsec(),
            info.mode()
        )
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        format!(
            "{}:{}:{}:{}",
            info.creation_time(),
            info.last_write_time(),
            info.file_size(),
            info.file_attributes()
        )
    }
    #[cfg(not(any(unix, windows)))]
    {
        format!("{:?}:{:?}:{}", info.created(), info.modified(), info.len())
    }
}
fn is_executable(info: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        info.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = info;
        false
    }
}
pub(super) fn put(path: &Path, bytes: &[u8]) -> Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|m| m.is_symlink()) {
        fs::remove_file(path)?;
    }
    if fs::metadata(path).is_ok_and(|info| info.is_file() && info.len() == bytes.len() as u64)
        && hash_file(path)? == hash(bytes)
    {
        return Ok(());
    }
    if path.exists() {
        writable(path)?;
    }
    fs::write(path, bytes)?;
    Ok(())
}
#[cfg_attr(windows, allow(clippy::permissions_set_readonly_false))] // Windows file attribute, never Unix permissions.
fn writable(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path)?.is_symlink() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    {
        let mut p = fs::metadata(path)?.permissions();
        p.set_readonly(false);
        fs::set_permissions(path, p)?;
    }
    Ok(())
}
#[cfg_attr(windows, allow(clippy::unnecessary_wraps))] // Unix permission changes can fail at this shared boundary.
pub(super) fn executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
pub(super) fn verify(root: &Path, records: &Value) -> Result<()> {
    let records = records.as_object().ok_or("invalid capture records")?;
    let mut directories = vec![root.to_owned()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            let name = path
                .strip_prefix(root)?
                .to_str()
                .ok_or("non-Unicode capture path")?
                .replace('\\', "/");
            if kind.is_dir() {
                if !GENERATED.contains(&name.as_str()) {
                    directories.push(path);
                }
            } else if !records.contains_key(&name) {
                return Err(format!("uncaptured source added: {name}").into());
            }
        }
    }
    for (name, record) in records {
        valid_name(name)?;
        let mut parent = root.to_owned();
        for part in Path::new(name)
            .parent()
            .ok_or("capture parent missing")?
            .components()
        {
            parent.push(part);
            let info = fs::symlink_metadata(&parent)?;
            if !info.is_dir() || info.is_symlink() {
                return Err("captured parent changed".into());
            }
        }
        let path = root.join(name);
        let info = fs::symlink_metadata(&path)?;
        if record["kind"] == "symlink" {
            if !info.is_symlink()
                || fs::read_link(path)?
                    != Path::new(record["target"].as_str().ok_or("missing link target")?)
            {
                return Err(format!("captured symlink changed: {name}").into());
            }
        } else if !info.is_file() || info.is_symlink() || hash_file(&path)? != record["sha256"] {
            return Err(format!("captured source bytes changed: {name}").into());
        }
        #[cfg(unix)]
        if record["kind"] == "file" {
            use std::os::unix::fs::PermissionsExt;
            let expected = if record["executable"] == true {
                0o555
            } else {
                0o444
            };
            if info.permissions().mode() & 0o777 != expected {
                return Err(format!("captured source mode changed: {name}").into());
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversized_source_is_refused_without_replacing_the_previous_capture() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("input");
        let output = root.path().join("output");
        fs::create_dir(&input).unwrap();
        fs::create_dir(&output).unwrap();
        fs::File::create(input.join("large"))
            .unwrap()
            .set_len(64 * 1024 * 1024 + 1)
            .unwrap();
        fs::write(output.join("large"), b"previous").unwrap();
        let error = capture_names(&input, &output, &["large".into()], None).unwrap_err();
        assert!(error.to_string().contains("64 MiB"), "{error}");
        assert_eq!(fs::read(output.join("large")).unwrap(), b"previous");
    }

    #[test]
    fn streaming_growth_limit_preserves_destination_and_cleans_temporary_file() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("source");
        fs::write(&target, b"previous").unwrap();
        let mut growing = std::io::repeat(b'x').take(MAX_SOURCE_BYTES + 1);
        assert!(
            copy_source(&mut growing, &target)
                .unwrap_err()
                .to_string()
                .contains("64 MiB")
        );
        assert_eq!(fs::read(&target).unwrap(), b"previous");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn streamed_capture_reuses_large_files_and_detects_same_length_changes() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("source");
        let mut bytes = vec![b'a'; 1024 * 1024];
        let (digest, count) = copy_source(&mut bytes.as_slice(), &target).unwrap();
        assert_eq!(digest, hash(&bytes));
        assert_eq!(count, bytes.len() as u64);
        let modified = fs::metadata(&target).unwrap().modified().unwrap();
        copy_source(&mut bytes.as_slice(), &target).unwrap();
        assert_eq!(fs::metadata(&target).unwrap().modified().unwrap(), modified);
        bytes[32_768] = b'b';
        copy_source(&mut bytes.as_slice(), &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), bytes);
    }

    #[test]
    fn unrelated_generated_directory_names_do_not_hide_new_source() {
        for name in [
            "target/new.rs",
            "nested/node_modules/new.rs",
            "nested/__pycache__/new.rs",
        ] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"uncaptured").unwrap();
            assert!(verify(root.path(), &json!({})).is_err(), "{name}");
        }
    }
    #[test]
    fn reserved_outputs_cannot_be_authored_capture_inputs() {
        let root = tempfile::tempdir().unwrap();
        for generated in GENERATED {
            let name = format!("{generated}/input.rs");
            let path = root.path().join(&name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"source").unwrap();
            let error = capture_names(root.path(), &root.path().join("capture"), &[name], None)
                .unwrap_err();
            assert!(error.to_string().contains("reserved generated directory"));
        }
    }
    #[test]
    fn capture_preserves_dirty_bytes_reuses_unchanged_files_and_rejects_new_inputs() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("input");
        let out = t.path().join("output");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("keep"), b"dirty\0bytes").unwrap();
        fs::write(root.join("removed"), b"old").unwrap();
        let first = capture_names(&root, &out, &["keep".into(), "removed".into()], None).unwrap();
        let time = fs::metadata(out.join("keep")).unwrap().modified().unwrap();
        let second = capture_names(&root, &out, &["keep".into()], Some(&first)).unwrap();
        assert_eq!(
            fs::metadata(out.join("keep")).unwrap().modified().unwrap(),
            time
        );
        assert!(!out.join("removed").exists());
        verify(&out, &second).unwrap();
        fs::write(out.join("added.rs"), b"injected").unwrap();
        assert!(verify(&out, &second).is_err());
        fs::remove_file(out.join("added.rs")).unwrap();
        put(&out.join("keep"), b"changed").unwrap();
        assert!(verify(&out, &second).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn symlink_targets_must_stay_in_source() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("input");
        fs::create_dir(&root).unwrap();
        fs::write(t.path().join("outside"), b"no").unwrap();
        std::os::unix::fs::symlink("../outside", root.join("escape")).unwrap();
        assert!(capture_names(&root, &t.path().join("output"), &["escape".into()], None).is_err());
    }
    #[test]
    fn captures_git_dirty_and_untracked_sources_but_not_ignored_build_output() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("input");
        fs::create_dir(&root).unwrap();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(&root)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        git(&["init", "-q"]);
        fs::write(root.join(".gitignore"), "ignored\n").unwrap();
        fs::write(root.join("tracked"), "original").unwrap();
        fs::write(root.join("deleted"), "removed from working tree").unwrap();
        git(&["add", "."]);
        fs::remove_file(root.join("deleted")).unwrap();
        fs::write(root.join("tracked"), "dirty").unwrap();
        fs::write(root.join("new"), "untracked").unwrap();
        fs::write(root.join("ignored"), "build").unwrap();
        let out = t.path().join("output");
        let files = capture(&root, &out, None).unwrap();
        assert_eq!(fs::read_to_string(out.join("tracked")).unwrap(), "dirty");
        assert_eq!(fs::read_to_string(out.join("new")).unwrap(), "untracked");
        assert!(!out.join("ignored").exists());
        assert!(!out.join("deleted").exists());
        verify(&out, &files).unwrap();
        fs::create_dir_all(out.join("apps/desktop/gen")).unwrap();
        fs::write(out.join("apps/desktop/gen/schema.json"), "{}").unwrap();
        verify(&out, &files).unwrap();
        fs::remove_file(out.join("new")).unwrap();
        assert!(verify(&out, &files).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn internal_links_modes_and_capture_parent_replacement_are_checked() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("input");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/run"), "script").unwrap();
        fs::set_permissions(root.join("sub/run"), fs::Permissions::from_mode(0o755)).unwrap();
        symlink("sub/run", root.join("alias")).unwrap();
        symlink("ignored/missing", root.join("missing")).unwrap();
        let alias = t.path().join("checkout-link");
        symlink(&root, &alias).unwrap();
        let out = t.path().join("output");
        let names = ["sub/run".into(), "alias".into(), "missing".into()];
        let files = capture_names(&alias, &out, &names, None).unwrap();
        verify(&out, &files).unwrap();
        assert_eq!(
            fs::read_link(out.join("alias")).unwrap(),
            Path::new("sub/run")
        );
        fs::set_permissions(out.join("sub/run"), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(verify(&out, &files).is_err());
        fs::rename(out.join("sub"), out.join("old")).unwrap();
        symlink(root.join("sub"), out.join("sub")).unwrap();
        assert!(capture_names(&root, &out, &names, Some(&files)).is_err());
        assert!(verify(&out, &files).is_err());
        assert_eq!(fs::read_to_string(root.join("sub/run")).unwrap(), "script");
    }
}
