# rsi-process

The Process contract accepts an already confined invocation plus explicit
stdin bytes, a complete child environment, per-stream capture reservations,
and termination grace. Spawn either fails before publishing a process identity
or returns one `ManagedProcess` that owns its process-group lifecycle and two
raw byte readers.

The confined executable, working directory, and argv are revalidated at this
boundary against process-API NUL restrictions and the shared 4,096-item and
1 MiB sandbox-plan ceilings, including platform-native encoded path and
argument bytes. The complete environment accepts platform-native names and values, but names
must be nonempty, unique, and contain neither `=` nor NUL; values must not
contain NUL. These process-API invariants are rejected by the platform-neutral
request contract before any provider admission.

Readers use monotonically increasing whole-stream offsets. Each stream retains
only its requested tail: a read older than the retained window is marked
lossy and begins at the oldest retained byte. Reads preserve arbitrary bytes;
UTF-8 decoding and split-sequence handling belong to consumers. Process
outcomes contain only exit code or signal. Callers own timeout and cancellation
classification.

One provider generation admits at most 256 active managed groups, at most 4 MiB
per stream, at most 1 MiB of aggregate environment names and values per
request, and at most 64 MiB of aggregate capture reservation. Capacity is
checked before a process identity is published. An outcome is terminal only
after stdin delivery, the direct child, captured pipes, and every process still
in the managed group have settled. After the direct child is reaped, a group
that survives TERM, the caller's TERM-to-KILL grace, KILL, and a fixed bounded
post-KILL observation returns `SettlementTimeout` instead of a false terminal
outcome. That failure releases active-process admission; it does not claim that
an unkillable host task was contained. Capture remains reserved while the
owning `ManagedProcess` and its readable tail are retained, even after
settlement or failure.

An optional completed-output cache is a separate read-only Local contract,
`ProcessOutputCacheContract`; possession never grants spawn authority. It
accepts an opaque 32-character lowercase hexadecimal output identity and raw
byte offset, with a 16 KiB default and 64 KiB maximum page. Readers return raw
bytes and the next raw cursor. Process stream reads expose a completed identity
only after the entire stream was captured and closed. Missing references mean
capture is pending, disabled, or unavailable; they never imply an empty stream.
Identities are best-effort same-user cache references, may be evicted at any
time, and are neither Session archives nor per-Session access-control tokens.

A completed stream is at most 64 MiB. Pages must advance by their exact raw byte
length, stay within that total, and return a nonempty page before EOF. Remote
consumers validate this contract before exposing provider data.

Completed pages use immutable shared bytes. API clients transfer their received
buffer owner into the page; clones and slices retain the original receive lease
until the last byte owner drops. `ProcessError::Api` preserves transport failure
classification. Raw bytes and cursors remain independent of display decoding.
