use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

/// Checks the producer's frozen inputs, without trusting a manifest's claimed digest.
pub fn validate(root: &Path, value: &Value) -> Result<Vec<PathBuf>, String> {
    if value.get("format").and_then(Value::as_u64) != Some(1) {
        return Err("unsupported paired build manifest schema".into());
    }
    let files = value
        .get("files")
        .and_then(Value::as_object)
        .filter(|files| !files.is_empty())
        .ok_or("paired build manifest has no frozen inputs")?;
    let root = root.canonicalize().map_err(|error| error.to_string())?;
    let mut paths = Vec::with_capacity(files.len());
    for (name, record) in files {
        if name.is_empty()
            || !Path::new(name)
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        {
            return Err(format!("invalid frozen input path: {name}"));
        }
        let path = root.join(name);
        if !path
            .parent()
            .ok_or("input has no parent")?
            .canonicalize()
            .map_err(|error| error.to_string())?
            .starts_with(&root)
        {
            return Err(format!("frozen input parent escapes source: {name}"));
        }
        validate_record(&path, record).map_err(|error| format!("frozen input {name}: {error}"))?;
        paths.push(path);
    }
    Ok(paths)
}

fn validate_record(path: &Path, record: &Value) -> Result<(), String> {
    let info = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    match record.get("kind").and_then(Value::as_str) {
        Some("symlink") => {
            let expected = record
                .get("target")
                .and_then(Value::as_str)
                .ok_or("missing symlink target")?;
            if !info.is_symlink()
                || fs::read_link(path).map_err(|error| error.to_string())? != Path::new(expected)
            {
                return Err("symlink changed".into());
            }
        }
        Some("file") => {
            let expected = record
                .get("sha256")
                .and_then(Value::as_str)
                .ok_or("missing file digest")?;
            let size = record
                .get("bytes")
                .and_then(Value::as_u64)
                .ok_or("missing file size")?;
            let executable = record
                .get("executable")
                .and_then(Value::as_bool)
                .ok_or("missing executable bit")?;
            if !info.is_file() || info.len() != size {
                return Err("file kind or size changed".into());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if (info.permissions().mode() & 0o111 != 0) != executable {
                    return Err("executable bit changed".into());
                }
            }
            #[cfg(not(unix))]
            let _ = executable;
            let mut input = fs::File::open(path)
                .map_err(|error| error.to_string())?
                .take(size.saturating_add(1));
            let mut hash = Sha256::new();
            let mut block = [0_u8; 8 * 1024];
            loop {
                let length = input.read(&mut block).map_err(|error| error.to_string())?;
                if length == 0 {
                    break;
                }
                hash.update(&block[..length]);
            }
            if format!("{:x}", hash.finalize()) != expected {
                return Err("source digest changed".into());
            }
        }
        _ => return Err("unknown input kind".into()),
    }
    Ok(())
}
