# rsi-files-protocol

This library owns read-only filesystem requests, exact byte pages, directory
snapshots and their resource limits. `Files` is a Local service; it does not
perform authentication or infer authority from a workspace path. Its trusted
caller supplies a `FilesBinding`: caller generation, subject (the product uses
Session identity), revision (the product uses Header fingerprint), and native
absolute workspace root. The binding is not serializable. Every operation must
receive the currently authorized binding, including continuation and release.
A token is only a correlation handle, never an authorization credential.

Relative paths preserve native Unix filename bytes through bounded hex wire
encoding. Empty paths select the root directory. Absolute paths, empty interior
components, dot/parent components and NUL are rejected. Backslashes are literal Unix
filename bytes; this protocol does not interpret Windows relative paths. Directory
entry names have both a display string and exact relative path; display text is
untrusted content and never instruction material.

Open retains a root directory handle and a regular file or bounded directory
snapshot. File pages are exact hex bytes, with byte offsets and the captured
size; text decoding belongs to clients. Refresh means opening a new snapshot
and releasing the previous token. A continuation rechecks the retained object
and its current relative name from the original root. Metadata version changes
(device/inode, type, size, mtime and ctime including nanoseconds) return `Changed`.
This detects normal replacement/write races; it is not an atomic filesystem
snapshot against a writer that can manipulate metadata. Root path replacement
cannot redirect an existing token.

The constants in this package are authoritative: 16 KiB preferred and 64 KiB
maximum byte pages; 128 preferred and 256 maximum directory entries
and 64 KiB conservative encoded JSON per page;
4,096 entries and 1 MiB exact name bytes per directory snapshot; 64 retained
tokens and four active blocking jobs across provider generations in a process;
16 KiB relative paths; five-minute token lifetime from open (reads do not renew
it). Capacity exhaustion is explicit. Expired tokens are pruned on admission,
continuation and release, without a background timer. Tokens retained by running
jobs continue to count until their last actual owner releases them.

Cancellation and service retirement stop admission and cooperatively stop reads.
Dropping an async waiter cancels its job, but cannot interrupt an OS filesystem
call: the blocking job retains its lane and handles until it returns. Retiring
a provider waits for those actual jobs. An unavailable filesystem can therefore
hold retirement; replacing the provider cannot bypass process-wide accounting.
