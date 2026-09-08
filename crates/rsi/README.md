# rsi

## Terminal application

`rsi --profile tui` selects the fullscreen text application. It accepts `--cwd`,
`--resume`, `--session-id`, `--agent-preset`, and `--trust-workspace`. Both stdin
and stdout must be terminals; rejection happens before Host bootstrap. The
input backend currently requires Unix; native Windows input is unsupported.
The terminal application plugin owns input, rendering, signals, and cleanup. One active Session
has one durable observer and one live-interaction observer. Child inspection
uses finite history reads rather than additional observers. Switching Sessions
preserves drafts and ignores asynchronous results from prior attachments.
The client retains at most 64 saved Session drafts and 1,024 owned pending
message identities. Read work leaves reserved capacity for submission and
cancellation; at most 12 client requests are outstanding.

Enter submits NextTurn, Ctrl+J inserts a newline, Ctrl+O sends Steer, Ctrl+P opens
the action menu, and Ctrl+Y copies the selection. Mouse release also copies a
selection. Ctrl+C always cancels the attached active Turn and this client's
accepted pending input, preserving the editor draft. Esc closes the focused
layer or selection. Ctrl+D with an empty editor exits. Bracketed paste is
enabled; enhanced keyboard events are negotiated when supported. Alt+Enter is
left to the terminal. A bounded input framer rejects an entire oversized paste
and discards through its terminator before accepting keys again. Drafts are
limited to 1 MiB UTF-8 each and 4 MiB in aggregate; rejected insertion preserves
the previous draft. Real SIGINT invokes cancellation; SIGTERM and SIGHUP exit.

Model selection is client-local and affects future explicit NextTurn requests.
An admitted request freezes its identity, model, and content through retries.
The Header remains the Session default. Steer carries no model override and may
be promoted to a new Turn using that default; the UI reports durable routing
rather than claiming a snapshot prevents the race. Questions and tree approvals
come from live snapshots and keep their exact request and owner identities.
PageUp/PageDown scroll long questions and details. The focused-card action
provides exact Fact source windows and separate stdout/stderr output pages;
left/right moves between pages. Opening another detail resets its actions.

All dynamic text uses the shared terminal sanitization rule before rendering
or copying: controls other than newline/Tab and Unicode bidi controls become
U+FFFD. Layout expands tabs and uses narrow ambiguous-character width throughout.
No model or tool text is interpreted as terminal escape sequences. A single
writer owns terminal output, including OSC52; diagnostics do not write over the
alternate screen. Cleanup restores terminal modes before ordinary diagnostics.
Panic cleanup is best effort and does not cover SIGKILL.

Initial history uses 128-Fact pages at a captured watermark, stopping at the
latest Turn start or after eight pages / 1,024 Facts / 16 MiB returned encoded
bytes. The byte threshold stops further prefetch, not an already returned page.
The live display retains at most 512 blocks, 4 MiB of text, and 8 MiB of projection
metadata including retained container capacity. A block has a 256 KiB text
window; omitted text has an explicit source range and can be reloaded.
Manual backward browsing owns a second projection with the same bounds, evicting
newer blocks to admit older pages. Live observation continues into the live
projection. End restores that live view and fences outstanding history reads.
Layout keeps only the viewport and one screen of overscan. A valid 36 MiB Fact and a
64 MiB transport page remain possible transient allocations; these UI budgets
are not RSS limits. Observation handles are released after projection.

Selection uses stable source positions across reflow, streaming, and historical
prepending. Copy preserves sanitized source text and hard line breaks, excluding
soft wrapping and UI decoration. Selection copy is limited to 4 MiB; it never
silently succeeds with a prefix. Native clipboard helpers use bounded process
operations; OSC52 sequences have a separate 32 KiB encoded limit and delivery is
reported as confirmed, unverified, or failed. Completed process output is read
only by its issued cache identity; it is not a general Fact archive.

Fullscreen exit follows the same remote-detach / embedded-shutdown lifecycle
as the line application, and displays the actual consequence. TUI behavior tests
use a pure controller and TestBackend, with Linux PTYs for input and terminal
restoration. Native terminal/clipboard and live-provider evidence is opt-in.

## Line application

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
and human prompts use stderr. JSONL version 4 emits only structured envelopes
on stdout, including live interaction snapshots. These snapshots report live
Host state; their absence in history never authorizes replay of a human wait.
Turn cancellation does not discard terminal Fact or Outcome envelopes queued for
the renderer. Detaching or stopping the renderer still ends presentation.

The standard Linux coding catalog includes `output_read`, an independent
read-only contribution over the Process completed-output cache. It accepts only
an issued output identity, raw offset, and bounded page size. Complete logs live
under the Host cache identity and may survive normal exit/restart until quota
eviction; they are not a durable Session archive. Model text includes safe UTF-8
decoding and raw cursors. A page that splits a UTF-8 character may display the
replacement character U+FFFD; the next cursor always advances by raw bytes.
Tool results and `:output` include `bytes_hex` for exact reconstruction. When
decoding or display sanitization changes a page, the Tool's model-facing text
also carries those hex bytes. Display text replaces control characters and
Unicode bidi controls, while preserving newline, tab, and ordinary joiners.

