//! Bounded lexical host paths, independent of the reader's filesystem target.
#![deny(unsafe_code)]
#![warn(missing_docs)]

/// Maximum encoded UTF-8 bytes in a host path.
pub const MAXIMUM_HOST_PATH_BYTES: usize = 16 * 1024;

/// Accepts a host path as text or a bounded native path with non-UTF-8 bytes.
pub fn is_absolute_path(path: &std::path::Path) -> bool {
    path.to_str()
        .map_or_else(|| native_bound(path) && path.is_absolute(), is_absolute)
}

/// Preserves native non-UTF-8 paths while validating foreign UTF-8 path data.
pub fn is_normalized_absolute_path(path: &std::path::Path) -> bool {
    if let Some(text) = path.to_str() {
        return is_normalized_absolute(text);
    }
    if !native_bound(path) || !path.is_absolute() {
        return false;
    }
    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir | std::path::Component::ParentDir => return false,
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized.as_os_str() == path.as_os_str()
}
fn native_bound(path: &std::path::Path) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();
    bytes.len() <= MAXIMUM_HOST_PATH_BYTES && !bytes.contains(&0)
}

/// Accepts bounded absolute host paths without parent components or NUL.
pub fn is_absolute(value: &str) -> bool {
    if value.len() > MAXIMUM_HOST_PATH_BYTES || value.contains('\0') {
        return false;
    }
    if value.starts_with('/') {
        return value.split('/').all(|part| part != "..");
    }
    if let Some(rest) = value.strip_prefix(r"\\?\") {
        return rest.strip_prefix(r"UNC\").map_or_else(|| drive(rest), unc);
    }
    value.strip_prefix(r"\\").map_or_else(|| drive(value), unc)
}

/// Additionally rejects non-root empty/dot components and trailing separators.
pub fn is_normalized_absolute(value: &str) -> bool {
    if !is_absolute(value) {
        return false;
    }
    let rest = if let Some(posix) = value.strip_prefix('/') {
        return posix.is_empty() || posix.split('/').all(normal_component);
    } else {
        value.strip_prefix(r"\\?\").unwrap_or(value)
    };
    if drive(rest) {
        let tail = &rest[3..];
        return tail.is_empty() || tail.split(['/', '\\']).all(normal_component);
    }
    let unc = rest
        .strip_prefix(r"UNC\")
        .or_else(|| rest.strip_prefix(r"\\"))
        .unwrap_or(rest);
    let mut parts = unc.split(['/', '\\']);
    let _ = parts.next();
    let _ = parts.next();
    parts.all(normal_component)
}
fn normal_component(part: &str) -> bool {
    !part.is_empty() && !matches!(part, "." | "..")
}

fn drive(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
        && value.split(['/', '\\']).all(|part| part != "..")
}

fn unc(value: &str) -> bool {
    let mut parts = value.split(['/', '\\']);
    let valid_root = |part: Option<&str>| {
        part.is_some_and(|part| !part.is_empty() && !matches!(part, "." | ".." | "?"))
    };
    valid_root(parts.next()) && valid_root(parts.next()) && parts.all(|part| part != "..")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn native_non_utf8_bytes_remain_usable_without_becoming_serializable_host_text() {
        use std::os::unix::ffi::OsStringExt as _;
        let path =
            std::path::PathBuf::from(std::ffi::OsString::from_vec(b"/workspace/\xff".to_vec()));
        assert!(path.to_str().is_none());
        assert!(is_absolute_path(&path));
        assert!(is_normalized_absolute_path(&path));
        assert!(!is_normalized_absolute_path(&path.join("../outside")));
    }
    #[test]
    fn foreign_absolute_paths_are_lexical_data_and_parent_components_are_rejected() {
        for path in [
            "/",
            "/workspace/项目",
            r"/posix/back\slash",
            r"C:\",
            "C:/workspace/project",
            r"D:\项目\code",
            r"\\server\share",
            r"\\server\share\project",
            r"\\?\C:\project",
            r"\\?\UNC\server\share\project",
        ] {
            assert!(is_absolute(path), "{path}");
        }
        for path in [
            "",
            "relative",
            "./workspace",
            "../workspace",
            "/workspace/../outside",
            "C:relative",
            r"\root-relative",
            r"C:\workspace\..\outside",
            r"\\server",
            r"\\server\..\outside",
            r"\\?\C:\project\..",
            r"\\?\UNC\server\share\..",
            r"\\.\device",
        ] {
            assert!(!is_absolute(path), "{path}");
        }
    }
    #[test]
    fn normalization_and_size_limits_do_not_depend_on_native_path_components() {
        for path in [
            "/",
            "/workspace",
            r"C:\",
            r"C:\workspace",
            r"\\server\share",
            r"\\?\UNC\server\share\project",
        ] {
            assert!(is_normalized_absolute(path), "{path}");
        }
        for path in [
            "//workspace",
            "/workspace/",
            "/workspace//file",
            "/workspace/./file",
            r"C:\workspace\.\file",
            r"\\server\share\project\",
        ] {
            assert!(!is_normalized_absolute(path), "{path}");
        }
        assert!(!is_absolute("/nul\0path"));
        assert!(is_absolute(&format!(
            "/{}",
            "x".repeat(MAXIMUM_HOST_PATH_BYTES - 1)
        )));
        assert!(!is_absolute(&format!(
            "/{}",
            "x".repeat(MAXIMUM_HOST_PATH_BYTES)
        )));
    }
}
