# rsi-files-protocol

This library owns read-only filesystem requests, exact byte pages, directory
snapshots and their resource limits. `Files` is a Local service; it does not
perform authentication or infer authority from a workspace path. Its trusted
caller supplies a `FilesBinding`: caller generation, subject (the product uses
Session identity), revision (the product uses Header fingerprint), and native
absolute workspace root. The binding is not serializable. Every operation must
receive the currently authorized binding, including continuation and release.
A token is only a correlation handle, never an authorization credential.
Trusted capability wrappers may replace the opaque caller with `with_caller`
while preserving the exact subject, revision and workspace. This scopes a binding
to the wrapper's lease without serializing authority or changing filesystem paths.
`describe` returns its admitted path/kind/length and executable flag after current binding and expiry
checks. Adapters compare a client-supplied descriptor before returning its body.
The protocol's `validate_for` methods own reply validation for both API and SSH
adapters: exact requested metadata, byte ranges, canonical hex, directory ordering,
child names and encoded page limits. Adapters map malformed replies to their own
transport error without inventing a different filesystem contract.
After stopping and draining its own admission, a caller uses `release_caller` to
release that generation's remaining tokens, including lost open responses.
Already running native jobs retain their resource permits until actual exit.

Relative paths preserve native Unix filename bytes through bounded hex wire
encoding. Empty paths select the root directory. Absolute paths, empty interior
components, dot/parent components and NUL are rejected. Backslashes are literal Unix
filename bytes; this protocol does not interpret Windows relative paths. Directory
entry names have both a display string and exact relative path; display text is
untrusted content and never instruction material.

Open returns its exact relative path alongside the token, captured length and
executable flag from the same opened regular-file metadata. Directories always
report false. Permission changes invalidate the token just like content changes;
consumers must not reopen a path on another machine to derive executable mode.
It retains a root directory handle and a regular file or bounded directory
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

`OutcomeUnknown` reports an admitted operation whose acknowledgement cannot be
verified. An adapter preserves this distinction from pre-admission rejection;
it does not silently repeat the operation or convert it to ordinary Tool text.

`Missing` is proof that opening a relative name below a successfully opened root
returned native not-found. Missing roots, unavailable providers, expired tokens
and transport failures do not prove absence. Consumers may infer deletion only
from `Missing`; all other failures preserve incomplete evidence.
