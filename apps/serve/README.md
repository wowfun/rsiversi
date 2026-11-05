# rsi-serve

The selected application's Tool listener can prepare and wrap a native Tool on a
dedicated authenticated loopback WebSocket. One invocation owns the connection.
Before the durable intent the application may request one approval with its
reason, deny the invocation with a `denied` frame carrying its rendered reason
and any deferred Tool messages from the completed policy,
supply a successful answer, or reach its one native
continuation. An application ask and native approval share the exact prepared
process review. The Host enters that continuation only after publishing the
Tool start. Disconnect cannot authorize another start.
The native owner attaches enforcement after the application finishes. If only
the presentation or deferred inputs change, the application returns those fields
alone. The owner preserves the exact native JSON value, including wide integers.
Native middleware borrows the admitted Tool's lifetime. The separate admission
for application-owned Tools stays available while native bodies or their human
interactions are pending.

Application inbox `wait_idle` delegates to the attached Session's read-only native
idle wait. It retains the request lifetime while waiting and creates no inbox
snapshot, message resource, polling timer or per-Turn cancellation request.

Application inbox `wait_draft` waits until the supplied draft revision changes or
the Session is published, using the standard Session draft subscription. It
retains no inbox snapshot and does not extend the draft lifetime. Request
cancellation releases the observation; expiry and access failures remain errors.

Application inbox `retain_draft` binds an exact native draft owner to the
application callback and a caller-selected key. `release_draft` releases only
that Session/key pair. All draft owners share one callback resource, with one
entry per Session; replacing a key retires the previous entry, and inactive
generations are pruned on acquisition. Thus native draft admission bounds live
entries without consuming a snapshot slot per Agent. Callback retirement drops
all owners. These owners prevent idle expiry, not explicit discard or shutdown.

The application inbox's `discard_draft` operation forwards selected message
identities to the native draft owner and returns its resulting revision. It
does not manufacture durable slots or receipts for unpublished inputs. Published
queue withdrawal continues to use the native operation receipt, including claim
races and execution-owner rejections.

Serve owns connection negotiation and Web asset operations for its entire
generation. Closing or retiring a local registration reports that owner's state;
it never redirects those operations to a colliding parent service registration.

## Resident product listeners

Opening an application may restore multiple installed package leases. The
process connection supervises liveness and the application scope cancels
preparation on retirement. Time spent verifying installed artifacts does not
expire the document handoff.

A Service Profile can select an application process with
`rsi.web.application`. Its configuration names a `source` (the same Web
asset selection as the browser), a private `profile` name, and a `process`
using the installed package process descriptor. One supervised process owns
the application's routes across local browser attachments. At activation the
host sends `application.open` with protocol 5, a private profile directory
and a random upstream key over the generation's private process connection.
The opening payload also supplies an opaque `owner` callback handle. Native
composition gives it the product's shared browser identity and the application
process's lifetime. Background plugin work uses this handle without borrowing
a browser credential or a completed HTTP request. It remains a device caller
through Session ingress and cannot perform local-only administration. Browser
closure or credential revocation retires that browser's requests; application
replacement retires the owner handle and all resources it retained.
The reply names its loopback port and owned route prefixes. Only the selected
assets' listeners forward those paths. After a published process disconnects,
the Host retires its sockets and request handles and prepares a fresh process
for subsequent browser requests. Each process authenticates a different key.
An initial or replacement activation failure is reported and stops recovery.

A Host profile can use `replace_preset_tools` with `tools = true` to assign
specific names to its selected application. Those native bodies are omitted
from the request even if the application withdraws a tool or cannot answer.
All other preset names retain precedence and delegated Sessions still obey
their tool allowlist.

