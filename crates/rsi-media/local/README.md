# rsi-media-local

This ordinary backend plugin owns one explicit local immutable CAS root. Each
object is one bounded envelope below a digest-sharded directory. Publication
writes and syncs a private temporary file, closes its writable handle, atomically links that exact inode
into place without replacement and removes the temporary name. Unix also syncs
the shard and its `objects` parent before acknowledging publication, including
retries of an existing identity. Activation syncs the CAS directory ancestry so
newly created roots also have durable namespace entries. Other platforms acknowledge file sync and atomic namespace
publication without promising directory durability across power loss. Failure
after linking, or loss of the put worker's result, returns the common API
`OutcomeUnknown`; an already published object is retained. A retry of the same
identity validates it and establishes a new publication receipt.
The backend revalidates caller-supplied bytes before writing;
existing or concurrently published identities are accepted only when their
metadata and bytes match exactly.

Every read opens an unchanged regular file without following its final
symlink, then revalidates the envelope, reference, length, and SHA-256 digest.
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
