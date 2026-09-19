# rsi-pty

An ordinary plugin over the native Process PTY contract. The provider admits at
most 256 scopes and 256 retained terminals. Aggregate reservations are 256 MiB
for screens, 64 MiB for snapshots, and 64 MiB for follower queues. Screens reserve
64 bytes per cell across normal/alternate visible grids and 1,000 scrollback
rows, plus 64 KiB overhead. Resize admits a complete replacement reservation
before changing native size or parser storage. Snapshots reserve 160 bytes per
visible cell plus 16 KiB before serialization, capped at 16 MiB.

Each follower retains at most 2 MiB of live output plus its admitted snapshot.
The generation admits at most 32 followers; detached shells need no follower
reservation. Follower reclamation follows the [family lease contract](../README.md).
Pages coalesce consecutive native chunks into at most 16 KiB of UTF-8 bytes;
one-byte echoes do not require one remote request each. Reads wait at most 200 ms and permit
one outstanding reader per attachment. Overflow starts a new snapshot stream
epoch; neither transcript acknowledgements nor a browser's claimed controller
state participates in this data path.

There is at most one admitted native input per terminal and 32 retained receipts.
A retry with the same sequence and digest returns the retained receipt; conflicting
bytes fail. Takeover and detach invalidate old controller epochs. Pending writes
block takeover until their native result settles. An unknown receipt never
re-executes its input. Provider retirement closes every live native handle and
waits for the screen reader to settle before releasing ownership.

Close and close-all share one scope cleanup gate. A repeated close cannot report
success while the first admitted close is still reaping its process. This gate
also orders scope retirement after already admitted close work.
Cleanup retains terminals until native reaping finishes. Cancelling a close
waiter leaves that ownership available to the next close or retirement call.
