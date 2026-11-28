use super::{Path, Result, Value, fs, hash, hash_file, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Seek, Write},
    path::PathBuf,
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
    let paths = String::from_utf8(result.stdout)?;
    capture_names(root, destination, paths.split_terminator('\0'), previous)
}
fn capture_names<'a>(
    root: &Path,
    destination: &Path,
    names: impl IntoIterator<Item = &'a str>,
    previous: Option<&Value>,
) -> Result<Value> {
    let root = root.canonicalize()?;
    fs::create_dir_all(destination)?;
    if fs::symlink_metadata(destination)?.is_symlink() {
        return Err("capture root cannot be a link".into());
    }
    let destination = &destination.canonicalize()?;
    let mut stamps = BTreeMap::new();
    for name in names {
        let path = root.join(name);
        match fs::symlink_metadata(path) {
            Ok(info) => {
                valid_name(name)?;
                stamps.insert(name, stamp(&info));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    let mut verified_sources = BTreeSet::new();
    let mut verified_targets = BTreeSet::new();
    if let Some(previous) = previous.and_then(Value::as_object) {
        for name in previous.keys().filter(|n| !stamps.contains_key(n.as_str())) {
            valid_name(name)?;
            parents(destination, name, &mut verified_targets)?;
            let path = destination.join(name);
            if fs::symlink_metadata(&path).is_ok() {
                writable(&path)?;
                fs::remove_file(path)?;
            }
        }
    }
    let mut records = BTreeMap::new();
    for (&name, expected) in &stamps {
        let path = root.join(name);
        let target = destination.join(name);
        let source_parent = path.parent().ok_or("source parent missing")?;
        if !verified_sources.contains(source_parent) {
            if !source_parent.canonicalize()?.starts_with(&root) {
                return Err(format!("source parent escapes checkout: {name}").into());
            }
            verified_sources.insert(source_parent.to_owned());
        }
        let before = fs::symlink_metadata(&path)?;
        if *expected != stamp(&before) {
            return Err(format!("source changed before capture: {name}").into());
        }
        parents(destination, name, &mut verified_targets)?;
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
            records.insert(name, json!({"kind":"symlink","target":link}));
        } else if before.is_file() {
            if before.len() > MAX_SOURCE_BYTES {
                return Err(format!("source file exceeds 64 MiB: {name}").into());
            }
            let mut input = fs::File::open(&path)?;
            if stamp(&input.metadata()?) != *expected {
                return Err(format!("source changed while opening: {name}").into());
            }
            let (digest, bytes) = copy_source(&mut input, before.len(), &target)?;
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
                name,
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
fn copy_source(input: &mut (impl Read + Seek), size: u64, target: &Path) -> Result<(String, u64)> {
    if fs::symlink_metadata(target).is_ok_and(|info| info.is_file() && info.len() == size)
        && let Ok(mut cached) = fs::File::open(target)
    {
        if let Some(matched) = compare_source(input, &mut cached, size, target)? {
            return Ok(matched);
        }
        input.rewind()?;
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(target.parent().ok_or("missing capture parent")?)?;
    let record = stream_source(input, &mut temporary, target)?;
    if let Ok(info) = fs::symlink_metadata(target) {
        if info.is_symlink() {
            fs::remove_file(target)?;
        } else {
            writable(target)?;
        }
    }
    temporary.persist(target)?;
    Ok(record)
}
fn read_chunk(reader: &mut impl Read, mut buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while !buffer.is_empty() {
        match reader.read(buffer) {
            Ok(0) => break,
            Ok(count) => {
                total += count;
                buffer = &mut buffer[count..];
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(total)
}
fn compare_source(
    input: &mut impl Read,
    cached: &mut impl Read,
    size: u64,
    target: &Path,
) -> Result<Option<(String, u64)>> {
    let mut digest = Sha256::new();
    let mut bytes = 0_u64;
    let mut source_buf = [0_u8; 16 * 1024];
    let mut cached_buf = [0_u8; 16 * 1024];
    loop {
        let count = match input.read(&mut source_buf) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        bytes += count as u64;
        if bytes > MAX_SOURCE_BYTES {
            return Err(format!("source file exceeds 64 MiB: {}", target.display()).into());
        }
        let Ok(cached_count) = read_chunk(cached, &mut cached_buf[..count]) else {
            return Ok(None);
        };
        if cached_count != count || source_buf[..count] != cached_buf[..count] {
            return Ok(None);
        }
        digest.update(&source_buf[..count]);
    }
    let at_eof = matches!(read_chunk(cached, &mut cached_buf[..1]), Ok(0));
    if bytes == size && at_eof {
        Ok(Some((format!("{:x}", digest.finalize()), bytes)))
    } else {
        Ok(None)
    }
}
fn stream_source(
    input: &mut impl Read,
    output: &mut impl Write,
    target: &Path,
) -> Result<(String, u64)> {
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
        output.write_all(&buffer[..count])?;
    }
    Ok((format!("{:x}", digest.finalize()), bytes))
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
fn parents(root: &Path, name: &str, verified: &mut BTreeSet<PathBuf>) -> Result<()> {
    let mut parent = root.to_owned();
    for part in Path::new(name)
        .parent()
        .ok_or("capture parent missing")?
        .components()
    {
        parent.push(part);
        if verified.contains(&parent) {
            continue;
        }
        match fs::symlink_metadata(&parent) {
            Ok(info) if info.is_dir() && !info.is_symlink() => (),
            Ok(_) => return Err("capture parent must be an ordinary directory".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&parent)?,
            Err(error) => return Err(error.into()),
        }
        verified.insert(parent.clone());
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
fn stamp(info: &fs::Metadata) -> impl Eq + use<> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (
            info.dev(),
            info.ino(),
            info.len(),
            info.mtime(),
            info.mtime_nsec(),
            info.ctime(),
            info.ctime_nsec(),
            info.mode(),
        )
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        (
            info.creation_time(),
            info.last_write_time(),
            info.file_size(),
            info.file_attributes(),
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
pub(super) fn write_manifest(path: &Path, bytes: &[u8]) -> Result<String> {
    let family = hash(bytes);
    if fs::symlink_metadata(path).is_ok_and(|m| m.is_symlink()) {
        fs::remove_file(path)?;
    }
    if fs::metadata(path).is_ok_and(|info| info.is_file() && info.len() == bytes.len() as u64)
        && hash_file(path)? == family
    {
        return Ok(family);
    }
    if path.exists() {
        writable(path)?;
    }
    fs::write(path, bytes)?;
    Ok(family)
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
    let mut verified = 0;
    while let Some(directory) = directories.pop() {
        if !fs::symlink_metadata(&directory)?.is_dir() {
            return Err(format!("captured directory changed: {}", directory.display()).into());
        }
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            let name = path
                .strip_prefix(root)?
                .to_str()
                .ok_or("non-Unicode capture path")?;
            #[cfg(windows)]
            let normalized = name.replace('\\', "/");
            #[cfg(windows)]
            let name = normalized.as_str();
            if kind.is_dir() {
                if !GENERATED.contains(&name) {
                    directories.push(path);
                }
                continue;
            }
            let record = records
                .get(name)
                .ok_or_else(|| format!("uncaptured source added: {name}"))?;
            if record["kind"] == "symlink" {
                if !kind.is_symlink()
                    || fs::read_link(&path)?
                        != Path::new(record["target"].as_str().ok_or("missing link target")?)
                {
                    return Err(format!("captured symlink changed: {name}").into());
                }
            } else {
                if !kind.is_file() || hash_file(&path)? != record["sha256"] {
                    return Err(format!("captured source bytes changed: {name}").into());
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let expected = if record["executable"] == true {
                        0o555
                    } else {
                        0o444
                    };
                    if entry.metadata()?.permissions().mode() & 0o777 != expected {
                        return Err(format!("captured source mode changed: {name}").into());
                    }
                }
            }
            verified += 1;
        }
    }
    if verified != records.len() {
        return Err(format!(
            "captured sources missing: verified {verified} of {}",
            records.len()
        )
        .into());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_fingerprint_tracks_exact_bytes_and_retains_an_unchanged_file() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = temp.path().join("build-family.json");
        assert_eq!(write_manifest(&manifest, b"one").unwrap(), hash(b"one"));
        let file = fs::OpenOptions::new().write(true).open(&manifest).unwrap();
        file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(60))
            .unwrap();
        let modified = file.metadata().unwrap().modified().unwrap();
        drop(file);
        assert_eq!(write_manifest(&manifest, b"one").unwrap(), hash(b"one"));
        assert_eq!(
            fs::metadata(&manifest).unwrap().modified().unwrap(),
            modified
        );
        assert_eq!(write_manifest(&manifest, b"two").unwrap(), hash(b"two"));
        assert_eq!(fs::read(manifest).unwrap(), b"two");
    }

    #[cfg(windows)]
    #[test]
    fn capture_retains_deeply_nested_fixture_payloads_on_windows() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("input");
        let output = root.path().join("capture");
        let parts = [
            "a".repeat(72),
            "b".repeat(72),
            "c".repeat(72),
            "d".repeat(48),
        ];
        let relative = parts.join("/");
        let file = input.join(&relative);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, b"fixture").unwrap();
        let records = capture_names(&input, &output, [relative.as_str()], None).unwrap();
        verify(&output, &records).unwrap();
        assert_eq!(fs::read(output.join(relative)).unwrap(), b"fixture");
    }
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
        let error = capture_names(&input, &output, ["large"], None).unwrap_err();
        assert!(error.to_string().contains("64 MiB"), "{error}");
        assert_eq!(fs::read(output.join("large")).unwrap(), b"previous");
    }

    #[test]
    fn streaming_growth_limit_preserves_destination_and_cleans_temporary_file() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("source");
        fs::write(&target, b"previous").unwrap();
        let mut growing = tempfile::NamedTempFile::new().unwrap();
        growing.as_file().set_len(MAX_SOURCE_BYTES + 1).unwrap();
        assert!(
            copy_source(&mut growing, MAX_SOURCE_BYTES, &target)
                .unwrap_err()
                .to_string()
                .contains("64 MiB")
        );
        assert_eq!(fs::read(&target).unwrap(), b"previous");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn unchanged_capture_reads_without_creating_a_staging_file() {
        struct Source<'a> {
            bytes: std::io::Cursor<&'a [u8]>,
            directory: &'a Path,
        }
        impl Read for Source<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                assert_eq!(fs::read_dir(self.directory)?.count(), 1);
                self.bytes.read(buffer)
            }
        }
        impl std::io::Seek for Source<'_> {
            fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
                std::io::Seek::seek(&mut self.bytes, to)
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("source");
        let bytes = b"previously captured";
        fs::write(&target, bytes).unwrap();
        let mut source = Source {
            bytes: std::io::Cursor::new(bytes),
            directory: directory.path(),
        };
        let size = bytes.len() as u64;
        assert_eq!(
            copy_source(&mut source, size, &target).unwrap(),
            (hash(bytes), size)
        );
    }

    #[test]
    fn streamed_capture_reuses_large_files_and_detects_same_length_changes() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("source");
        let mut bytes = vec![b'a'; 1024 * 1024];
        let size = bytes.len() as u64;
        let (digest, count) =
            copy_source(&mut std::io::Cursor::new(&bytes), size, &target).unwrap();
        assert_eq!(digest, hash(&bytes));
        assert_eq!(count, bytes.len() as u64);
        let modified = fs::metadata(&target).unwrap().modified().unwrap();
        copy_source(&mut std::io::Cursor::new(&bytes), size, &target).unwrap();
        assert_eq!(fs::metadata(&target).unwrap().modified().unwrap(), modified);
        bytes[32_768] = b'b';
        copy_source(&mut std::io::Cursor::new(&bytes), size, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), bytes);

        // Early byte mismatch rewinds without reading the remaining source stream first.
        struct PrefixProbe<'a> {
            bytes: &'a [u8],
            pos: usize,
            max_before_rewind: usize,
            rewound: bool,
        }
        impl Read for PrefixProbe<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = (self.bytes.len() - self.pos).min(buf.len());
                buf[..n].copy_from_slice(&self.bytes[self.pos..self.pos + n]);
                self.pos += n;
                if !self.rewound {
                    self.max_before_rewind = self.max_before_rewind.max(self.pos);
                }
                Ok(n)
            }
        }
        impl Seek for PrefixProbe<'_> {
            fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
                assert_eq!(to, std::io::SeekFrom::Start(0));
                self.rewound = true;
                self.pos = 0;
                Ok(0)
            }
        }
        bytes[0] = b'z';
        let mut probe = PrefixProbe {
            bytes: &bytes,
            pos: 0,
            max_before_rewind: 0,
            rewound: false,
        };
        let (digest, count) = copy_source(&mut probe, size, &target).unwrap();
        assert!(probe.rewound);
        assert!(probe.max_before_rewind <= 16 * 1024);
        assert_eq!(digest, hash(&bytes));
        assert_eq!(count, size);
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
            let error = capture_names(
                root.path(),
                &root.path().join("capture"),
                [name.as_str()],
                None,
            )
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
        let first = capture_names(&root, &out, ["keep", "removed"], None).unwrap();
        let time = fs::metadata(out.join("keep")).unwrap().modified().unwrap();
        let second = capture_names(&root, &out, ["keep"], Some(&first)).unwrap();
        assert_eq!(
            fs::metadata(out.join("keep")).unwrap().modified().unwrap(),
            time
        );
        assert!(!out.join("removed").exists());
        verify(&out, &second).unwrap();
        fs::write(out.join("added.rs"), b"injected").unwrap();
        assert!(verify(&out, &second).is_err());
        fs::remove_file(out.join("added.rs")).unwrap();
        writable(&out.join("keep")).unwrap();
        fs::write(out.join("keep"), b"changed").unwrap();
        assert!(verify(&out, &second).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn verification_distinguishes_literal_backslashes_from_directories() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("input");
        let output = root.path().join("output");
        fs::create_dir(&input).unwrap();
        let name = r"sub\file";
        fs::write(input.join(name), "literal").unwrap();
        let records = capture_names(&input, &output, [name], None).unwrap();
        verify(&output, &records).unwrap();
        fs::create_dir(output.join("sub")).unwrap();
        fs::rename(output.join(name), output.join("sub/file")).unwrap();
        assert!(verify(&output, &records).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_targets_must_stay_in_source() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("input");
        fs::create_dir(&root).unwrap();
        fs::write(t.path().join("outside"), b"no").unwrap();
        std::os::unix::fs::symlink("../outside", root.join("escape")).unwrap();
        assert!(capture_names(&root, &t.path().join("output"), ["escape"], None).is_err());
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
        let out = t.path().join("output");
        let before = capture(&root, &out, None).unwrap();
        fs::remove_file(root.join("deleted")).unwrap();
        fs::write(root.join("tracked"), "dirty").unwrap();
        fs::write(root.join("new"), "untracked").unwrap();
        fs::write(root.join("ignored"), "build").unwrap();
        let files = capture(&root, &out, Some(&before)).unwrap();
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
        let names = ["sub/run", "alias", "missing"];
        let files = capture_names(&alias, &out, names, None).unwrap();
        verify(&out, &files).unwrap();
        assert_eq!(
            fs::read_link(out.join("alias")).unwrap(),
            Path::new("sub/run")
        );
        fs::set_permissions(out.join("sub/run"), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(verify(&out, &files).is_err());
        fs::rename(out.join("sub"), out.join("old")).unwrap();
        symlink(root.join("sub"), out.join("sub")).unwrap();
        assert!(capture_names(&root, &out, names, Some(&files)).is_err());
        assert!(verify(&out, &files).is_err());
        assert_eq!(fs::read_to_string(root.join("sub/run")).unwrap(), "script");
    }
}
