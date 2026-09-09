# rsi-files-native-fs

This Unix library owns component-wise directory-handle opens without following
symbolic links. Absolute root acquisition starts at `/`; relative directory and
file opens descend from an already owned `cap_std::fs::Dir`. Root and parent
components cannot redirect an operation through a symlink. Parent traversal and
absolute paths are rejected at relative entry points. Empty relative directory
paths clone the supplied handle. Renaming the root leaves existing handles bound
to the original directory.

`create_absolute_directory_no_follow` acquires the same authority while creating
missing components with owner-only directory permissions (0700, reduced by the
process umask). It preserves permissions on existing directories and validates
all components before mutation. Each mkdir and subsequent no-follow open uses
the retained parent handle. An I/O failure can leave earlier newly created
parents; the helper does not remove pre-existing or partially created trees.
Embedded NUL bytes are rejected during root preflight, before creating a parent.

An explicit `resolve_absolute_root_alias` path operation canonicalizes only the
first component below `/` and preserves the untouched suffix. Callers choose
whether an absent first component may remain unresolved for later creation.
This supports a caller-authorized OS alias such as `/var` on macOS; it neither
opens the final root nor grants directory authority. The caller still acquires
the returned path through the no-follow helpers. Relative paths, traversal and
NUL are rejected before alias resolution. Root acquisition itself never opts into
alias resolution implicitly.

File opens are read-only, close-on-exec and nonblocking. They return a native
file handle; callers must check the opened handle's type before reading content.
Opening a FIFO cannot wait for a writer. These helpers neither enumerate nor
read file bodies, and allocate no background work. Callers own input length,
read bounds, cancellation, authorization and directory-handle lifetime.

This package has no non-Unix implementation. Consumers with a separate fallback
must describe that fallback's own guarantees. It does not supply a process
sandbox enforcement stamp or interpret WorkspaceTrust.
