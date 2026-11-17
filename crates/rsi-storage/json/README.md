# rsi-storage-json

This Unix backend owns an explicit directory of non-session domain units. It
never imports the former shared-file layout. An existing file at the configured
root is rejected without modification. Malformed configuration is InvalidInput;
valid configuration on an unsupported platform is Unsupported before filesystem
access. The factory maps these to Meta InvalidInput and Activation respectively.

Each unit selects single or per-record layout. Names and keys are hashed for
native filenames; every document carries and verifies its original identity.
Keys retain the Storage UTF-8 contract, including slashes, empty strings and NUL.
Opening a unit reserves its name until its last handle and admitted worker finish.
There is no cross-process merge or external-edit watcher; composition supplies
one writer for the root. Existing caller parent permissions are preserved; new
owned directories and files are private, and final symlinks are rejected.

Single layout retains only that open unit's shared encoded records and exact size
accounting. A mutation measures only changed entries, applies the batch with an
undo record for each touched key, and streams one compact JSON document through
a bounded temporary file, fsync, rename and parent fsync. A failed publication
restores those keys and keeps the previous accounting. Releasing the
unit releases its cache. Per-record layout retains only address/size accounting;
record bodies are read at open and owned by Domain. Each replacement writes one
identity/version envelope. Only single layout supports arbitrary atomic batches.

Per-record delete-many stages the selected files out of the readable set, syncs
all affected directories, then removes the staged residue. Deletion retains
no-follow directory handles for every selected parent before
mutation; staging, rollback, synchronization and residue cleanup use only those
handles. Replacing a pathname cannot redirect deletion into another directory.
Any staging or sync
failure restores all moved files and syncs the restoration before returning a
known rejection. Failed restoration is OutcomeUnknown. This is rollback on a
reported failure, not a crash-atomic transaction across multiple files: an
interrupted, unacknowledged deletion may be partially visible after reopen.
Acknowledged deletion remains absent. Consumers requiring crash-atomic batches
must select single layout or SQLite. Hidden staging residue is never a record.

Each value is admitted as bounded compact raw JSON before copying; business
codecs run once in Domain. Both layouts enforce the family record and aggregate
byte bounds. Configuration maximum_document_bytes bounds each complete physical
document, including envelopes, at no more than 64 MiB. Reads accept explicitly
compatible versions without rewriting; writes stamp only their publication unit.
Single layout stamps the document; per-record stamps each touched envelope.

Only per-record layout supports explicitly disposable table backup-and-skip.
Malformed syntax, invalid bounded values, or rejected business codecs preserve
the exact source in a unique quarantine file and sync both directories before
omission. Globals, identity mismatches, undeclared tables and unknown format or
business versions reject open. No corrupt file silently becomes absent. Reports
contain the table, optional key, backup path and cause, never private values.

A shared operation slot is acquired before spawning blocking I/O. Accepted work
retains the unit lease through publication despite caller cancellation. Failures
before replacement keep the generation usable; uncertain replacement, directory
sync or rollback fences every unit in that backend generation until reopen.
Directory creation syncs ancestor entries, including interrupted earlier creates.
Each atomic file writer owns parent creation and its ancestor sync; callers check
layout identities without repeating directory publication. Quarantine still syncs
both the source and destination directories after a move.
Reopening validated files syncs their parents before admitting recovered state.
