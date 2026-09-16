# rsi-credentials-local

The ordinary local plugin resolves exact credential references from a private
file first and an explicitly captured startup environment snapshot second.
Only an absent file or entry permits fallback. Corruption, permission failures,
unsafe paths and I/O errors never select another secret. Admin may replace an
environment credential; deleting a stored entry reveals the captured fallback.
Status reports the effective source, editability, store location and a closed
safe failure category. It contains neither secrets nor backend error text.

Composition injects the store and captured environment. The standard product
selects the path; this module never discovers a home directory. Tests inject
memory stores or private temporary files, never real user credentials.

## File contract

The JSON document is `{"version":1,"entries":[{"reference":{"owner":"provider",
"slot":"default"},"secret":"value"}]}`. The whole file is at most 4 MiB with at
most 4,096 distinct references. Reference and secret limits belong to the
[protocol](../protocol/README.md). Unknown fields, versions, duplicate object
fields or references, invalid content and oversized files fail closed.
The file codec and authenticated credential API explicitly encode secret text.
Secret wrappers and the file codec's owned input/output buffers are zeroized on
drop. Serde's escape scratch space and API/transport buffers do not promise
zeroization; this is not a guarantee that every transient plaintext copy is erased.

Unix directory authority is acquired component by component without following
links. The private credential directory must belong to the effective user with
mode 0700; credential, lock and temporary files must be single-link regular
files owned by that user with mode 0600. Existing permissions are never silently
repaired. Missing directories and files are created only by mutation. A retained
directory handle owns each operation; subsequent file operations do not reopen
its absolute path. Other targets fail explicitly as unsupported when equivalent
native access controls are unavailable; they never write an unprotected file.

Writers acquire a stable private lock file with a one-second bounded wait, reread
the current document under the lock and preserve unrelated entries. Lock files
are created exclusively; an existing name is opened separately without creation
flags and receives the same no-follow and metadata checks. Writers never remove
or replace that lock file. A unique
same-directory temporary file is written and synced before atomic replacement;
the directory is then synced. Pre-publication failures preserve the old document.
A failure after replacement reports an unknown mutation outcome, never a false
failure or automatic retry. Temporary-file cleanup also runs when metadata
validation fails. No in-place rewrite or corruption recovery occurs.

## Work ownership

Resolve, set and unset share `maximum_concurrent_store_operations`, default 8,
range 1..=64. One full reference shares one in-flight lookup; settled results are
not cached. `resolution_timeout_ms` defaults to 30 seconds, range 1 ms through
5 minutes. A timeout detaches only that waiter; admitted blocking work retains
its permit until completion. Queued callers acquire admission before allocating
a background task or singleflight entry. Admin awaits its exact result; dropping
a caller cannot cancel accepted writes or release their permits early.
Mutation completion invalidates older lookups for that reference, including when
the waiter was dropped. A resolution started after completion cannot join a pre-write lookup; an
old lookup's cleanup cannot remove a newer flight. A resolve overlapping a write
may still finish with the earlier value, as may a provider call that already
froze that value. Rotation does not revoke already issued secret values.

`FileSecretStore::new` uses strict no-follow acquisition. Explicit owners may use
`with_trusted_root_alias` to resolve only the first component beneath `/` before
that same acquisition, using the native Files root-alias contract. Construction
performs no I/O; each operation validates the logical and resolved path. Nested
symlinks and all existing file/permission checks remain enforced. Other platforms
retain their explicit unsupported behavior.
