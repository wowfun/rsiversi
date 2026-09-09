#![cfg(unix)]

use rsi_files_native_fs::*;
use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _, symlink};
use std::{
    fs,
    io::{Read as _, Write as _},
    path::Path,
};

#[test]
fn roots_and_relative_opens_reject_links_and_parent_traversal() {
    let temporary = tempfile::tempdir().unwrap();
    let base = temporary.path().canonicalize().unwrap();
    fs::create_dir_all(base.join("inside/sub")).unwrap();
    fs::create_dir(base.join("outside")).unwrap();
    fs::write(base.join("outside/private"), "outside").unwrap();
    symlink(base.join("outside"), base.join("inside/link")).unwrap();
    symlink(base.join("outside/private"), base.join("inside/file-link")).unwrap();
    symlink(base.join("inside"), base.join("alias")).unwrap();
    assert!(open_absolute_directory_no_follow(&base.join("alias")).is_err());
    assert!(open_absolute_directory_no_follow(&base.join("alias/sub")).is_err());
    assert!(open_absolute_directory_no_follow(Path::new("relative")).is_err());
    let root = open_absolute_directory_no_follow(&base.join("inside")).unwrap();
    for path in ["../outside", "sub/../../outside", "/"] {
        assert!(
            open_relative_directory_no_follow(&root, Path::new(path)).is_err(),
            "{path}"
        );
    }
    for path in [
        "../outside/private",
        "sub/../../outside/private",
        "/outside/private",
        "link/private",
        "file-link",
        "",
    ] {
        assert!(
            open_relative_file_no_follow(&root, Path::new(path)).is_err(),
            "{path}"
        );
    }
    assert!(is_link_rejection(
        &open_relative_directory_no_follow(&root, Path::new("link")).unwrap_err()
    ));
    assert_eq!(
        fs::read_to_string(base.join("outside/private")).unwrap(),
        "outside"
    );
}

#[test]
fn retained_root_and_child_handles_survive_path_replacement_without_following_new_links() {
    let temporary = tempfile::tempdir().unwrap();
    let base = temporary.path().canonicalize().unwrap();
    fs::create_dir_all(base.join("root/sub")).unwrap();
    fs::create_dir(base.join("outside")).unwrap();
    fs::write(base.join("root/sub/value"), "inside").unwrap();
    fs::write(base.join("outside/value"), "outside").unwrap();
    let root = open_absolute_directory_no_follow(&base.join("root")).unwrap();
    let child = open_relative_directory_no_follow(&root, Path::new("sub")).unwrap();
    fs::rename(base.join("root"), base.join("old-root")).unwrap();
    symlink(base.join("outside"), base.join("root")).unwrap();
    let mut value = String::new();
    open_relative_file_no_follow(&root, Path::new("sub/value"))
        .unwrap()
        .read_to_string(&mut value)
        .unwrap();
    assert_eq!(value, "inside");
    fs::rename(base.join("old-root/sub"), base.join("old-root/moved")).unwrap();
    symlink(base.join("outside"), base.join("old-root/sub")).unwrap();
    assert!(open_relative_file_no_follow(&root, Path::new("sub/value")).is_err());
    value.clear();
    let cloned = open_relative_directory_no_follow(&child, Path::new("")).unwrap();
    drop(child);
    drop(root);
    open_relative_file_no_follow(&cloned, Path::new("value"))
        .unwrap()
        .read_to_string(&mut value)
        .unwrap();
    assert_eq!(value, "inside");
}

#[test]
fn file_handle_is_read_only_and_fifo_open_has_nonblocking_flags() {
    let temporary = tempfile::tempdir().unwrap();
    let base = temporary.path().canonicalize().unwrap();
    fs::write(base.join("regular"), "retained").unwrap();
    let root = open_absolute_directory_no_follow(&base).unwrap();
    let mut file = open_relative_file_no_follow(&root, Path::new("regular")).unwrap();
    assert!(file.write_all(b"changed").is_err());
    assert_eq!(
        fs::read_to_string(base.join("regular")).unwrap(),
        "retained"
    );
    assert!(
        std::process::Command::new("mkfifo")
            .arg(base.join("fifo"))
            .status()
            .unwrap()
            .success()
    );
    // Keep both ends open so a regression in O_NONBLOCK fails the flag assertion
    // instead of blocking this test's own process indefinitely.
    let reader = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(base.join("fifo"))
        .unwrap();
    let writer = fs::OpenOptions::new()
        .write(true)
        .open(base.join("fifo"))
        .unwrap();
    let fifo = open_relative_file_no_follow(&root, Path::new("fifo")).unwrap();
    assert!(fifo.metadata().unwrap().file_type().is_fifo());
    assert!(
        rustix::fs::fcntl_getfl(&fifo)
            .unwrap()
            .contains(rustix::fs::OFlags::NONBLOCK)
    );
    assert!(
        rustix::io::fcntl_getfd(&fifo)
            .unwrap()
            .contains(rustix::io::FdFlags::CLOEXEC)
    );
    drop((reader, writer));
}
