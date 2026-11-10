pub(super) fn temporary_root() -> tempfile::TempDir {
    let root = std::env::temp_dir();
    #[cfg(not(windows))]
    let root = root.canonicalize().unwrap();
    tempfile::tempdir_in(root).unwrap()
}
