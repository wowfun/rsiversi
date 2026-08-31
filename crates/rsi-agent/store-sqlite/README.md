# rsi-agent-store-sqlite

SQLite and filesystem-CAS ordinary plugin for
`rsi-agent-store-protocol`. Opening the Store acquires one cross-process writer
lease for the entire root before schema validation or recovery reads. Only the
exact current schema is accepted; this pre-release implementation does not
migrate old layouts. Open validates SQLite integrity, foreign keys, mechanical
Fact watermarks, and the lowercase SHA-256 encoding of every stored Fact-prefix
digest without redundantly decoding all typed history or recomputing every prefix;
Kernel recovery owns paged header/Fact validation, while each CAS read validates
the exact requested body. Header and Fact reads project SQLite byte length and
return no TEXT body to Rust when that row exceeds its protocol bound. CAS publication never deletes caller or unrelated
files. It stages publication in a dedicated private directory that is reset
after the writer lease is acquired on open, so a process crash cannot retain
partial CAS files indefinitely.

The configured root must be on a filesystem that honors the host's exclusive
file locks, same-directory atomic rename, and file/directory sync semantics.
Network or shared filesystems that weaken those operations are outside this
backend's durability and single-writer contract; the Store does not infer their
behavior from a path string.

SQLite work and CAS file work use separate single-slot blocking admissions.
Hashing and immutable file publication do not hold the SQLite connection mutex;
metadata is checked or inserted only after the file phase completes.

On Unix, owned Store and CAS directories are created and tightened to mode
`0700` before database, writer-lock, or CAS files are opened.

The exact schema version 6 admits only the current mandatory Agent-preset
Header encoding, indexes Fact rows by turn, advances a Store-owned
canonical Fact-prefix digest with every append, and tracks which accepted
turns do not yet have a terminal Fact. Index maintenance is atomic with append;
SQLite also stores one bounded opaque Context checkpoint and its Fact-prefix
digest per session behind an exact durable-tail transaction, rejecting metadata
that differs from the Store-owned header and prefix. It does not interpret
checkpoint bytes, effect state, or choose a terminal outcome. Write-behind
timing, interruption recovery, and turn state belong to the Kernel. See [the
Agent architecture](../docs/architecture.md).

Checkpoint reads reassert the stored header fingerprint against the immutable
session header and reject a checkpoint cursor beyond the current durable tail.
They cannot compare the checkpoint's prefix digest with the session's current
tail digest after later Facts have been appended; Context remains responsible
for binding and validating the opaque checkpoint bytes and their exact prefix.
