# rsi-terminal

`/export [PATH] [-f markdown|md|json] [-i INCLUDE]` exports the selected native
Session without submitting a message. Quoted paths are supported; the default
file is `rsi-session-<safe-id>.md` (or `.json`) in the client working directory.
Line mode accepts `:export` with the same arguments. Scripted export uses
`rsi --profile cli --export SESSION|latest [-f FORMAT] [-i INCLUDE] [-o PATH]`:
Each short or long export option accepts a separate value or `=VALUE`.
without a path the artifact goes to stdout and status/errors go to stderr.
`latest` selects the newest durable root in the selected workspace. Export is
exclusive with creation, resume, list, history and `--output` reporting modes.
See the [shared export contract](../session-export/README.md) for include names,
diagnostic precision, streaming and file replacement behavior.

`/profiles` opens reviewed Host leaf management from Home or a Native conversation.
It uses the same Rust workbench as Settings → Plugins, preserving the conversation
draft while editing. Source selection, exact Local-issued grants, configuration
preparation, review acceptance and original receipt queries are explicit actions.
The bounded JSON editor passes text to Rust; leaving the panel does not cancel an
admitted source operation. Saved source and runtime application have separate labels.

`/attention` is available at home and within native conversations. It reads the
Host's bounded activity view, prioritizes exact pending requests and opens the
selected Native or External conversation. Native requests retain Session/Turn
identity; ACP permissions retain connection generation and request identity.
Reading positions are acknowledged after attachment. Escape detaches the view;
refresh never restarts execution. The menu has at most 257 choices and reports
truncation. It does not continuously watch hidden historical conversations.

`/external` opens configured external conversations from either the unattached
home or a native Session. It uses the shared external controller and an application
scene with no invented native Session header. Enter sends one text prompt; Ctrl+P
opens exact permission options, history and explicit peer controls. Escape detaches
the external view and retains its draft within this TUI lifetime. External menus
page saved conversations and accept mouse choices only from their current rendered
revision. Source windows advance by raw bytes, including UTF-8 boundaries. The Host retains
the peer. Only Close peer ends that connection. Native model, Goal and preset
controls remain with the native Session view.

The resident input loop captures a bounded scene snapshot. JSON encoding and
linked decoding, source validation, layout and cell rendering run on blocking
workers. Home and attached Sessions use the same render job. Each loop admits
one job at a time; cancellation and attachment/presentation/revision checks keep
obsolete results from replacing the acknowledged frame. Blocking work already
running may finish after cancellation, but cannot publish its result.

The [interaction design](docs/tui-design.md) owns attached layout, transcript,
model selection, navigation and information visibility contracts.

TUI owns terminal input and configuration screens before a Session is attached.
Absent defaults or unavailable default routes leave an editable home screen with
history and exit actions. Catalog read failures also keep this screen available,
with an explicit diagnostic and setup refresh action. One catalog scan reads at
most 4,096 routes; exceeding this bound fails explicitly. The setup controller
retains the complete menu and projects a 256-row window around selection into
each scene, so maximal route names plus provider actions fit the display budget.
Resume uses the durable
Session's frozen settings. Home Ctrl+C exits, including when a draft is present.
If configuration disappears after the startup check, a typed `SetupRequired`
from Session creation also returns to home. Other creation and resume errors keep
their error classifications.
Application commands are `/help`, `/login [deepseek|openai|openai-compatible]`,
`/model`, `/effort`, `/markdown [on|off]`, `/new`, `/resume [session_id]`, `/quit` and its `/exit` alias. They
bypass message submission, prompt recall and Session command receipts, and shadow
same-name Session commands. Recognition requires raw single-line input and a
cursor at the end; invalid arguments retain the draft. Multi-line input starting
with other reserved names and `//` input are literal Human text in both dispatch layers.
`/export` rejects multiline arguments and never submits them as Human text.
Editing and submission keys follow the [interaction design](docs/tui-design.md).
`@name` opens Agent completion. Ctrl+P → “@ Workspace file · browse and insert
path” opens the separate file picker on all supported key decoders. A decoded
Alt+@ is an additional shortcut; whether a terminal emits it depends on its
keyboard configuration. Both picker paths insert text, without adding file contents.
Exit has no confirmation; current and retained process-local drafts and undo
history are discarded. Session switching retains them.