The `rsi` product owns standard application composition and the Service Host.
Its library owns the explicit linked factory catalog, standard composition,
Application and Host Profile catalogs, the transport-independent
Session domain, and process-local and Unix-domain-socket adapters for independent
Workspace, Models, Media and completed Process output capabilities. Its
terminal application package owns its argument grammar, interaction and signals;
the binary owns launcher/management parsing, process control,
and construction of the Tokio runtime.

The standard catalog links OpenAI, OpenAI-compatible, and DeepSeek factories
without implicitly enabling a deployment. A persistent Profile instantiates
the chosen provider and Settings names an exact default deployment/model.
The standard Host explicitly configures four maximum active Agent turns. The
Kernel still serializes turns within one Session; the four lanes permit bounded
progress across independent Sessions. A copied Host Profile may replace the
complete executor configuration to select another value from `1..=256`.

`rsi --profile NAME [application arguments]` selects one Application Profile.
The built-in, non-shadowable `cli`, `headless`, `tui`, and `serve` profiles select the
built-in `standard` Host Profile. The `devices` profile connects only to the
already running local owner and does not start a backend. There is no implicit
Application Profile.
Application Profiles are ordinary ordered Profile programs below
`application-profiles/<id>/application.profile.toml`. They declare the connection
and application plugins, with the same groups, isolation, Rhai expressions and
relative sources as any other Profile. Every initial leaf prepares before any
backend activates. The old application enum and `application.toml` documents are
unsupported; recreate them explicitly. The retired `session` application name
is rejected with guidance to select `cli`; old files are never overwritten.

On Linux, `serve` composes a publishing Service Host and the independent
[HTTP application](serve/README.md) in one Runtime. Configure the standard Host's
explicit `rsi.agent.default_model` setting and provider Profile before serving
coding Sessions. For an isolated development listener use
`rsi --profile serve --bind 127.0.0.1:8787 --origin http://127.0.0.1:8787 --dev-http`.
Production instead supplies an HTTPS origin, `--tls-certificate FILE` and
`--tls-key FILE`. HTTP always requires device authentication; the same process
also publishes the existing same-user local endpoint. `host status`, `reload`
and `stop` address that owner normally. `host serve` remains the foreground
local-daemon management entry. Compose the independent [Web assets plugin](web-assets/README.md)
with the HTTP application to serve the [Web application](web/README.md).
The [Web build and launch reference](../../plugins/rsi/web/README.md) provides
the application Profile and bundle command. Its Rust Worker owns shared client
controllers and two independent panes; the document renders views and forwards input.

`rsi --profile devices register LABEL` returns one JSON receipt with EndpointId,
device id, label and its one-time token. `list` returns non-secret records;
`revoke DEVICE_ID` revokes that exact device. These commands use the live owner's
same-user local API and never edit its credential database independently. Keep
the registration receipt for browser login or explicit remote client setup.
An unknown registration outcome requires list/revoke reconciliation; the CLI
does not silently issue another credential. Ordinary remote credentials cannot
invoke device administration.

An explicit native remote Application Profile composes `rsi.credentials.local`
and `rsi.application.http`, followed by its CLI, Headless or TUI plugin. The HTTP
connection configuration is the [native HTTP client contract](../rsi-api/http-client/README.md):
explicit origin, expected EndpointId, credential reference and optional CA file.
It negotiates wire and domain operations without the local executable hash gate.
Credentials configuration holds references and environment names only; the
launcher captures `RSI_API_DEVICE_TOKEN` alongside the standard provider variables.
The connection plugin prepares transport policy before activation, composes HTTP
and independent domain clients in a child Profile of the same Runtime, and
publishes their capabilities with remote-detach lifetime. It never creates local
backend state or signals the remote service on exit.

Host Profiles are bounded TOML
documents below `host-profiles/<id>/host.profile.toml`. Profile management can
list, inspect, copy, delete, and purely preview these documents without
activating plugins, resolving credentials, acquiring the Store lease, or
publishing a Host endpoint. Host preview reads the authoritative Agent-preset
Settings used by daemon launch, but it does not materialize the built-in preset
asset or activate the selected Host Profile.
Catalog listing includes only regular profile documents; a symbolic-link
document is neither opened nor advertised as an available profile.

The exact management surfaces are `rsi profile application
<list|show|path|copy|delete>` and `rsi profile host
<list|show|path|copy|delete|preview>`. `rsi host start` is the only operation
that detaches a new daemon; `serve` runs it in the foreground, `status` probes
the recorded generation, `reload` requests a full Profile rebuild, `stop`
drains it, and `restart` composes stop and start. `stop --force` and `restart
--force` open a pidfd and validate the recorded process start token before
sending `SIGKILL` to that exact process descriptor. If the runtime's SIGHUP
source closes, the daemon disables only the reload branch after one diagnostic;
it does not spin on an always-ready closed stream. SIGTERM/SIGINT closes reload
admission and aborts any in-flight SIGHUP waiter before daemon shutdown, so a
stalled reload cannot retain the Profile lifecycle lock ahead of stop.
Daemon task failure still drains reload, diagnostics and preset ownership before
returning the task error.
The `host start` launcher reserves the owner lease before spawning and passes
it to the child without releasing ownership. The child creates a new Unix
session before Host bootstrap, so
terminal process-group signals and hangup ownership do not remain shared with
the launcher. Foreground `host serve` deliberately keeps its caller's session.

