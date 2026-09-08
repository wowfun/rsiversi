# rsi-terminal

Tool cards retain shared exact lifecycle metadata with their presentation blocks.
Suffix-only cards mark missing intent; backfill repairs the name and argument
source without regressing the observed phase. Focused-card details offer exact
arguments, structured result and rejection sources even when their preview was
evicted. Completed-output IDs are validated before retention.

The TUI extension-state action inspects the latest complete projection snapshot,
with bounded per-producer detail and separate failure text. Fresh drafts subscribe
without creating a message. A snapshot replaces the previous value and is released
on attachment replacement. Line-mode text suppresses unsolicited projection
payloads; JSONL emits typed replacement events. Projection failure does not stop
core history observation.

Interactive applications expose the pinned Session command catalog: the TUI
action menu and line-mode `:commands`. Registered `/name arguments` inputs run
through the shared controller; unknown slash names remain Human messages for
workspace skills. TUI saved Sessions retain one bounded command invocation and
its last receipt. The command-result action and line-mode `:command-result`
query unresolved identities without replaying a mutation. New messages wait for
that command to resolve. A failed command preserves editable input.

Headless `--commands` prints discovery and its predecessor. `--command JSON`
accepts one complete closed SessionCommandInvocation, including the discovered
contribution identity, request ID, predecessor and JSON arguments. It executes
once and may precede an optional TASK or `--stdin` input in the same handle.
Without a task it reports only the command outcome; draft edits then end with
the process-local lease. `--command-status REQUEST_ID` queries a receipt without
invoking a callback. List and status are mutually exclusive with message options.
JSON is bounded before decoding; signals and application retirement stop local
waits and output while preserving admitted server ownership.

Explicit Retry queries the retained MessageId before sending any mutation. A
failed query preserves the unresolved request; only NotFound permits resubmission
of its frozen content and options.

DevicesFactory owns the finite `register LABEL`, `list`, and `revoke DEVICE_ID`
terminal grammar over a negotiated local ApiClient. It requires no Session,
Workspace or model capability. It uses the same exclusive terminal lease and
owned ApplicationRun entry as the other terminal plugins. Registration is the
only explicit output that includes a new credential; diagnostics and list do not.
On Unix device result delivery uses the same cancellable nonblocking output owner
as the line renderer, including descriptor restoration before terminal retirement.

Native CLI, Headless and fullscreen TUI applications consume independent Session,
Workspace, Models, Output and Media capabilities. They own argument parsing,
terminal input, presentation, signals and their application work. They have no
dependency on the standard composition, Service Host, Agent Kernel or Store
implementation. Shared submission and observation behavior belongs to
[rsi-client](../client/README.md).

The three ordinary application factories prepare their own arguments before
backend activation and publish ApplicationRun. The invoking Profile owns their
lifetime; application withdrawal stops presentation and drains its owned work.
Connection ownership is supplied independently, so terminal exit can display
whether the enclosing application will shut down its embedded service or detach
from a remote service. A terminal application never controls a service process.
On Unix, CLI output owns duplicate nonblocking stdout/stderr descriptors and
restores their flags before releasing the terminal lease. Cancelling a renderer
interrupts pipe backpressure and its channel wait; tracked worker completion
precedes clean withdrawal. Headless cancellation allows one second for both message completion and output
flush, then stops presentation while retaining exit status 130. The deadline
also covers a full render queue and a model that completed before the signal. This can leave
a partial final output record when the consumer does not drain its pipe.
On expiry the application aborts and joins its local message observer, including
a stalled terminal-Fact read. Accepted domain mutations retain their independent
owner; exit 130 does not assert that remote execution has reached a terminal state.

Line-mode SIGINT covers initial queries and all command waits. It allows one
second for in-flight work to reach input cancellation; another SIGINT or expiry
stops client reconciliation and exits 130. Healthy acceptance races still cancel
the accepted input and keep the interactive attachment. Interrupted exit bounds
renderer flushing separately to one second; admitted domain work is independent.

Interactive attachments are ordinary child Profiles owned by a bounded Shell in
the application generation. Each Profile publishes its terminal observation sink
and shared Session controller with fresh Local identities, inheriting the chosen
Session service. The renderer sink owns presentation, while the shared controller
owns submissions and all observation tasks. A fresh draft starts Fact/interaction
observation after acceptance and projection observation immediately. Switching closes the prior surface and rejects its later work;
dropped surface waiters are cleaned up by the Shell. No additional Runtime is
created for an attachment.

The product [terminal contract](../README.md#terminal-application) owns user-visible
behavior and presentation bounds. Input against an older rendered frame resolves
its Session identity and source anchors in the current projection; removed
sources are inert until redraw. Accepted-message detail uses a bounded JSON
window with an explicit truncation marker.
The shared [conversation source contract](../conversation/README.md) owns closed
Fact fields, raw UTF-8/JSON windows and Tool outcome classification. The TUI adds
sanitization and display/source mapping while preserving the raw window offsets.
Closing or replacing a TUI detail cancels its exact-source read in the shared
controller. Its view revision also rejects a result that completed concurrently
with replacement. Cancellation of this read has no effect on the running Turn.
The producer submits complete cell buffers and their source maps through a
coalescing channel. Only the output writer computes cell differences against its
last completely written frame. Interrupted or short writes never advance that
baseline or publish a new hit map. Identical cells emit no bytes; resize or a new
attachment generation forces a full repaint. Input uses the last acknowledged
source map for the current attachment generation.
Incomplete bounded escape sequences expire after 50 ms without input. Paste and
oversized-sequence quarantine retain their terminator rule across idle intervals.
On macOS the fullscreen input and output owners reopen the actual terminal
devices named by stdin and stdout, respectively, with independent nonblocking
descriptions. Darwin cannot register the `/dev/tty` alias with kqueue. Other Unix
targets open that alias directly. Neither path changes the standard descriptors'
flags; failure to open or register a terminal is reported with its I/O stage.
Line framing retries interrupted reads without discarding an accumulated prefix.
The [development tutorial](../../../docs/tui-development.md)
and [debugging reference](../../../docs/tui-debugging.md) explain isolated launch,
source tracing and visual evidence. Pure projection, argument and controller
tests live here; built-product CLI/PTY integration remains with the launcher.
The terminal-restoration PTY probe bounds captured output and explicitly stops
and joins its nonblocking reader after child exit. It never relies on the host
PTY delivering EOF to complete fixture cleanup. Failure teardown starts draining
before terminating the child, bounds its wait, and preserves a separate stage
file even when terminal output is blocked.