An application configured with `tools = true` can advertise a separate private
loopback listener in its opening reply. Tool protocol 11 owns capture, input
preparation, start, outcome, cancellation and view release. Preparation borrows
the captured claims and retains the same view even without Tools. Local Turns
wait for this process's opening handshake when it owns input preparation. A
closed publisher wakes waiting Turns, and the Host continues serving unrelated
operations during startup. Capture and
preparation share the executor's deadline. It authenticates the process key,
has independent admission and bounded streamed replies. Browsers cannot
forward requests to this listener. HTTP 400 can carry a short JSON `error`
diagnostic for the caller. The native source resolves registrations
against the initiating local Session and a started model call admitted by the
ordinary execution policy. Application origin alone does not add an approval.
Outcome reads and cancellation carry the registration and exact identity from
the durable Tool intent. Start returns the settled outcome after the Worker has
committed it. A lost start response triggers only read-only outcome queries.
The source can read the same receipt from a replacement process. The effect
body is admitted once. An absent or mismatched receipt cannot prove a result.
Capture includes `modelSelection` with the native request's provider, model and
optional reasoning effort. This is the admitted request choice, independent of
the immutable Session Header and any later model-selection command.
Its reply includes required rendered `instructions` from the same captured
application context, with an empty string meaning no system contribution. Native
admission applies the Step instruction bound before retaining the request view.
Capture, preparation and start use the caller's cancellation/deadline. Receipt
queries and control requests retain their ten-second transport timeout.
The Host supplies a profile-specific receipt directory at application open.
An application result can include ordered text context messages with source
metadata. A confirmed receipt returns the same result and deferred input on
the running Host and during cold recovery.
The same directory is available to the standard Kernel before the Worker
starts. The application atomically records an outcome there before replying.
Cancellation allows five seconds for a receipt. A body that ignores its
signal keeps its Worker admission until it actually exits. The native caller
receives an unknown outcome when that grace expires.

An application observing a root Session can follow its pending human
interactions under the shared browser owner's authority. The observation
starts with the native complete snapshot. Its bounded fragments retain the
same snapshot through the last byte. Approval decisions and question answers
use the exact root, subject and request identity on the native Session handle.
Withdrawing a view releases its observation. It does not answer the user.

The history opening snapshot includes the native preset identity alongside its
immutable coordinates and captured draft or durable cursor. Application readers
may project that snapshot into read-only Session handles; this does not grant
write ownership, create an Agent, or make an unpublished draft durable.

`application.inbox` can observe any Session visible to the caller, including a
Kernel subagent preparing a model request. Interactive commands and queue
mutations on a subagent still go through its parent. A request-owned inbox
snapshot retains the exact active Turn, Fact and
control cursors and pending-message metadata. Opening returns the complete
metadata object, bounded by the native 64-message count and typed identities.
An empty snapshot retains nothing and needs no close operation. Message bodies
are read as bounded UTF-8 fragments; a message read must name an acceptance in that
snapshot. Reads remain valid if execution subsequently claims the message, but
do not assert it is still pending. Closing the snapshot or HTTP request releases
it. Consecutive fragments of one acceptance share its encoded body; reading the
last fragment or a different message releases that body. The snapshot never
caches all pending message bodies. For an unpublished draft it captures the Header revision and held command
inputs from the same native draft snapshot. The reply distinguishes draft input
from durable acceptance; a draft has no invented control cursor or queue slot.
Queue mutations and receipt lookups use the same native Session methods as other
clients. A withdrawal names the original slot and expected message identity;
claim or replacement races return their actual rejection, never cancel a Turn.
Only Human input is currently editable through this queue contract.
The read-only `message_status` operation returns the native message receipt or
null when no acceptance exists. It does not publish a draft or submit input.
Its acceptance cursor can address the immutable body through `application.history`.
Transport and access failures remain errors; absence is not proof that an earlier
in-flight submission cannot still commit.

Application history control pages include committed model selections and mailbox
acceptance, successor, claim, promotion and discard transitions, with their stored
Fact anchors. Acceptance metadata names its immutable control sequence; the
separate history message read fragments that exact acceptance through the Session
read contract. Pending state is never reconstructed from the mutable mailbox tail.
Model controls originating from a command include its exact invocation and
committed receipt. Baselines have neither: a draft baseline folds multiple
changes together and cannot identify an individual command by matching its value.

