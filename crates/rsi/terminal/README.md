# rsi-terminal

The machine-readable JSONL envelope is version 5. It retains internal model
events with their authenticated purpose tags. Plain answer output excludes
context-compaction text; the TUI renders it as an internal status block.

The resident application owns raw terminal modes, input decoding, the sole output
writer, signal and unwind restoration, Session controllers and editable drafts.
The [terminal presentation library](../terminal-ui/README.md) owns grapheme
editing, bounded transcript windows, wrapping, styling and cell layout. Its render
input borrows presentation values; it cannot invoke a backend or install process
hooks. Completed writes publish their matching source map together with the frame.

TUI configuration is null for the linked presentation, or a closed object with
`presentation` containing an ordinary Profile. The resident application starts
that Profile beneath its own Context and follows the enclosing application's
immutable catalog source. The child must publish `rsi.terminal.presentation`.
`rsi.terminal.ui` provides the linked implementation; `rsi.terminal.portable`
imports the explicitly required `rsi.terminal.render` service. Renderer replacement
never reruns ApplicationRun or changes terminal ownership.
This is a lifecycle boundary for operator-selected Profiles and trusted native
code, not a plugin sandbox. Presentation Profiles may compose dependencies from
the application catalog; the resident controller does not transfer its terminal
descriptor or Session controller through the rendering contract.

Portable frame exchange has a two-second deadline, including sending the scene.
Expiry cancels the read and enters the resident diagnostic-frame and 250 ms retry
path; terminal input and draft ownership remain resident. This bounds the local
wait, not execution or destruction of trusted native code. Linked synchronous
rendering still requires bounded, non-panicking implementation work.
Withdrawal without a replacement renderer and an already poisoned linked
renderer enter the same resident diagnostic/retry path. A later valid publication
resumes rendering without replacing the Session or losing resident drafts.
The terminal guard restores modes when its owner unwinds; caught panics in other
work do not retire the writer. Process aborts cannot guarantee restoration.
INT, TERM, HUP and QUIT listeners are installed before entering raw terminal mode;
each requests the ordinary output restoration and presentation shutdown path.
Keyboard Ctrl-C in raw mode remains a cancel action for the current turn.
Terminal output restoration and presentation disposal are both awaited even if
either cleanup reports an error.

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

Headless message delivery admits its claimed-Turn envelope to the renderer before
starting live interaction observation. Output backpressure or a stopped renderer
cannot start that watcher before its Turn is visible in the ordered event queue.

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

The TUI New action retries API capacity failures from the idempotent Workspace
`get_or_create` operation with at most five attempts for the same directory and
50/100/200/400 ms delays inside its owned work. Other failures, including unknown
mutation outcomes, return immediately. Session creation is attempted only once;
failure preserves the current attachment and draft.

DevicesFactory owns the finite `register LABEL`, `list`, and `revoke DEVICE_ID`
terminal grammar over a negotiated local ApiClient. It requires no Session,
Workspace or model capability. It uses the same exclusive terminal lease and
owned ApplicationRun entry as the other terminal plugins. Registration is the
only explicit output that includes a new credential; diagnostics and list do not.
Its `configuration list`, `configuration grant DEVICE_ID REVISION` and
`configuration revoke DEVICE_ID REVISION` commands use the same Local connection.
The revision must come from the durable grant snapshot; changes execute once and
unknown outcomes require an explicit fresh list. Registration itself grants no
configuration authority.
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

When the connected Files capability is supplied, each UI surface also composes
the independent [Files browser](../session-files-ui/README.md) state plugin. Its
Local mapping and snapshot belong to that same surface. The generic action menu
renders the contribution's directory, text and hex pages without another observer.

The product [terminal contract](../README.md#terminal-application) owns user-visible
behavior and presentation bounds. Input against an older rendered frame resolves
its Session identity and source anchors in the current projection; removed
sources are inert until redraw. Accepted-message detail uses a bounded JSON
window with an explicit truncation marker.
The shared [conversation source contract](../conversation/README.md) owns closed
Fact fields, raw UTF-8/JSON windows and Tool outcome classification. The TUI adds
sanitization and display/source mapping while preserving the raw window offsets.
Closing or replacing a TUI detail cancels its pending source, output, message,
or child-history read. Exact-source reads remain owned by the shared controller;
other detail reads drop their local I/O future. Its view revision rejects both
results and errors completed concurrently with replacement. Closing a view has
no effect on the running Turn or an admitted mutation.
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
The [development tutorial](docs/tui-development.md)
and [debugging reference](docs/tui-debugging.md) explain isolated launch,
source tracing and visual evidence. Pure projection, argument and controller
tests live here; built-product CLI/PTY integration remains with the launcher.
Tests that override process-global terminal rendering settings run in isolated
child processes. Writer acknowledgment and byte-comparison scenarios have bounded
waits; changing another test's color mode cannot alter an in-flight byte oracle.
The terminal-restoration PTY probe bounds captured output and explicitly stops
and joins its nonblocking reader after child exit. It never relies on the host
PTY delivering EOF to complete fixture cleanup. Failure teardown starts draining
before terminating the child, bounds its wait, and preserves a separate stage
file even when terminal output is blocked.

Body layout caching belongs to the [presentation library](../terminal-ui/README.md).


InspectorFactory owns the finite `runtime [AFTER_FIBER]`, `profile [OFFSET]`,
`factories [OFFSET]` and `native` grammar. It prints a bounded JSON page through
the same cancellable output owner as device administration and retains no live
Session. The response carries the next cursor; each invocation reads one page.

NativeAddonsFactory owns the finite `refresh` grammar over the local operator
ApiClient. It uses the shared terminal lease, cancellation and bounded document
delivery. A missing result never triggers automatic replay: inspect native state
before another explicit attempt. The command needs no Session or model.

`ManagementWriter` gives finite management documents the same exclusive terminal
lease and cancellable nonblocking Unix output primitive as operator applications.
It admits one frame at a time (caller-selected maximum, at most 4 MiB), retains
its lease through a dropped write waiter, and restores descriptor flags before
tracked completion. Closing or dropping the writer cancels delivery; explicit
close joins work. The caller owns text sanitization and the document format.