One owner process holds the standard Host paths at a time. A foreground daemon
publishes a same-user Unix-domain socket; an application uses it when its exact
protocol, product build, Host launch key, and Host epoch handshake is
compatible. Durable metadata remains structurally readable across executable
rebuilds so lifecycle commands can identify and signal an older exact process
generation; compatibility is enforced during application selection and the
handshake. The active daemon's validated metadata endpoint is authoritative,
including when the client's runtime-directory environment differs or cannot
itself hold a Unix socket. With
no owner, an application may acquire the same owner lease and
run a private embedded Host without publishing an endpoint. A starting,
embedded, or temporarily unresponsive owner is waited for up to the same
15-second readiness bound and is never bypassed by a second Host. The standard
product daemon is Linux-only because its lifecycle
signals are fenced by a pidfd plus Linux process start identity; other
platforms support embedded mode only.

The Session service creates drafts from registered Workspace identities, attaches,
and lists sessions, then exposes one
handle for ordered text-and-image mailbox submission, direct Image generation,
cancellation, reconnectable observation, bounded backward history, and live
approvals. Callers allocate a `MessageId` for Language or multimodal input.
Acceptance atomically persists the immutable Header when needed and a canonical
mailbox record, but it does not invent a `TurnId`; the later durable claim
creates the Turn and first Step. Retrying the same identity and body returns the
indexed message state across reconnect or restart, while a changed body is a
typed conflict. Agent-control records and Facts are independent durable streams.
Approval waiters remain bounded live Host state and are never replayed as
effects.

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

The Rust Session interface additionally exposes direct Image generation. Its
caller allocates the `TurnId`; the operation validates its exact Image route and
does not require the session's default Language deployment to be available.
Image results remain Media references.

The product materializes its built-in `standard` Agent preset as a verified,
digest-addressed cache asset and prepends it before configured and writable
user roots. Unix materialization creates, verifies, and publishes through
no-follow directory descriptors. It accepts an operating-system alias only in
the first component below `/`, then rejects symbolic links throughout the
owned suffix. The portable fallback rejects observed link
or reparse-point components before publishing. Each fresh session retains a
process-local draft carrying the current preset generation until its first
submission is durably accepted; a failed pre-durability attempt can therefore
retry through the same handle without resolving a different generation.
Durable resume uses the Header's required
`agent_preset_id` and cannot override it. The Kernel retains that exact pin for
the resident session, while the executor reads definitions and executes every
Tool through the claim's immutable catalog. The runner prepares that exact
fresh or resume generation before any durable Workspace registration, and a
generation-preparation failure therefore cannot create a Workspace row.
Dropping an unsubmitted resume token has no Store or resident-capacity side
effect.

The standard Agent preset also selects workspace and time context as ordinary
contributions. Workspace refresh and its last-good domain state commit together;
time context commits one UTC clock reading before each new provider retry series.
Both belong to the immutable Agent generation. Custom presets select their own
contributions through the Agent-only addon catalog.

The standard preset selects [plan policy](../rsi-agent/plan-policy/README.md)
through that same catalog. Planning starts disabled and can change through the
shared Session command service before or after publication. Its Tool allowlist
adds a constraint to existing approval and sandbox policy.

On Linux, linking the standard coding Tools makes a successfully probed
restricted sandbox backend a Host activation requirement. The Host does not
begin serving and defer an unavailable enforcement backend until the first
Tool call. On Linux, the binary resolves its own canonical executable and `/bin/bash`,
freezes the scrubbed child environment before Host construction, and passes
those values explicitly into the standard composition. The Bash Job producer
is global because Jobs identities outlive Agent generations. The model-facing
`bash`, three Jobs controls, and `apply_patch` are separate Agent-only
contributions activated inside an unpublished Tool catalog and atomically
sealed with one preset generation. Other platforms omit the Linux-only Bash
and apply-patch contributions before Runtime mutation rather than advertising
effects whose native lifecycle guarantees were not tested.
The Session Jobs finalizer cancels and reports all unfinished turn work before
the terminal Fact; unreported background completion blocks a successful turn.
These closure claims require the host process to remain alive through
finalization. Restricted standard plans also bind Bubblewrap to parent death;
`danger-full-access` has only process-group ownership and cannot honestly claim
cleanup after host `SIGKILL` or containment of a descendant that calls
`setsid(2)`. Web, TCP, cloud identity, marketplaces, arbitrary executable
profile bundles, Media export, and native package management are outside this
Host contract.
