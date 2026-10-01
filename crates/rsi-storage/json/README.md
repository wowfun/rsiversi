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

Each successful mutation still copies and publishes the bounded complete document;
this backend does not promise incremental write cost.

The backend does not merge concurrent processes or watch external edits. A
standard composition must not point two writers at the same file.
Domain schema versions are nonzero at both the direct backend seam and reload.

Pretty JSON is streamed to a bounded temporary file. Failure before replacement
leaves the instance usable; failure to sync the parent after replacement returns
`OutcomeUnknown` and fences the entire backend. The parent directory is opened
before replacement. On Unix, reopening an existing validated document syncs its
parent before admission. Files are already synced before their names become
visible. This establishes recovery within the single-writer contract, without
claiming protection against external edits. Non-Unix directory sync remains
subject to the platform's existing replacement guarantees.

On Unix, directory creation synchronizes every ancestor entry before publishing
backend state, including an existing ancestor left by an interrupted creation.
Directory-sync failure prevents a successful first publication.