The optional `importer` selection names the package, manifest digest, export and executable
allowed to handle `application.import`. A callback supplies package
specifications and optional importer options. Serve fixes its workspace to the application profile and
passes process cancellation to the shared PackageStore import operation.
The callback includes the profile's expected roots. Its reply contains the
admitted package digests and the updated reservation for that profile.
The process can materialize an exact installed selection with
`application.payloads`. The reply contains its named payload and the named
payload of each direct dependency. Reads of the same digest share one closure
lease. After retiring every user of a selection, the application can send
`application.release` with its digest. Release is idempotent. The limit of
32 counts simultaneously held digests. Process retirement releases all
remaining leases.
An importing application reads its own profile's retained roots with
`application.roots`. Sending `expected` and `next` replaces that profile's
set under the Store writer lock. The Host selects the profile identity from
its configuration. A retiring process cannot overwrite a different root set.
The successful application handshake retains the exact application and importer
as separate bootstrap roots. Inspecting the importer manifest for retention
does not start or materialize its process or npm payload.
Every browser logged into this Host enters its application as the same owner.
This includes the operator's filesystem and package-install capabilities.
Each browser request and streaming attachment retains its own lifetime.
Session reads use `application.session` with the active HTTP request handle
and a Session ID. Native Session ingress resolves the durable Header under
the shared owner's authority. For an unpublished Session it reads the existing
draft handle. A local filesystem plugin can only receive a native local
workspace path through this operation.
`application.sessions` reads one page of visible durable Sessions with the
same live request authority. Each indexed activity summary supplies the last
activity time and durable Fact watermark used for the blank flag. The running
flag reports current Kernel ownership of a nonterminal Turn for that visible
identity. A cold open Turn stays in the list with a clear running flag.
Listing reads neither transcript bodies nor pending human interactions.
With `history: true`, the same cursor returns immutable history metadata and
the durable Fact/control cursor from one indexed summary per Session. The
parent names the history source for both independent and Agent forks. The
working directory is present for local execution only.
`application.create` resolves a named Workspace or registers an explicit
local directory through Workspace ingress for the live device. An unplaced
conversation uses the first visible registered local Workspace in insertion
order. A Home without one requires an explicit directory. Creation returns
the native draft identity, preset and creation-time list summary. The application
can announce that accepted draft through its own list event contract without
another catalog query. A requested blank draft can be reused;
a used root in the same Workspace opens a fresh identity. Only submission
publishes a draft to durable history.
Creation can also select a manual fork at an exact local terminal Turn.
Serve derives its Workspace and preset from the admitted source. The draft
starts an independent root and retains the native inherited cut.
`application.workspaces` pages the visible Workspace registry and reads
membership, archive and pin state through Navigation under the browser owner.
A pin intent on a published Session merges with current metadata under the
Navigation writer. Its receipt lists the owner's visible pins in pin order.
Archive admission reads the root and its activation-owned descendants at one
durable cut. It reports open, parked or waking work, including a pending
approval, and identifies running subagents by Session ID and task name.
The inspection returns activity families. The root's running and stopping
background Jobs contribute their IDs and labels from the current executor.
Cancelling that Turn reaps its Jobs through the original executor scope.
Independent forks and detached program branches have their own lifetimes.
On confirmation the archive is durable before cancellation. Serve stops the
active subagents and handles the root when its live Agent did not take the
stop. The parent's history records the stopped child's cancellation notice
without starting another Turn. Repeating the archive skips
the stop.
It uses the Workspace registry for creating, renaming and deleting
registrations. Creation returns the registry's atomic inserted flag.
The application can also list the browser owner's unpublished draft
locations. Both creation receipts and draft listings mark a draft nonblank when
its immutable selection retains completed history, including before first input.
Reconnecting does not require publishing the fork or reading its transcript.
An authenticated application request can page the committed
Language model catalog through `application.models`. Each page reports
the configured creation default, the deployment and model identifiers,
and reasoning efforts actually supported by each route. An unavailable
catalog or missing default reports its native failure.
`application.model.select` executes the Session's pinned model-selection command
with a caller-supplied request identity and the current command revision. The
existing command validates route and effort support and commits the choice.
The reply contains the selected value and the actual draft or durable receipt.
Recovery uses `application.command` with the original complete invocation; it
does not call the convenience selector again with a new revision.
The application does not change the creation default for unrelated Sessions.
`application.command` lists the addressed Session's pinned commands, executes a
complete caller-supplied invocation, or reads its request receipt. These operations
use the same live request authority and native Session command service as other
clients. Execution returns its actual draft or durable receipt; the application
cannot choose a publication sequence or replace the expected revision silently.
A workbench uses draft locations to place blank conversations before their
first message publishes them into the durable Navigation catalog.

