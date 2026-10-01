# rsi-pty-protocol

Typed live terminal scope, bounded operation and attachment contracts. Scope
possession is trusted in-process authority. Product adapters authenticate and
narrow Session targets before using it. No type here is a durable Session record.

Read cursors count UTF-8 bytes in one attachment stream epoch. Snapshot pages
precede live output. Epoch changes reset the cursor and presentation; stale or
expired output cursors require a fresh snapshot. Input epochs are independent
controller authority. An input sequence is submitted at most once and its
bounded receipt distinguishes accepted bytes, pending work and an unknown result.
`PtyError::OutcomeUnknown` preserves a lost effect acknowledgement or worker result,
including creation and resize. Refresh live state or the original input receipt
before deciding whether to submit another operation; the error never authorizes replay.

The local scope exposes an infallible, non-I/O emptiness snapshot, including
in-flight creations and terminals still owned for cleanup. Product owners use it
after the last admitted operation to release empty scopes, including after a
failed or cancelled close. It does not reserve the scope against concurrent creation.

Native embeddings create from their explicit native plan. Location-aware products
use `create_execution` with the opaque prepared plan. `execute_with` supplies the
current caller's exact lease for input and resize; the resource checks that it
belongs to the original provider before native I/O. Scope possession remains the
trusted authority for bounded projection, attachment and cleanup operations, so
products must admit those operations before dispatch, including while offline.
