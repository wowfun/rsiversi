use crate::{
    DirectoryPage, FileKind, FilePage, FilesError, MAXIMUM_DIRECTORY_ENTRIES,
    MAXIMUM_DIRECTORY_PAGE_BYTES, MAXIMUM_DIRECTORY_PAGE_ENTRIES, MAXIMUM_FILE_PAGE_BYTES,
    OpenedFile, RelativePath, Result,
};
impl OpenedFile {
    /// Validates untrusted opened metadata against the exact admitted request.
    pub fn validate_for(&self, path: &RelativePath, kind: FileKind) -> Result<()> {
        if &self.path != path
            || self.kind != kind
            || (kind == FileKind::Directory && self.executable)
            || (kind == FileKind::Directory && self.length > MAXIMUM_DIRECTORY_ENTRIES as u64)
        {
            return Err(FilesError::Invalid);
        }
        Ok(())
    }
}
impl FilePage {
    /// Validates exact bytes and coordinates before publishing a remote file page.
    pub fn validate_for(&self, file: &OpenedFile, offset: u64, maximum: usize) -> Result<()> {
        if file.kind != FileKind::File
            || offset > file.length
            || !(1..=MAXIMUM_FILE_PAGE_BYTES).contains(&maximum)
        {
            return Err(FilesError::Invalid);
        }
        let count = usize::try_from((file.length - offset).min(maximum as u64))
            .map_err(|_| FilesError::Invalid)?;
        if self.offset != offset
            || self.total != file.length
            || self.bytes_hex.len() != count * 2
            || !self
                .bytes_hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(FilesError::Invalid);
        }
        Ok(())
    }
}
impl DirectoryPage {
    /// Validates bounded, ordered immediate children of the exact opened directory.
    pub fn validate_for(&self, file: &OpenedFile, offset: usize, maximum: usize) -> Result<()> {
        if file.kind != FileKind::Directory
            || file.length > MAXIMUM_DIRECTORY_ENTRIES as u64
            || offset as u64 > file.length
            || !(1..=MAXIMUM_DIRECTORY_PAGE_ENTRIES).contains(&maximum)
            || self.offset != offset
            || self.total as u64 != file.length
            || self.entries.len() > maximum
            || offset
                .checked_add(self.entries.len())
                .is_none_or(|end| end > self.total)
            || (offset < self.total && self.entries.is_empty())
            || rsi_api_protocol::measure_json(self, MAXIMUM_DIRECTORY_PAGE_BYTES).is_err()
            || !self
                .entries
                .windows(2)
                .all(|entries| entries[0].path < entries[1].path)
        {
            return Err(FilesError::Invalid);
        }
        for entry in &self.entries {
            let name = entry
                .path
                .as_bytes()
                .rsplit(|byte| *byte == b'/')
                .next()
                .ok_or(FilesError::Invalid)?;
            if file.path.join(name)? != entry.path || entry.name != String::from_utf8_lossy(name) {
                return Err(FilesError::Invalid);
            }
        }
        Ok(())
    }
}
