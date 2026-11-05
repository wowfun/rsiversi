use crate::{
    DirectoryPage, FileKind, FilePage, FilesError, MAXIMUM_DIRECTORY_ENTRIES,
    MAXIMUM_DIRECTORY_PAGE_ENTRIES, MAXIMUM_FILE_PAGE_BYTES, MAXIMUM_FILE_PATH_BYTES, Result,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Bounded native filename bytes, interpreted only by the execution backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadPath(Box<[u8]>);
impl ReadPath {
    /// Accept a nonempty path without NUL; Unix bytes or Windows UTF-8.
    pub fn new(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > MAXIMUM_FILE_PATH_BYTES || bytes.contains(&0) {
            return Err(FilesError::Invalid);
        }
        Ok(Self(bytes.into()))
    }
    /// Exact native path bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}
impl Serialize for ReadPath {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(&self.0))
    }
}
impl<'de> Deserialize<'de> for ReadPath {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.len() > MAXIMUM_FILE_PATH_BYTES * 2 {
            return Err(serde::de::Error::custom(FilesError::Invalid));
        }
        let bytes = hex::decode(value).map_err(serde::de::Error::custom)?;
        Self::new(&bytes).map_err(serde::de::Error::custom)
    }
}

/// One validated finite read; possession does not grant host access.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "RawPathRead")]
pub struct PathRead {
    cwd: PathBuf,
    path: ReadPath,
    kind: FileKind,
    offset: u64,
    maximum: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPathRead {
    cwd: PathBuf,
    path: ReadPath,
    kind: FileKind,
    offset: u64,
    maximum: usize,
}
impl TryFrom<RawPathRead> for PathRead {
    type Error = FilesError;
    fn try_from(raw: RawPathRead) -> Result<Self> {
        Self::new(raw.cwd, raw.path, raw.kind, raw.offset, raw.maximum)
    }
}
impl PathRead {
    /// Validate coordinates and page bounds without resolving the target path.
    pub fn new(
        cwd: PathBuf,
        path: ReadPath,
        kind: FileKind,
        offset: u64,
        maximum: usize,
    ) -> Result<Self> {
        let limit = match kind {
            FileKind::File => MAXIMUM_FILE_PAGE_BYTES,
            FileKind::Directory => MAXIMUM_DIRECTORY_PAGE_ENTRIES,
        };
        if !rsi_workspace_path::is_normalized_absolute_path(&cwd)
            || maximum == 0
            || maximum > limit
            || (kind == FileKind::Directory && offset > MAXIMUM_DIRECTORY_ENTRIES as u64)
        {
            return Err(FilesError::Invalid);
        }
        Ok(Self {
            cwd,
            path,
            kind,
            offset,
            maximum,
        })
    }
    /// Base on the selected execution host.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }
    /// Requested spelling, including any native parent or symlink components.
    pub const fn path(&self) -> &ReadPath {
        &self.path
    }
    /// Required object kind.
    pub const fn kind(&self) -> FileKind {
        self.kind
    }
    /// Byte or entry offset.
    pub const fn offset(&self) -> u64 {
        self.offset
    }
    /// Maximum bytes or entries.
    pub const fn maximum(&self) -> usize {
        self.maximum
    }
}

/// One finite read result, without a retained continuation token.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathPage {
    /// Canonical absolute path bytes on the execution host.
    pub path: ReadPath,
    /// Captured page of the requested object.
    pub page: ReadPage,
}
/// Native page payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "page",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ReadPage {
    /// Regular-file bytes.
    File(FilePage),
    /// Immediate relative child names of the returned directory.
    Directory(DirectoryPage),
}
impl PathPage {
    /// Check an external reply before publishing it to trusted in-process callers.
    pub fn validate_for(&self, kind: FileKind, offset: u64, maximum: usize) -> Result<()> {
        let path = self.path.as_bytes();
        let absolute = if path.starts_with(b"/") {
            path.len() == 1
                || path[1..]
                    .split(|b| *b == b'/')
                    .all(|p| !p.is_empty() && p != b"." && p != b"..")
        } else {
            std::str::from_utf8(path).is_ok_and(rsi_workspace_path::is_normalized_absolute)
        };
        if !absolute {
            return Err(FilesError::Invalid);
        }
        match (&self.page, kind) {
            (ReadPage::File(page), FileKind::File) => page.validate_range(offset, maximum),
            (ReadPage::Directory(page), FileKind::Directory) => page.validate_range(
                &crate::RelativePath::default(),
                usize::try_from(offset).map_err(|_| FilesError::Invalid)?,
                maximum,
            ),
            _ => Err(FilesError::Invalid),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finite_read_boundary_rejects_invalid_requests_and_mismatched_replies() {
        let request = PathRead::new(
            "/cwd".into(),
            ReadPath::new(b"../file").unwrap(),
            FileKind::File,
            1,
            2,
        )
        .unwrap();
        let mut wire = serde_json::to_value(&request).unwrap();
        wire["maximum"] = 65537.into();
        assert!(serde_json::from_value::<PathRead>(wire).is_err());
        for path in [b"".as_slice(), b"a\0b"] {
            assert!(ReadPath::new(path).is_err());
        }
        let valid = PathPage {
            path: ReadPath::new(b"/resolved/file").unwrap(),
            page: ReadPage::File(FilePage {
                offset: 1,
                total: 4,
                bytes_hex: "6162".into(),
            }),
        };
        valid
            .validate_for(request.kind(), request.offset(), request.maximum())
            .unwrap();
        for path in [b"relative".as_slice(), b"/a/../b", b"/a//b"] {
            let mut page = valid.clone();
            page.path = ReadPath::new(path).unwrap();
            assert!(
                page.validate_for(request.kind(), request.offset(), request.maximum())
                    .is_err()
            );
        }
        for file in [
            FilePage {
                offset: 0,
                total: 4,
                bytes_hex: "6162".into(),
            },
            FilePage {
                offset: 1,
                total: 4,
                bytes_hex: "61".into(),
            },
            FilePage {
                offset: 1,
                total: 4,
                bytes_hex: "zzzz".into(),
            },
        ] {
            let mut page = valid.clone();
            page.page = ReadPage::File(file);
            assert!(
                page.validate_for(request.kind(), request.offset(), request.maximum())
                    .is_err()
            );
        }
    }
}
