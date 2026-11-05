# rsi-media-local

`LocalAttachmentReaderFactory` is an ordinary optional plugin configured with an
absolute `root`. It creates the namespace directory when absent, opens one
read-only `LocalAttachmentReader`, and provides `FileReadContract` and
`PublishedImagesContract`. Uploaders publish
into that same namespace; the reader never republishes objects or repairs their
permissions. Consumers resolve it when admitting a file or preparing a model call.

Normalized images occupy `objects/<first two digest characters>/<digest>` as
exact raster bytes. The publisher owns image admission and normalization. The
reader uses its retained directory and worker slot to open a regular file,
charge its bounded allocation, verify SHA-256, and inspect encoded type and
dimensions. Borrowed slices retain the read charge through their last owner.

`LocalAttachmentReader` also reads a verbatim file namespace. An
object is `files/<first two digest characters>/<digest>/<portable filename>`
below its explicit root. Publishers commit immutable exact bytes before handing
out a `FileRef`; the reader neither publishes nor repairs objects. This layout
also permits a publisher to hard-link filename aliases to a shared digest object.
Verification opens relative to a retained directory without following links,
checks the exact length and streams SHA-256 with a 64 KiB buffer on one blocking
worker. Concurrent reads wait for a bounded worker slot; dropping a waiting
verification signals its running worker to stop between chunks. Path resolution
performs no I/O. It does not promise access from another execution environment.
This read-only namespace does not require the private-writer DACL or exclusive
namespace pin used by the image publisher below, and never changes permissions.

Factory preparation preserves malformed configuration as `MetaError::InvalidInput`
and reports unavailable native capability as `MetaError::Activation`, the Meta
category for plugin preparation and activation failure.

This ordinary backend plugin owns one explicit local immutable CAS root. Each
object is one bounded envelope below a digest-sharded directory. Publication
writes and syncs a private temporary file, closes its writable handle, atomically links that exact inode
into place without replacement and removes the temporary name. Unix also syncs
the shard and its `objects` parent before acknowledging publication, including
retries of an existing identity. Activation syncs the CAS directory ancestry so
newly created roots also have durable namespace entries. Unix acquires that
ancestry component by component without following links (only a trusted first
root component may be a platform alias). It retains the `objects` handle; shard
acquisition, private modes, staging, publication, cleanup and synchronization
use retained directory authority. Replacing an absolute path cannot redirect
an active backend. Existing `objects` and shard links are rejected before their
targets can be read or chmodded. Windows requires exact
local NTFS no-reparse paths, creates private objects atomically, and flushes each
changed directory through retained handles. Its private CAS root retains a
NamespaceLease through every admitted blocking operation. The `objects` directory
and each writing shard retain their own namespace pins; child creation, immutable
link publication, staging cleanup and synchronization use these retained parents.
Existing shards must already be private; their permissions are not repaired. Higher existing
ancestors remain unchanged. Other platforms reject before filesystem effects. Failure
after linking, or loss of the put worker's result, returns the common API
`OutcomeUnknown`; an already published object is retained. A retry of the same
identity validates it and establishes a new publication receipt.
The backend revalidates caller-supplied bytes before writing;
existing or concurrently published identities are accepted only when their
metadata and bytes match exactly.
Configuration absolute-root syntax is checked without I/O before platform
admission: malformed configuration returns `InvalidInput`, while valid
configuration on an unsupported platform returns `Unsupported`.

Every read opens a regular file without following any internal directory or file
symlink, then revalidates the envelope, reference, length, and SHA-256 digest.
Reads, publication and activation classify rejected links or non-directory
components as `Corrupt`. Native backends share the immutable object publication
algorithm; platform code owns root acquisition, private permissions and the
namespace handle used for publication and synchronization.
The backend admits at most 64 I/O tasks before scheduling blocking work. A
put checks reference shape and declared length before admission, then verifies
the digest on the admitted blocking worker before touching durable objects.
When admission is full, a shape-valid put returns `AdmissionFull` before checking
its digest; malformed reference shape or length still rejects before admission.
A read reserves its bounded declared file length before allocation and rejects
length changes. Its 64 MiB read pool remains charged with the returned allocation's
last clone or slice, including the header retained by a canonical-body view.
Cancellation of an async waiter cannot release a running blocking read's charge.
Garbage collection and reference counting are intentionally absent.