The selected application opens one directory observation before paginating.
Its owner callback waits for coalesced changes until the application retires.
Workspace registration and Navigation metadata changes wake Workspace readers.
Session changes also wake the Session catalog. Providers without a directory
revision require periodic Workspace reads.
An explicit `application.submit` attaches the addressed Session with the
live browser request. It uses the caller's durable message ID,
validated ordered content and native mailbox delivery mode. Success returns
the existing or newly committed message receipt.
`application.terminal` uses the browser owner and the addressed
interactive Session. It exposes the Session terminal catalog and validated
native PTY operations. A create whose key retired before delivery returns
`{"type":"retired"}` so clients can discard the missing terminal. Other
failures keep their own diagnostics. Controller takeover, resize and input receipts remain
owned by the native PTY.
`application.cancel` passes the caller's `keep_inbox` choice to one native
Session-wide cancellation operation, or uses inbox-only cancellation when
`inbox_only` is true, and returns its receipt. It does not first
inspect an active Turn or enumerate message-removal requests. The standard
Session adapter handles unpublished drafts without publication. Interactive
root commands reject an Agent-owned child.
For an explicitly opened Session, `application.history` reads immutable Facts
under the active browser owner's Session handle. Opening returns the durable cursor
and the current native model-selection projection. An open with `metadata_only:
true` returns the stored header metadata and indexed cursor. Opening an absent
or inaccessible Session returns null. A metadata-only read also returns null
for an unpublished draft. Other read failures remain errors. The captured Fact watermark
selects the history cut, while the control watermark identifies its independent
selection revision. An absent producer uses the immutable Header baseline.
A failed producer is reported rather than replaced with a default.
Control history uses the same authorized handle and a captured control horizon.
Pages expose model-selection commits with their Store-owned Fact anchors;
the native model-selection codec resolves complete replacements. A wait returns
a coherent pair of current watermarks after either stream advances.
Fact and decoded model-attempt reads are UTF-8 fragments. Waiting subscribes to
native observations after the supplied control and Fact cursors. A retired
upstream request loses access. Session listing uses its independent cold path.
Draft and published catalog rows include the native creation timestamp, so an
application can associate its presentation metadata with the same Session.
`application.descendants` reads an authorized Session's atomic Store subtree
without activating Agents. It returns reachable, individually visible descendants
in Agent path order, with direct parents and depth relative to the requested
Session. An open Turn with no known live owner has unknown activity.
Model observers belong to their physical application request and one authorized
Session and attempt. They read bounded pages of the native coalesced prefix,
independently of durable Fact cursors. Closing the reader, losing the device,
or retiring the upstream releases its observer.
If the serving extension disconnects, Host diagnostics report its terminal
operation and error.
An inherited-history reader is bound to its authorized target Session and
physical request. The application can request Facts and model-attempt data
only by interval ordinal and cursor inside the captured selection. Opening a
subagent address also verifies its direct parent before returning any data.
With `RSI_PACKAGE_TIMINGS=1`, the Host also requests process startup timings
in the application opening reply. Applications may supply elapsed setup,
composition and reconciliation milliseconds for the Host diagnostic log.
The opaque continuation belongs to the requesting principal. Retirement also
invalidates a page which was still being read.
Large package closures prepare in the background as soon as the application
profile starts. Optional `routes` announce paths whose first authenticated
request should wait for readiness. Other Service operations and the document
bootstrap remain available while the application verifies its packages.

The product launcher connects to the already selected local Host. Its ordinary
`rsi.service.web` plugin owns browser listeners and per-connection Serve/Desktop
listeners. Control frames are bounded, native-peer authenticated and fenced by
Host epoch plus the `rsi.web.control` descriptor. Applications do not create an
additional Store or rotate credentials when attaching.
Invalid Web/Serve arguments are rejected before activation and retain a bounded
application diagnostic, so the CLI can report the specific rejected option.
Failures before listener/frontend readiness use bootstrap exit code 2. After
readiness, failures including an unexpected Host listener or watched frontend
exit use runtime exit code 1. The foreground application's own shutdown request
or signal succeeds; an independently requested Host stop is listener loss for
that application and exits 1. This follows the application
[failure classification](../../crates/rsi/application/README.md).

`rsi web` starts or reuses one loopback listener, prints a fresh one-use ticket,
opens the browser in the caller's desktop environment and returns. Omitting
`--port` reuses the actual bound port; changing an explicit port or source requires
`rsi web stop` first. Stop retires only that browser listener. Serve and Desktop
instead retain a control connection; disconnect retires their own listener.
Serve is API-only by default. `rsi serve --assets ABSOLUTE_DIRECTORY` additionally
serves an internally verified Web bundle from that same authenticated listener.
Its explicit HTTP Origin and TLS policy remain unchanged.

