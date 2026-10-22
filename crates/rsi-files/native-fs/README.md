# rsi-files-native-fs

Windows callers may validate the native absolute path grammar without I/O, and
verify an already acquired directory's private policy without creating a guard.

Unsafe code is confined to the Windows native syscall module, where each operation
documents its contract. Common code, the Unix implementation and the rest of the
Windows adapter retain the crate's unsafe-code denial.

This library owns component-wise directory-handle opens without following
links. Unix absolute root acquisition starts at `/`; relative directory and
file opens descend from an already owned opaque `Directory`. Root and parent
components cannot redirect an operation through a symlink. Parent traversal and
absolute paths are rejected at relative entry points. Empty relative directory
paths clone the supplied handle. Renaming the root leaves existing handles bound
to the original directory.
Single-child kind lookup uses the retained directory without enumeration or link
traversal; accumulated sibling archives cannot block a lookup by capacity.

Private staging creation, single-component hard-link publication and removal use
retained directory handles. Publication never replaces an existing destination;
callers own content verification, synchronization order and recovery. Windows
namespace pins support the same operations without reopening absolute names.
Windows can also privatize one owned child directory relative to a retained
parent. This explicit mutation returns the admitted child handle.
Single-component empty-directory removal also uses the retained parent and
refuses links and nonempty directories. Callers own traversal bounds and must
release a child's namespace pin before removing its permanent guard.

`Directory`, `DirectoryEntries` and `DirectoryEntry` own the native handles and
enumeration lifetime. No public signature or dereference exposes cap-std's
path-based directory operations. An entry exposes its name and closed kind
classification, not an absolute path or an independently acquired authority.
Unix consumers may explicitly borrow its file descriptor or consume it as a
standard file for their own handle-based operations; this never reopens a path.
They may also transfer an already opened file into `Directory::from_opened_file`.
It verifies the retained object's directory type and adopts that same handle,
without resolving a name. The caller owns the original acquisition policy.
Directory metadata inspection uses the retained file without cloning a handle.

On Unix, `create_absolute_directory_no_follow` acquires the same authority while creating
missing components with owner-only directory permissions (0700, reduced by the
process umask). It preserves permissions on existing directories and validates
all components before mutation. Each mkdir and subsequent no-follow open uses
the retained parent handle. An I/O failure can leave earlier newly created
parents; the helper does not remove pre-existing or partially created trees.
Embedded NUL bytes are rejected during root preflight, before creating a parent.

An explicit `resolve_absolute_root_alias` path operation canonicalizes only the
first component below `/` and preserves the untouched suffix. Callers choose
whether an absent first component may remain unresolved for later creation.
This supports a caller-authorized OS alias such as `/var` on macOS; it neither
opens the final root nor grants directory authority. The caller still acquires
the returned path through the no-follow helpers. Relative paths, traversal and
NUL are rejected before alias resolution. Root acquisition itself never opts into
alias resolution implicitly.

File opens are read-only, close-on-exec and nonblocking. They return a native
file handle; callers must check the opened handle's type before reading content.
Opening a FIFO cannot wait for a writer. File helpers do not read file bodies
or allocate background work. Callers own input length,
read bounds, cancellation, authorization and directory-handle lifetime.

