# rsi-storage-json

This ordinary backend plugin stores all of its routed non-session domains in
one explicit JSON file. It validates the complete bounded document at startup
and publishes updates through a same-directory temporary file, file sync,
rename, and directory sync. One async operation slot is acquired before a
blocking filesystem task is created, so concurrent domains cannot create an
unbounded blocking-task queue for this file.
Startup opens an existing document without following its final symlink and
pins one unchanged regular-file identity before reading bounded bytes.
Newly created path components and files are private on Unix; existing
caller-supplied parent directories retain their permissions.

The backend does not merge concurrent processes or watch external edits. A
standard composition must not point two writers at the same file.
Domain schema versions are nonzero at both the direct backend seam and reload.
