# rsi-settings-local

This ordinary provider plugin owns one explicit JSON Settings document. A write
acquires a same-directory cross-process lock, re-reads the complete bounded
document, compares the target raw section, and publishes through a synced
temporary file and atomic rename. Supporting platforms also sync the parent
directory. The persistent lock must be a real regular file and preplaced
symlinks are rejected before permissions are changed. Unloaded namespaces are
preserved.
Newly created path components and files are private on Unix; the provider never
changes permissions on a caller-supplied directory that already exists.

The provider does not read environment variables, discover a default path, or
store secrets. External file watching is outside this first local provider;
each write still detects unseen concurrent changes instead of overwriting them.
