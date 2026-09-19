---
name: Session-owned interactive terminals
comment: Restricted Linux PTYs with bounded projection and explicit input authority
---

## Problem

Agent Jobs capture finite process output but do not provide a controlling terminal,
interactive shell job control, or a Session-owned shell that survives Agent eviction.
Reusing transcript subscriptions would couple terminal progress to conversation ACKs.

## Decision

[Process](../../../../crates/rsi-process/README.md) owns native PTY handles and reaping.
[PTY](../../../../crates/rsi-pty/README.md) owns generation-scoped terminal registries,
ANSI/VT100 projection and bounded follower streams. The standard
[Session adapter](../../../../crates/rsi/session/README.md) authenticates the durable
Header and freezes the existing workspace/sandbox choice into the native plan.
GUI Rust owns output cursors, page acknowledgements, input sequences and uncertain
receipt reconciliation. xterm renders the independent stream and forwards bytes.

The supported native route is Linux Bash under Bubblewrap with ReadOnly or
WorkspaceWrite. A typed process-local PTY intent omits only Bubblewrap's new-session
flag after the native PTY establishes a controlling terminal. Ordinary pipe APIs
reject this intent. There is no unconfined or pipe fallback.

The latest DeepSeek Harness attachment becoming writer is useful evidence for
session terminal lifetime, but does not establish explicit takeover epochs or
input receipts. Those are RSI contracts: attachments begin read-only, takeover
advances an epoch, and a sequence identifies exactly one native input attempt.
A confirmed prefix permits forwarding only the remaining bytes; an unknown
receipt blocks input until an explicit successful takeover.

PTY confinement retains a directory handle across spawn and mount. A second
canonicalization would still leave a rename window. An opaque Sandbox plan owner
keeps native resources in the local provider while Process retains their lifetime;
the host proc-fd cwd is selected before exec. A fixed Bash launcher opens cwd as
descriptor 3 and execs the positional wrapper argv; Bubblewrap consumes that
handle through bind-fd. This fixes the plan-to-launch substitution window, but
older backend backports lack a post-mount inode check. Mount-internal races remain
a backend limitation, not a guarantee inferred from the flag's presence. portable-pty closes other descriptors, while direct
cross-namespace proc-fd mounting fails permission checks. This launch sequence
avoids inherited descriptor flag changes and unsafe pre-exec code. The handle
remains owned through native settlement. No persistent inode authority is inferred
from a saved Session path.


## Alternatives considered

Reusing Jobs would lose interactive controlling-terminal semantics and retain a
Turn owner. Sending raw output directly to xterm would leave unbounded control
strings outside Rust admission. Letting the browser own input receipts would
split native and Worker behavior. These alternatives were rejected in favor of
one Rust controller over an independently bounded terminal service.

## Consequences

xterm's DOM renderer writes dynamic font, cell and color CSS through style text,
which the existing `style-src 'self'` policy rejects. Its public document override
is scoped to the terminal and maps only those style writes to constructed CSSOM
sheets. Disposal removes those sheets. A hash-checked build patch corrects the
pinned 6.0.0 source's nullish/conditional precedence in `CoreBrowserTerminal.open`;
otherwise the viewport silently receives the global document instead of the
override. Upgrading xterm requires reviewing that exact patch. This retains the
document's CSP without intercepting global DOM methods.

vt100 0.16.2 reconstructs its supported visible ANSI/VT100 screen and modes; it
cannot restore arbitrary xterm application state. An incremental 8 KiB control
sequence filter precedes both parsers because vte's OSC retention is otherwise
unbounded. Screen, snapshot and follower reservations have aggregate admission;
queue accounting includes per-chunk metadata and spare allocation capacity.

Terminals and their IDs are live only. Restart intentionally loses them. Pane
closure and application teardown detach followers; explicit close, Session service retirement and Host
retirement terminate and reap shells. Closing the last terminal reclaims an empty
Session scope once admitted operations finish; detach retains live shells.
Read-renewed idle follower leases allow later admission to reclaim attachments
from abruptly destroyed clients. Lease loss never kills the shell, but suspended
clients must reattach and take control explicitly. Creation
authenticates the Header, while local live operations use that frozen scope.
Native spawn runs outside registry locks, with pending creations retained through
retirement. Snapshot replacement reuses old reservations so saturation cannot
prevent a slow follower from recovering.

Input uses Data admission. Output uses one-page Subscription streams, because an
idle poll can hold admission for 200 ms and must not occupy a scarce Data slot.
The Session client retains its finite read interface and bounds capacity retries;
Control admission remains unchanged. Definitive stale-controller rejection makes
input read-only without treating rejected bytes as an uncertain write.

## Evidence

Native Process tests exercise controlling TTYs, Bash job control, resize, policy
writes and provider retirement. Core tests exercise bounded parsing, Unicode,
slow followers, cancellation, receipts and stale controller fencing. A public UDS
Session test proves persistence requirements, Session isolation, unchanged Facts,
close-all, reconnect and restart ID loss. GUI controller tests cover exact partial
UTF-8 writes, lost receipts and idempotent output-page acknowledgement.

Native input deadlines must bound the write itself. Timing out a detached blocking
writer would allow old bytes to arrive after controller takeover. Process therefore
uses a nonblocking master, owns each write through a 500 ms readiness deadline,
and performs no deferred write after return. A narrow raw-fd borrow duplicates the
pinned portable-pty master while its owner is borrowed; subsequent I/O uses owned
descriptors. The native saturation regression fills a noncanonical input queue,
then verifies bounded completion, admission release, resize and provider reaping.
GUI writes use eight separate command slots so receipt polling cannot starve
unrelated commands. GUI output reads retain task/shutdown ownership under an independent 32-slot budget,
so idle polling cannot consume the ordinary eight-command admission budget.

Document and Worker admission use the same separate terminal read/write lanes.
Known-unadmitted bridge requests retain queued input through bounded-delay retries;
ambiguous writes still require Rust receipt reconciliation. Browser fixtures inspect
output at the Worker reply boundary before xterm, so filter assertions do not rely
on xterm independently discarding control strings.
