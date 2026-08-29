# rsi-agent-store-sqlite

SQLite and filesystem-CAS ordinary plugin for
`rsi-agent-store-protocol`. Opening the Store acquires one cross-process writer
lease for the entire root before schema validation or recovery reads. Only the
exact current schema is accepted; this pre-release implementation does not
migrate old layouts. Open validates SQLite integrity, foreign keys, and
mechanical Fact watermarks without redundantly decoding all typed history;
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

The exact schema indexes Fact rows by turn and tracks which accepted turns do
not yet have a terminal Fact. Index maintenance is atomic with append; SQLite
does not interpret effect state or choose a terminal outcome. Write-behind
timing, interruption recovery, and turn state belong to the Kernel. See [the
Agent architecture](../docs/architecture.md).