Windows read-only acquisition accepts an uppercase local drive root on fixed
NTFS, with exact long-name spelling for every existing component. It rejects
UNC/device/verbatim input, traversal, ADS, DOS device names, trailing dots or
ASCII spaces, and all reparse points. Native I/O adds the verbatim prefix only for the
drive-root open. Subsequent `NtCreateFile` calls are relative to the retained
parent, with non-inheritable handles and read/write/delete sharing.
[`OBJ_DONT_REPARSE`](https://learn.microsoft.com/en-us/windows/win32/api/ntdef/ns-ntdef-_object_attributes)
rejects reparse traversal during name parsing;
[`FILE_OPEN_REPARSE_POINT`](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/nf-ntifs-ntcreatefile)
bypasses final-component reparse processing. Every successful open checks
`FileAttributeTagInfo` on the returned handle and rejects a reparse attribute,
the wrong directory/file type, or a mismatched long name before returning it.
Native UTF-16 names, including unpaired surrogates, remain lossless
`OsString` values inside this package; this does not define a wire path encoding.
Windows `resolve_absolute_root_alias` only validates the admitted spelling; it
does not resolve aliases or test existence, so `allow_missing` has no effect
there. Existence and filesystem admission belong to subsequent handle acquisition.

Windows enumeration reopens the retained directory through an empty relative
native name, verifies its volume/FileID, and owns a separate enumeration cursor.
`NtQueryDirectoryFileEx` fills one reusable, aligned 64 KiB buffer owned by the
iterator. Entries are exposed one at a time from that bounded batch; a new query
occurs only after the batch is consumed. Every record header, next-record offset,
8-byte alignment and UTF-16 name extent is checked before exposing an entry.
Iteration remains lazy and caller-cancellable between entries, with at most one
buffer prefetched. Enumeration does not provide a snapshot of concurrent changes.
Reparse entries
are classified as links, without opening or following their targets. No operation
reopens an absolute path after root admission.

Delete sharing permits renames supported by the filesystem; it does not promise
that NTFS permits an ancestor rename while descendant handles are open. Windows
directory creation sets an explicit TokenUser owner and protected DACL atomically
in `NtCreateFile(FILE_CREATE)`: the user, SYSTEM and Administrators receive full
control with child inheritance. The actual created handle's owner and DACL are
checked before returning its authority. Single-level creation refuses an existing
target; absolute creation preserves existing objects and creates only missing
components. A concurrent creation collision acquires the current component by
the same no-follow open: this operation created no object and has no earlier
identity receipt to compare. A replaced directory may be admitted as an existing
directory; a reparse point or wrong type is refused. It does not validate an
existing object's private-data policy or repair permissions. Callers must still
enforce their own admission policy.
Private-descriptor verification compares the native canonical SDDL of the expected
and observed protected owner/DACL, including ACE order and well-known SID aliases;
the informational `AI`
DACL flag added by Windows during a later protected-DACL assignment is ignored.
Inherited ACE flags and every actual ACE remain exact. It fails closed rather
than accepting every semantically equivalent ACL. Parent inheritance cannot add
ACEs to this protected DACL.
Privatizing an existing TokenUser-owned file requires metadata and DACL authority,
not write access to its contents. A reader or running executable may deny content
writes while still permitting this permission change and a same-volume rename.
Windows writable private-file acquisition uses the same retained parent and
no-reparse checks. Creation applies a protected TokenUser/SYSTEM/Administrators
descriptor atomically and verifies it on the returned handle; an existing file
must already have that exact private descriptor. Existing private directories may
have their DACL replaced only after their owner matches TokenUser. This never
changes an ancestor's permissions. Writable directory reopen remains relative to
the retained handle, verifies FileID, and propagates `FlushFileBuffers` failures.
File versions expose FileID, attributes, length, native link count, last-write and
change times from the same retained native handle. Credentials enforce a single
link themselves; Files does not ban CAS hard links. Callers own mutation admission and durability
receipts. A failed private-file creation may leave its created name.
Private file handles share reads/writes while excluding name deletion/replacement
until their owner drops them. Regular-file DACLs contain no directory inheritance
flags; directory DACLs retain child inheritance.
File replacement uses `FileRenameInformationEx` with replace and POSIX semantics:
delete-sharing readers retain the old object while new opens observe the published
file. Handles that exclude delete sharing still prevent replacement. Directory
publication and non-replacing publication retain their ordinary rename semantics.
The durable Windows creation entry point syncs each newly created directory and
the retained parent whose namespace it changed. An unchanged final directory is
not a newly published namespace; the consumer owns later child-publication flushes.
Existing higher ancestors are not mutated or reopened for write access. A failed
sync propagates and may leave a created prefix; it never confirms publication.
`NamespaceLease` requires the exact private TokenUser policy and pins a writable
directory handle without delete sharing. It also creates or admits the empty
private `.rsi-namespace-pin` file and retains it without delete sharing. The
directory cannot become empty while the lease is live, so NTFS refuses conversion
to a directory reparse point. Child publication can still obtain directory write
access. The guard is persistent owner state, contains no data, and is never
silently replaced or repaired.
It prevents another opener from changing the directory into a reparse point or
renaming/removing it while path-based native consumers are active. Its flush uses
that same handle. Consumers retain the lease through their last actual I/O.
Its explicit child-publication operation admits one source name, destination
name, regular-file or directory kind, and replacement policy. It checks both
entries without following reparses and renames through the retained writable
parent and exact source handle. Callers own mutation authorization and directory
durability; the lease does not enforce a process sandbox.
Pins of the same volume/FileID share one native owner within a process, so
independent connections retain the protection until their last actual I/O ends.
The weak registry prunes expired owners and admits at most 4096 live pinned
directories. Independent processes retain their own pins; consumer locks own
writer serialization.
Directory flushes can use that process-owned pin after revalidating its exact
private policy and retained identity; they do not reopen the pinned directory for
write or silently discard a synchronization failure.
Other platforms have no implementation. Consumers with a separate fallback must
describe its guarantees. This library does not supply a process sandbox
enforcement stamp or decide which sources become model context.

`Directory::entries` enumerates an already retained directory. Unix single-level
`create_directory_no_follow` accepts exactly one nonempty normal name, rejects an
existing target and opens the result without following a symlink. It never creates
parents. Callers own enumeration bounds and must retain their own cancellation,
authorization and absolute-path identity checks.

Run `cargo test -p rsi-files-native-fs` on the native host. Windows tests use
isolated fixed-NTFS temporary roots and create their own junctions without a
symlink privilege or changes to system policy. Unix tests retain their original
symlink and FIFO fixtures.
