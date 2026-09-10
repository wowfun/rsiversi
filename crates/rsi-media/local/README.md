# rsi-media-local

This ordinary backend plugin owns one explicit local immutable CAS root. Each
object is one bounded envelope below a digest-sharded directory. Publication
writes and syncs a private temporary file, atomically links that exact inode
into place without replacement, removes the temporary name, and syncs the
directory. The backend revalidates caller-supplied bytes before writing;
existing or concurrently published identities are accepted only when their
metadata and bytes match exactly.

Every read opens an unchanged regular file without following its final
symlink, then revalidates the envelope, reference, length, and SHA-256 digest.
The backend admits at most 64 I/O tasks before scheduling blocking work. Each
read reserves its bounded declared file length before allocation and rejects
length changes. Its 64 MiB read pool remains charged with the returned allocation's
last clone or slice, including the header retained by a canonical-body view.
Cancellation of an async waiter cannot release a running blocking read's charge.
Garbage collection and reference counting are intentionally absent.
