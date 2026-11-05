# rsi-settings-local

Loads and writes share a gate owned by the blocking filesystem worker, so a
dropped waiter cannot let a read overtake its write. Failures before publication
are definite I/O failures; failures after rename or a dispatched write worker
failure return `OutcomeUnknown`. Readback establishes visible state only and
does not certify that a failed directory sync became durable.

Factory preparation preserves malformed configuration as `MetaError::InvalidInput`
and reports unavailable native capability as `MetaError::Activation`, the Meta
category for plugin preparation and activation failure.

This ordinary provider plugin owns one explicit JSON Settings document. A write
acquires a same-directory cross-process lock, re-reads the complete bounded
document, compares the target raw section, and publishes through a synced
temporary file and atomic rename. Supporting platforms also sync the parent
directory. The persistent lock must be a real regular file and preplaced
symlinks are rejected before permissions are changed. Unloaded namespaces are
preserved.
Unix document acquisition is nonblocking and admits only an opened regular file
before reading. FIFOs and other special files are rejected without waiting for
a writer or stream EOF; existing directory and alias policy is unchanged.
Newly created path components and files are private on Unix; the provider never
changes permissions on a caller-supplied directory that already exists.
Unix writes synchronize the complete parent-directory chain before acquiring the
writer lock, including pre-existing ancestors from an interrupted creation.
A synchronization failure prevents document publication and success.
This requires readable, synchronizable ancestors: an execute-only intermediate
directory can permit ordinary file access yet still make a write fail. Each
write pays synchronization cost proportional to directory depth, including retries.
Windows reads use retained no-reparse parent and regular-file handles. Writes
require a TokenUser-owned protected private parent, retain its namespace pin and
exclusive no-follow writer lock, and publish a synced atomically private staging
file before flushing the directory. Existing parent permissions are never
repaired. Staging retries up to 64 occupied names without removing another
writer's file; failure cleanup uses the retained parent. Other unsupported
platforms refuse before reading or creating locks.
Configuration absolute-path syntax and document bounds are checked without I/O
before platform admission: malformed configuration returns `InvalidInput`,
while valid configuration on an unsupported platform returns `Unsupported`.

The provider does not read environment variables, discover a default path, or
store secrets. External file watching is outside this first local provider;
each write still detects unseen concurrent changes instead of overwriting them.
