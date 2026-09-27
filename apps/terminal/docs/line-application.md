# Line and Headless applications

The line application accepts ordinary text directly into the durable next-Turn
mailbox and `:steer TEXT` as immutable steering intent. It observes continuously
across Turns. `--list`, `--history SESSION`, and `--resume SESSION` select bounded
listing, history, and attachment. `:sessions`, `:attach SESSION`, `:history
[BEFORE]`, `:status`, `:agents`, and `:queue` expose durable inspection. Repeated
listing/history commands advance pages of 20 Sessions or 128 Facts and report
exhaustion without replaying the first page. Observation and live interaction
refresh retry independently with bounded backoff from 250 ms to two seconds.
Interaction subscriptions deliver scoped changes without polling unchanged
snapshots. Capacity failures remain retryable until detach; five consecutive
non-capacity observation failures end the attachment with a visible error
and a nonzero exit, even while stdin remains open. Interaction refresh failure
stops that watcher and explains that reattachment restarts it. Switching
Session first reads the new bounded snapshot/history, then stops the previous
observer before rendering the new attachment. A failed read preserves the
current handle, observer, cancellation ownership, and history cursor.
In text mode, Session query results go to stdout as indented JSON; live status
and errors go to stderr. JSONL mode keeps all events on stdout.

`:approvals`, `:allow SESSION ID`, `:deny SESSION ID`, `:questions`, and `:answer ID` expose live
human intervention. Answer mode collects one answer per prompt, accepting an
option number or free text; Ctrl-C abandons only that answer draft. Outside
answer mode, Ctrl-C cancels this client's accepted pending messages and current
Turn; `:cancel [MESSAGE_OR_TURN_ID]` explicitly permits cancellation of attached
work.
During attachment, queries, submission or cancellation waits, Ctrl-C allows one
second for the operation and cancellation to complete. If the wait remains blocked,
or on a second Ctrl-C, the line application exits with status 130. It stops client reconciliation, preserving the
printed Session/Message identity for status lookup; dispatched mutations may
still complete. It does not accept another message after this interruption.
`:output ID [OFFSET]` reads a completed cache page. `::TEXT` escapes a
leading colon. `:exit` or EOF detaches a remote client; an embedded owner shuts
down and interrupts active work while preserving accepted mailbox input.

One renderer owns output: model text uses stdout and status, Tool feedback,
and human prompts use stderr. JSONL version 5 emits only structured envelopes
on stdout, including live interaction snapshots. These snapshots report live
Host state; their absence in history never authorizes replay of a human wait.
Turn cancellation does not discard terminal Fact or Outcome envelopes queued for
the renderer. Detaching or stopping the renderer still ends presentation.

The `headless` application accepts one message, may independently upload repeatable `--image`
inputs before message admission, and has no answering UI. Unanswered interactions remain
pending until another attached client answers, cancellation, or Host shutdown.
The line reader has a bounded handoff; acceptance receipts always describe
Kernel-persisted input, and client memory is never a follow-up queue.
On Unix, each image path is opened no-follow and nonblocking before its handle is
verified as a regular file, so a FIFO, device, or final symlink cannot occupy a
blocking worker while waiting to be classified.

Headless exit status 0 means a completed turn, 1 means a submission or execution
failure, and 130 means signal cancellation. Interactive exit status 0 means the
client detached successfully; individual Turn outcomes appear in observation and
history. Interactive client or rendering failures return 1. Both applications
return 2 for command-line, Profile/catalog, or Host bootstrap failures before
acquiring the Session surface.