The input-driven command popup is independent from actions. It ranks exact,
prefix, then substring matches and names, shows descriptions, and offers login
provider arguments. The unfiltered popup keeps application actions in their
curated order and hides the exit alias; typing it still completes and executes it. Up/Down select, Tab replaces only the current token, Escape
hides until another edit or Tab. Enter completes and executes application commands;
a partial Session command is completed only. Fully typed Session commands retain
the shared invocation-time predecessor checks. Session descriptors are bounded
display snapshots: fetch on each popup/help opening, coalesce invalidation during
control progress, command completion and publication, and reject obsolete results.
Read failures clear Session candidates while retaining application commands.
Help has a filterable list and paged details, bounded to 64 KiB overall, 256-byte
previews and 8 KiB detail pages. Recent Session IDs can be copied with Ctrl+Y.

Login proceeds through provider, existing connection (when present), compatible
endpoint (when needed), credential status, searchable models and missing limits.
Official endpoints and default names need no confirmation; connection settings
remain editable. Known name conflicts are resolved before credentials. The final
provider write retains its CAS check. Catalog failures do not block login.
Credential status distinguishes saved, environment, missing and unavailable,
and exposes editability. Only editable credentials show a masked input. An
unavailable store shows recovery guidance and Enter retries status without a
write. File failures display a safe category and recovery instructions. Saved
and environment credentials permit masked replacement; empty Enter reuses the
confirmed source without copying it to disk. The page shows the connected Host
store location. Environment changes require restarting the owning Host.
Keys use a dedicated zeroizing 64 KiB allocation with a fixed mask; they never
enter scenes, ordinary editor journals, clipboard operations or diagnostics.
Paste trims surrounding whitespace and rejects internal control characters;
Ctrl+U clears the key. Escape goes back, root Escape closes, Ctrl+C closes only
setup even during a running turn. Leaving secret input erases unsaved bytes.
Validation precedes advancing; field errors clear on editing and transitions,
while save receipts remain independent. Discovery offers retry, key/connection
repair and manual entry. Changing connection inputs retires prior reads.

Integration credentials opened from Plugins are independent of model login.
Escape returns directly to Plugins; an admitted write remains owned until its
result arrives, and reopening never replays it.

The shared [setup owner](../workbench-ui/README.md) persists through Host APIs.
Known capacities complete selection immediately; missing capacities are editable
in both directions and validated together. Attached Enter changes subsequent
Step selection; Ctrl+S also saves startup default. Home selection saves the
default and starts a Session, preserving the draft until explicit submission.
The application retains at most one pending setup operation independently of the
visible wizard. Closing cancels reads but retains an admitted write waiter; its
completion after close records results without initiating discovery, selection or
attachment. Changing the attachment closes setup and prevents its model selection
from reaching the successor Session. Reopening cannot overwrite or replay it.
Refresh reconciles conflicts
and unknown outcomes. This pending state is process-local; restart reads the real
configuration. Rendering uses the shared application/Session scene protocol.



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

The [interaction design](docs/tui-design.md) owns visible behavior. The
[presentation contract](../terminal-ui/README.md) owns scene and cache bounds. Input against an older rendered frame resolves
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

## Controller lifecycle and retained state

Both stdin and stdout must be terminals; rejection precedes Host bootstrap.
Native input currently requires Unix. The terminal application owns input,
rendering, signals and cleanup. An attached Session has one durable observer and
one live-interaction observer. Child browsing uses finite reads and does not
acquire execution. Switching rejects unresolved sends and fences prior results.
At most 12 client requests are outstanding, with reserved submission and
cancellation capacity. The client owns at most 1,024 pending message identities.
Renderer failure preserves the Session and editor, displays a resident diagnostic
without source authority, and retries at most once every 250 ms.

Each draft contains at most 1 MiB UTF-8. Draft and undo/redo buffers share a 4 MiB
budget across the current Session and at most 64 saved Sessions; insertion that
would exceed a bound leaves the prior draft intact. Each editor retains at most
128 changes and 1 MiB of inserted/removed text. Old changes yield first; an edit
larger than the journal resets it without rejecting otherwise valid input.
Submission clears that editor's journal. Saved Sessions retain their own editor,
model/effort display choice, receipts, source anchor and fold preferences.
Recomputable folds have at most 512 entries per Session, 4,096 across current and
saved Sessions, and 1 MiB including key and container allocation capacity. Old
saved preferences yield before current ones; eviction never discards a draft or
receipt.

Prompt recall keeps at most 100 inputs and 1 MiB across Sessions. It stores exact
frozen local request text, including failed and unresolved requests, and skips
consecutive duplicates within a Session. It is memory-only, restores no image
attachments, and selecting an input creates one undoable edit without submission.

