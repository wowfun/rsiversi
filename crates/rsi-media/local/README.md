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
Garbage collection and reference counting are intentionally absent.
