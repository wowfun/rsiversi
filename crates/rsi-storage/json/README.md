# rsi-storage-json

Factory preparation preserves malformed configuration as `MetaError::InvalidInput`
and reports unavailable native capability as `MetaError::Activation`, the Meta
category for plugin preparation and activation failure.

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
Non-Unix preparation and acquisition reject explicitly before reading the
document or creating filesystem entries; native privacy and publication
guarantees are not implemented there.
Configuration syntax and bounds are checked without filesystem access; malformed
configuration remains `InvalidInput`, while valid unsupported acquisition
returns `Unsupported`.

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
claiming protection against external edits.

On Unix, directory creation synchronizes every ancestor entry before publishing
backend state, including an existing ancestor left by an interrupted creation.
Directory-sync failure prevents a successful first publication.