Initial history reads 128-Fact pages at a captured watermark, stopping at the
latest Turn start or after eight pages / 1,024 Facts / 16 MiB of returned encoded
bytes. This threshold stops further prefetch, not an already returned page.
Historical Session restoration starts from its saved source anchor. The live
projection retains 512 blocks, 4 MiB of text and 8 MiB of metadata including
container capacity. Each block has a 256 KiB text window. Backward browsing owns
one separate projection with the same bounds and evicts newer blocks; observation
continues into the live projection. End restores that projection and fences older
history reads. Legal large Facts and transport pages are transient allocations;
these retention budgets are not RSS limits. Fact leases drop after projection.

Dynamic text replaces controls other than newline/Tab, and Unicode bidi controls,
with U+FFFD before rendering or copying. Layout expands tabs and uses narrow
ambiguous-character width. Text never becomes terminal escape instructions. A
single writer owns output, including OSC52, and restores terminal modes before
ordinary diagnostics. Panic cleanup is best effort and cannot cover SIGKILL.
SIGINT requests cancellation; SIGTERM and SIGHUP exit. Oversized bracketed pastes
are rejected as a whole and discarded through their terminator. Native clipboard
helpers use bounded process operations; OSC52 has a separate 32 KiB encoded limit.
Delivery is reported as confirmed, unverified or failed. Completed output can only
be read through its issued best-effort cache identity.

Clipboard work remains outside input handling, including the unattached home
screen; closing that screen cancels its pending clipboard operation. Setup mouse
hits bind to the exact menu contents as well as the current filter and selection.

Contributed cards share the closed text/field/form/button contract with Web.
Opening another detail cancels its reads. Menu open/close retains the visible
card and its field draft; selecting another view replaces it. Contribution or
surface retirement rejects new actions and drains admitted work. The ordinary
[tree inspector](../session-tree-ui/README.md) supplies finite read-only tree and
conversation details. Full attachment uses the Session navigation behavior in the
interaction design. Remote exit detaches; embedded exit shuts down its owned Host.

When discovery advertises output capacity equal to the context window, setup
asks for a smaller execution output limit before saving. Discovery preserves the
advertised fact; setup never silently selects a reserve that leaves no input room.

Idle slash completion retains its menu until the editor or command catalog changes.
Fold-budget eviction scans retained keys a bounded number of times, removes an
oldest Session's excess entries as one batch, and shrinks each visited allocation
once. Draft text and pending submission identities survive fold-cache eviction.

Successful command-catalog notices remain visible without triggering a retry on
every edit. Only failed reads retry on edits; reopening or explicit refresh
requests a new catalog.

The TUI completion display retains at most 64 KiB of catalog text and resource
coordinates, including skills. Omitted entries produce a visible notice. Filtering
and help frames share immutable entry strings and skill metadata.

The Actions menu's **Session terminals** lists the Session's live Bash terminals
with exit/controller status. A selected terminal can be closed explicitly;
**Close all terminals** reaps every terminal in that Session. The TUI does not
embed terminal emulation or forward its own terminal input into these shells.

Skill completion uses the [workspace reference parser](../../rsi-agent/workspace-context/README.md),
including cursor-local tokens in multiline drafts. Dollar completion contains
only skills and replaces only the active token. Colliding skill names remain
reachable with `$name` or `/skill name`.
`/markdown` toggles assistant and expanded reasoning rendering; `on` and `off`
set it explicitly. This presentation preference starts enabled and lasts across
Home, Session switches and renderer replacement in this TUI process only.

Fresh, resumed, forked and `/new` Sessions discover project instructions and
skills from their selected workspace by default. `--trust-workspace` is not an
application option; after `--`, it remains ordinary application input.

`/history <session-id|external:id> <query>` opens lexical conversation text search
in the current draft's workspace. Coverage and omissions remain visible before
matches. Explicit actions advance one indexing batch, open and page an original,
and select a displayed page or one of its first 128 nonempty lines for freezing.
The reference preview then offers the ordinary Add-to-draft action. Querying or
reading never sends a model prompt; the existing draft remains owned by its
Session. This text search is separate from recent-session metadata navigation.

The [service UI client](../service-ui/README.md) contributes Service extensions
to actual Session surfaces with a negotiated API connection. Its standard views
reuse the terminal form editor and generation-fenced actions.

Opening an external attention target retains the attached conversation even if
recording its read position fails; the status shows that failure independently.