An Application Profile may configure `rsi.application.web` with
`assets = { kind = "package", package = "NAME", export = "EXPORT" }` (or an
explicit absolute directory). The resident listener acquires and retains the
selected package closure. Package-backed listener creation has a five-minute
readiness deadline for complete archive and expanded-tree verification; other
Web control commands retain the thirty-second deadline. No hash, file-type or
path check is skipped when starting a listener. An invocation's `--assets` and configured assets are
mutually exclusive. Port and browser-opening options remain invocation-owned.

Ordinary Desktop retains its loopback port in `state/desktop-port`, so the exact
Origin and WebView drafts survive both window and Host restarts. The first
listener selects a free port and durably publishes it before issuing a ticket.
This private record is exactly one nonzero big-endian u16; no-follow reads reject
malformed or nonregular entries. Publication syncs a private staging file before
linking it without replacement and syncing the state directory. A later launch
reclaims only the known regular `desktop-port.pending` staging entry. File work
runs on blocking workers under a Desktop creation coordinator; the selected Host
already holds exclusive state authority. A busy retained port fails explicitly,
without silently changing Origin. Thus one ordinary Desktop listener can be
active per state home. Connection loss still retires the listener, and `web stop`
does not affect it. Explicit source frontends use their configured Origin.

Asset presence never selects browser authentication: Serve retains device
authentication and its requested binding; only local Web/Desktop listeners
issue browser launch tickets.
There are at most 32 control connections, of which at most 16 can retain
connection-owned listeners. Long-lived attachments leave 16 slots for commands.
Excess connections or attachments receive a bounded capacity error. There are
at most 64 pending ten-minute tickets per listener. Repeated tickets share one Host-owned managed device credential.
Revocation prevents new tickets and successful redemption until an explicit Host
restart reauthorizes the managed device.

The default Web source is the installed `web` package's `web` export. An explicit
absolute `--assets` directory is local operator authority. Both sources verify
the internal bootstrap receipt; installed packages additionally retain their
immutable package closure and check required interfaces. Core executable identity
is not a Web bundle family.
The embedded local Web adapter applies the same internal bundle verification.
Its merged operation catalog uses the same Application-local precedence as
dispatch. A later Service generation publishing the same operation ID cannot
replace that local operation or erase unrelated discovery entries.

A watch invocation with an explicit source frontend recipe creates an independent
connection-owned listener. Its exact frontend Origin is validated before HTTP
activation; forwarded headers never select that authority. The caller's ordinary
Process/Sandbox children supervise Vite, including bounded control frames,
readiness deadlines, exit and descendant cleanup. The verified bundle supplies
Worker/WASM assets; frontend source changes use Vite's document reload.
This listener does not replace the persistent browser listener.

After a source package is admitted, `Refresh` re-resolves that package for all
live listeners using its export. Each listener stages a verified candidate and
publishes only renderer changes, retaining the new package lease. A changed
bootstrap reports restart-required and leaves the old generation serving.
The watching application then drains and re-resolves its own composition.

The following embedded SDK contracts apply only to explicitly composed
in-process services; product commands use the resident listener contract above.

Retained service observation and reload handles do not own the native service
lifetime. After service cleanup, observing its stopped result remains valid and
reload fails; neither operation pins the stopped generation's owner lock.

The native Serve application owns HTTP configuration, process signals and one
ApplicationRun entry. It consumes the API dispatcher, device authentication and
connection-description capabilities, plus a ServingService capability for the
chosen service generation's failure notification and Profile reload. It creates
no backend or Runtime: an ordinary
HttpFactory child owns the listener and its requests within the invoking Profile.
The standard product composes its independently owned service generation beside
this application.
Native signal ownership supports Unix and Windows. Windows handles Ctrl+C,
Ctrl+Break, console close, logoff and shutdown; the OS can bound the grace period
for the last three. Profile reload remains available through its API; SIGHUP is
Unix-only. Unsupported platforms reject before creating the HTTP child or listener.
Complete static HTTP/argument validation precedes platform admission. Malformed
configuration returns `MetaError::InvalidInput`; valid configuration requiring
unavailable signals returns `MetaError::Activation`. The retained application
diagnostic uses the existing `RsiError::Boot` class for Host bootstrap failures.
Activation failures also use `MetaError::Activation` and retain that diagnostic.

