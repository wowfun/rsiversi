# rsi-files-native-fs

This Unix library owns component-wise directory-handle opens without following
symbolic links. Absolute root acquisition starts at `/`; relative directory and
file opens descend from an already owned `cap_std::fs::Dir`. Root and parent
components cannot redirect an operation through a symlink. Parent traversal and
absolute paths are rejected at relative entry points. Empty relative directory
paths clone the supplied handle. Renaming the root leaves existing handles bound
to the original directory.

File opens are read-only, close-on-exec and nonblocking. They return a native
file handle; callers must check the opened handle's type before reading content.
Opening a FIFO cannot wait for a writer. These helpers neither enumerate nor
read file bodies, and allocate no background work. Callers own input length,
read bounds, cancellation, authorization and directory-handle lifetime.

This package has no non-Unix implementation. Consumers with a separate fallback
must describe that fallback's own guarantees. It does not supply a process
sandbox enforcement stamp or interpret WorkspaceTrust.
