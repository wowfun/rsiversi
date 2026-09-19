# rsi-pty

This family owns live terminal scopes, bounded ANSI/VT100 projection, attachment
streams and explicit controller transfer. The Process family owns native PTY
allocation, I/O, resize and reaping; Sandbox owns the confined plan. Session
identity, authorization and workspace binding belong to the standard product.

Each scope admits at most eight terminals. Dimensions are 1..=500 columns and
1..=200 rows, with at most 1,000 scrollback rows. Screen and snapshot reservations
are admitted before allocation against aggregate byte bounds. Screen admission
uses each terminal's maximum observed rows and columns until retirement, because
vt100 retains old-width scrollback and row capacity after shrinking a window. Each attachment
has a bounded 2 MiB output queue; input is at most 64 KiB. The stream is separate
from transcript acknowledgement. Slow followers must reconnect to a fresh
bounded snapshot; they cannot grow retained output indefinitely.

Before either Rust's parser or the browser sees output, one incremental UTF-8
and control-sequence filter withholds incomplete sequences, caps each at 8 KiB,
and discards oversized sequences through their terminator. All OSC, DCS, APC,
PM and SOS strings are discarded, including bounded title, palette, hyperlink
and clipboard commands; output cannot alter browser or application state. UTF-8 continuation
bytes are not mistaken for C1 controls. An unterminated control string suppresses subsequent output until its terminator
or CAN/SUB cancellation; the byte cap bounds memory, not that duration.
Screen reconstruction covers the supported
ANSI/VT100 visible screen and input modes, not arbitrary xterm application state.

The creator controls input; additional attachments begin read-only. Explicit
takeover increments a controller epoch. Stale writes, resize and detach cannot
affect a newer controller. Serialized input uses sequence identities and bounded
receipts reporting accepted bytes, never command completion. An unknown receipt
must not be blindly replayed. Detach releases follower queues and snapshots even
while an admitted native write is pending. That write retains its old-epoch
receipt and admission until settlement; takeover remains blocked until then.
Request controller and stream epochs, and input sequences, are positive.

Explicit close/close-all and scope or provider retirement terminate and reap
PTYs. Pane closure and follower disconnect only detach. The standard product
retains a scope across Kernel residency changes and retires it with its Session
service generation. Process restart leaves old terminal IDs unavailable.

Create and attach reclaim followers only when attachment, queue or snapshot
capacity is exhausted. Per-terminal exhaustion scans only that terminal; shared
queue or snapshot exhaustion may scan the bounded generation. Followers that
have not read output for 60 seconds are eligible. Reads renew this lease; abandoned
clients cannot permanently exhaust attachment capacity. Reclamation releases the snapshot,
queue and controller authority without killing the shell or discarding pending
input receipts. A suspended client may need to reattach and explicitly take control.

Input requests carry exact byte arrays, rather than UTF-8 strings, because
a successful native write can stop within a multibyte character. A receipt
reports only the accepted byte prefix. No shell completion is inferred.

Creation reserves capacity under short registry locks, then spawns outside those
locks. In-flight creations count toward capacity; retirement fences new creations
and waits for admitted spawns before reaping every resulting child.

Snapshot replacement can reuse reservations held by the snapshots it replaces.
Retained old text is released before constructing its replacement, including shared
snapshots; capacity failure before replacement leaves the existing stream intact.
Refreshing a shared snapshot resets every follower using that snapshot to a new
stream epoch, so no attachment continues reading obsolete bytes.

Creation requires an entered Tokio runtime. The provider checks this before
reserving a screen or starting the native child and reports unavailable otherwise.

The provider generation's 64 MiB follower queue reservation admits at most 32 attachments
at 2 MiB each, independently of the per-terminal and per-scope limits. Admission
returns Capacity when any of these bounds is exhausted.
Detached shells still count toward the independent 256-terminal generation bound.
Closing a group terminates every member before awaiting their reaping concurrently.