The factory validates arguments and HTTP policy during preparation, before any
backend activates. Configuration may be an HttpConfig object with no arguments,
or null with `--bind ADDRESS --origin ORIGIN` and either `--tls-certificate FILE
--tls-key FILE` or explicit `--dev-http`. Production requires TLS and the existing
HTTP/2 browser contract. Development HTTP requires loopback binding and origin.
Certificate contents are read by the HTTP owner at activation, never as arguments.

Calling ApplicationRun schedules its one owned entry before returning a waiter.
Dropping the waiter does not stop the listener. SIGINT or SIGTERM finishes the
entry, allowing the enclosing Profile to drain requests and shut down its owned
service. Listener failure returns a failed exit. Withdrawal fences entry, cancels
the signal waiter and drains owned work; it never signals another service process.
On Unix, SIGHUP starts at most one reload waiter. Stop remains selectable during
reload and drops that waiter before composition shutdown; a closed SIGHUP source
is disabled. The application republishes the read-only HttpListener observation
so another application plugin can discover the actual bound address.

ServeFactory::with_web_assets selects StaticHttpFactory and explicitly requires
the separately composed HttpAssets capability. Embedded SDK callers compose it directly
with `rsi.web.assets` owning the bundle. The product catalog uses the resident
`rsi.application.serve` entry instead.
Its argument and lifecycle contracts match Serve; it does not resolve asset
paths or read files itself. API-only Serve requires no Web build artifacts.

Serve Web additionally owns the authenticated renderer-generation API. Its scoped
HTTP dispatch combines the selected Service's live dispatch with an independent
Application registration owner for asset leases. Only `connection.describe` and
`connection.operations` are deliberately replaced to negotiate the combined
catalog; other duplicate operation identities reject activation. The endpoint
identity and Host epoch remain those of the selected Service. The listener and
lease registrations retire together; renderer publication preserves both. This
composition neither exposes the Service's registrar to the document nor changes
its domain ownership.

## Local Web launch

After frontend readiness, bounded stdout chunks are drained until EOF or child
exit. Unexpected output is reported once without echoing its contents; a log
line is not process termination, including bytes received in the same chunk as
the readiness frame. Before readiness, stdout carries only protocol frames.
Package closure verification runs on blocking
workers, leaving async runtime workers available for listener control.

The ordinary local Web factories own `--port` (default 8787, range 0..65535),
`--no-open`, `--assets ABSOLUTE_DIRECTORY`, and Web-only help. The launcher
supplies frozen executable-relative assets and SSH/browser-opening inputs.
Assets activate before the Service. Missing bundles identify the selected path
and build command. Opening never builds assets or replaces an existing Host.
The ordinary loopback listener derives its exact Origin from its own socket;
owned source frontends use the resident plugin described above.
Local listener failure reports the requested port and suggests `rsi web --port 0`
to select a free port, including Windows reserved-port failures.

Explicit startup rotates a product-managed browser device credential while
retaining its DeviceId, then grants configuration access through the product's
existing Local authority. Revocation during that run is never reversed. The
application retains one random 256-bit launch ticket for ten minutes, redeemable
once at that listener. It closes the ticket owner on retirement. Readiness
prints a fragment-bearing link; the secret is never placed in a query string.
Browser opening uses argument arrays, is bounded and best effort, and is skipped
under SSH. A used, expired or revoked link without a valid cookie requires restarting
`rsi web`; normal reloads and additional tabs recover through the cookie.
Redemption consumes the ticket before authenticating its device. A concurrent
revocation, deletion or credential rotation makes that token unusable; failure
does not undo revocation, issue a replacement token or restore the one-use ticket.

The automatic OS opener receives the launch URL as a process argument. Local
users able to inspect those arguments may see the unused ticket, including while
it remains in a browser's initial command line. Fragment transport prevents HTTP
URL logging, not local process inspection. `--no-open` avoids this automatic
handoff and leaves the explicit printed-link channel. The current opener does
not provide a private cross-platform credential-transfer channel.

Resident browser creation has a separate coordinator from published status.
Package hashing and listener activation never hold the status mutex. Stop fences
any in-flight creation before awaiting its cleanup; a cancelled creation cannot
publish readiness. Status returns Stopped for a terminated listener.

Embedded and resident launchers share one ticket implementation for entropy,
expiry, redemption and retirement. Embedded SDK composition remains a supported
public surface, exercised independently from the product resident launcher.
