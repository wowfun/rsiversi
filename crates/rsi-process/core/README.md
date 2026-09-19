# rsi-process

`ProcessOutput::peek_tail` copies only the requested newest raw bytes, within
1..=32 KiB, under the capture lock. It preserves whole-stream offsets and the
best-effort completed-output identity without waiting or changing process state.

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

This is a trusted in-process Local capability. A `ConfinedProcess` is a typed
plan, not an unforgeable authorization token: its holder must forward the exact
Sandbox-produced plan. Process validates spawn framing, not the authenticity of
the issuer or the truth of a caller-supplied enforcement stamp. Untrusted callers
must enter through the policy-owning product boundary, not receive Process.

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

`DuplexProcess` is a separate Local contract for ongoing byte protocols. Its
request has no batch stdin. It retains a bounded lossless stdout queue, a bounded
stderr tail and explicit persistent stdin. Reads consume stdout once in order;
queue saturation backpressures the child instead of dropping bytes. Each read or
write is at most 64 KiB; overlapping reads/writes on the same stream reject as
capacity rather than creating unbounded waiters. A write reports actual bytes
accepted and must not be blindly replayed after cancellation. Closing stdin is
explicit. Dropping the final managed duplex handle starts termination; clones
share one handle lifetime, while retained byte ports remain readable through
settlement without keeping the child alive. JSON framing and RPC identity belong to the consumer.

Duplex stdout EOF means that the pipe closed and its buffered bytes have been
delivered; the child and stderr may still be alive. Stream failure follows any
already buffered bytes and never becomes successful EOF. Whole-process `wait`
independently reports reaping, stderr and drain failures. A stdout half-close
does not itself terminate a generic child or release process admission.

Duplex and batch processes share the same provider's 256-process and 64 MiB
capture admission. Output capacity remains reserved while handles/readers retain
it. Termination and provider retirement unblock protocol pipes, terminate the
managed group and reap the direct child. A pipe that cannot drain within the
explicit grace reports an error, never lossless EOF. The same Unix process-group,
descendant-escape and host-crash limitations apply to both contracts.

PTY intent is explicit, typed and process-local. Ordinary pipe execution rejects
PTY plans. The Linux local PTY path consumes the
[restricted Sandbox PTY plan](../../rsi-sandbox/README.md).
portable-pty establishes setsid and TIOCSCTTY before executing that wrapper.
Process owns native I/O, resize, termination and reaping; live terminal state
belongs to [PTY](../../rsi-pty/README.md).

PTY request validation checks framing and the supported enforcement stamp, not
the authenticity of an arbitrary Rust-constructed plan. The trusted consumer
must obtain the exact plan from Sandbox; neither a stamp nor an opaque plan owner
constitutes proof of confinement.
