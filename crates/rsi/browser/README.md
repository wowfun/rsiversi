# rsi-browser

Browser owns one generation-bound runtime lease, its frozen destination policy,
private CDP and MCP connections, deterministic checker and user evidence. Process
owns native launch and reaping; Sandbox supplies the exact restricted plan.
Neither Jobs nor the global MCP manifest participates.
The shared runtime pool pins its configuration and native owner before preparing
confinement, then releases the pool lock. Matching consumers share that owner's
readiness and capacity; mismatched configurations fail without waiting for the
probe. Cancellation or failed readiness never replaces the pinned owner within
that Service generation.

Restricted runtime requires verified Bubblewrap, Chromium sandbox and cgroup v2.
PID/network/mount namespaces expose only fixed read-only runtime resources and
private scratch. There is no host network route. An owned bounded broker is the
sole HTTP/CONNECT exit, validates complete DNS answers and pins actual addresses.
Automation's preview policy denies loopback. Chromium CDP uses a pipe; no CDP TCP listener is
present in the page namespace. Separate restricted client scopes attach through
private authenticated bridges. Browser and client epochs never cross attempts.
The broker retains at most eight socket workers, including DNS, connection and
closing stages. DNS plus TCP admission has one five-second deadline; writes have
a per-socket five-second deadline. The router dispatches network work without
awaiting it, so one stalled destination cannot stop CDP routing. Each direction
allows two unacknowledged 32 KiB frames; acknowledgements match exact FIFO byte
counts and CONNECT head bytes consume the same credits. Socket identities
strictly increase and are never reused in a scope;
late completion and acknowledgement packets cannot name a successor socket.
Scope traffic is capped at 32 MiB. Close interrupts pending work and does not
recycle a socket slot before the worker settles.
Proxy headers are accumulated and searched incrementally, with a 16 KiB header
and at most 32 KiB of initial payload checked before copying a received chunk.
The local WS upgrade's complete initial frame is capped at 32 KiB.
Both HTTP parsing paths reject invalid field names, bare CR/LF and control bytes
before any egress admission; binary payload bytes remain unchanged.
A socket worker closes its input lane before publishing the close receipt;
buffered outbound data still precedes that receipt. Input racing a closed worker
is discarded; a full live-worker input queue still fails the credit contract.
On a broker close, the helper finishes admitted local writes through callback
and drain before ending the socket. One five-second close deadline bounds this
flush and graceful shutdown; the socket retains its slot until native close.
Scope termination and socket errors destroy the socket immediately.
Helper stdout retains at most 16 MiB of encoded packets, with an 8 MiB proxy
share. Charges survive through write callback and drain. Proxy reads pause under
stdout backpressure; overflow fails the scope closed.

Fixed Node, Chromium and dependency paths/digests are operator-supplied. Checker
pins Playwright 1.63.0; exploration pins @playwright/mcp 0.0.80. No download or
unconfined fallback occurs at activation. Each isolated process scope defaults to 1 GiB memory, 256 processes and a
ten-minute runtime maximum; Browser and its client occupy separate scopes. A missing enforcement mechanism
is NotReady, not an invitation to weaken the plan.

Preparation hashes the installed runtime on a blocking worker, then proves one
launch/retirement before publishing readiness. Operators keep those installed
bytes unchanged for this runtime generation; replacing them requires a new
generation and preparation. Opens consume the verified generation without
rehashing it. Preparation is serialized and idempotent for a verified generation,
so occupied slots cannot turn readiness into a verification failure. A failed
process settlement fences the runtime until replacement. Subsequent operations
report the retained failure diagnostic, or the settlement failure fallback if
none is available, rather than a temporary capacity refusal.

Checker predicates include an entry identity guard, final URL, visible text and
role/name visibility. Missing entry identity and protected pages are unavailable
targets. Only complete assertion results settle a verdict. Navigation is bounded
to 20 seconds, checker to two minutes; dialogs are dismissed and recorded.
Exploration exposes only fixed navigate and text observation wrappers. Raw MCP
catalogs are never exposed. Screenshots are viewport-only user evidence, not
Agent Media: at most four 512 KiB canonical PNGs. No screenshot base64 enters
durable Agent facts.
Checker operations share a 110-second budget, including evidence capture,
inside the Rust two-minute exchange deadline. Navigation and subsequent text
share one 20-second budget inside their 25-second exchange deadline.
Exploration validates the current top-level URL from private structured CDP
metadata after every operation; page text never supplies a policy coordinate.
Multiple page targets fail closed. Structured control replies and private MCP
replies have separate bounded receive lanes; the idle MCP reader cannot consume
or block a policy response. A blocked checker destination retains its URL
and blocked disposition but withholds snapshot, assertion details and images.

Launch accepts a caller cancellation token. Once admitted, an independent startup
job retains its slot and every returned process through verification and cleanup.
Cancellation prevents undispatched stages; an admitted Process spawn is awaited
before cleanup. Abandoning startup starts cancellation. Panic fences the generation
and dispatches cleanup through the captured runtime handle.
Cleanup follows Meta's [embedding execution lifetime](../../rsi-meta/core/README.md#runtime).
Retirement stops admission/egress, drains clients, terminates the restricted
scope, awaits Process settlement, then removes owned scratch. Capacity remains
retained until receipts settle; a failed receipt fences the generation before
capacity is released. All admitted process receipts are observed concurrently;
cleanup reports each failure. Ordinary spawn failure or cancellation with clean
settlement leaves the generation available. Abrupt Host death has a bounded
final retirement guarantee only after native proof.
All bounded response queues interrupt delivery when their owner stops, including
error delivery, so an unread saturated queue cannot prevent retirement.
Each helper packet lane holds at most 16 parsed frames. A wire frame is at most
8 MiB, giving a 128 MiB queued wire-content ceiling per lane, in addition to
in-flight frames and JSON allocation overhead. Private MCP packets require an
explicit `value` field before routing to the MCP reader.
Any command timeout or abandoned dispatched command retires its scope before another command
can enter. Late responses cannot satisfy a later request. Explicit `close()` awaits
settlement; dropping the last public owner starts retirement, whose independent
guardian retains capacity until both processes settle.
Runtime hashing covers npm command links under `node_modules/.bin` too.
32,768 total entries, including directories and roots, and 1 GiB of encoded
relative paths, link literals and actual file bytes may be inspected. Directory
discovery charges before retaining names, depth is at most 128, and iterative
traversal shares one 64 KiB read buffer. A file length change rejects preparation.
Only relative links resolving to regular files within the hashed tree are allowed.
The checker uses the stable Playwright dependency; upstream MCP independently
requires its pinned 1.63.0-alpha-2026-08-31 Playwright core. Both trees are covered
by the artifact digest; merging them would change the upstream MCP contract.

Default suites are deterministic and keyless. Linux product CI additionally
executes the helper's HTTP parsing, proxy-flow and structured observation Node
tests in its fixture readiness gate, independently of native builds. It also
installs the pinned Node/Playwright runtime, prepares a systemd user manager and
explicitly runs the native checker/retirement and abrupt-owner-death tests.
The [CI user-manager fixture](../../../fixtures/rsi/browser-runtime/README.md)
records existing runner state and restores only job-created resources on exit.
These tests remain opt-in locally and require the three `RSI_TEST_BROWSER_*`
runtime paths. Real provider tests stay separate and require explicit credentials.
The abrupt-owner proof observes renderer filter initialization within one shared
two-second deadline: a visible renderer command line alone is not readiness.
Every observed renderer must reach `Seccomp=2` and `NoNewPrivs=1`; namespace and
no-sandbox-flag checks remain mandatory. A persistent missing filter fails with
the observed status rather than being skipped.

Initialization waits for the browser helper's ready acknowledgement before
starting the CDP client. Client CDP traffic cannot precede Chromium's pipe
installation. The helper assembles NUL-delimited CDP frames in a reusable 8 MiB
buffer, copying incoming fragments once and bounding each complete frame.
The Rust bridge relays CDP JSON as an opaque string inside its bounded envelope;
only the JavaScript endpoints decode its UTF-16 string values. This preserves
private DOM identities, including lone surrogates, without admitting them as Rust
strings. Public helper packet strings use Unicode scalar values; encoded envelope
bounds include escaping overhead.

## Interactive Session browser

The opt-in Session browser uses a typed Playwright session helper over the same
restricted runtime. A Service owner retains one single-page, temporary-profile
instance per immutable Local native Session binding. Automation and Session
browsers share two slots, including opening and retiring scopes. Either the
Session-entry limit or the shared runtime limit returns `not_started/capacity`
before launch; this known refusal is retryable. A fenced runtime instead retains
its failure diagnostic. Actual process settlement releases capacity. Turn completion,
Session reattachment and presentation detach do not release the browser.
Explicit close, authority withdrawal, five minutes idle, ten minutes from open
admission or Service retirement stop admission and retire it. Status polling,
reopening a panel and rejected work do not extend idle or the hard deadline.

Each browser admits one active and one pending operation; pending waits at most
five seconds within the operation deadline. Additional pressure returns
NotStartedBusy with retry_after_ms=1000. Close and revocation bypass waiting work.
Open cancels launch at 60 seconds and retains ownership through its launch receipt.
Initial navigation starts a fresh 25-second owner budget after launch; the two
budgets are independent. Navigation helper/owner deadlines are 20/25 seconds;
click, fill and scroll 5/10; observation and screenshots 10/15. No parameters can
relax deadlines. Known refusals do not retire; cancellation or uncertainty after
dispatch retires without replay. Broker DNS/connect retains its five-second limit.
Screenshot publication stops waiting at the operation deadline; any dispatched
Media worker retains its own publication ownership and the uncertain charge.
One independent native retirement owner settles both processes and bridge tasks.
It retains native capacity until both receipts settle. Close waiters share its
receipt. If that owner cannot establish settlement within
30 seconds, the shared runtime is fenced; no replacement browser can reuse
unproven capacity. A failed retirement removes its Session binding after the
failure receipt, while the Process provider retains native cleanup responsibility.

BrowserId identifies one open instance; document_version changes on main document
replacement or top-level URL change; starting an observation invalidates the old
observation_id and node handles. Failed observation retires the scope. Only the
latest observation's at most 256 ElementHandles is retained.
The complete structured snapshot, including URL and observation coordinates,
fits 64 KiB of encoded UTF-8 JSON and reports truncation. Page-controlled strings
replace lone UTF-16 surrogates with U+FFFD before budgeting and serialization.
Nodes omitted to meet
that bound release their ElementHandles. Nodes are checked for attachment,
document, actionability and operation-relevant semantics before dispatch; tokens
never become caller selectors. Observation, successful mutation, navigation and
closure invalidate old references. URLs fit 8 KiB, fill values 16 KiB, and scroll
moves one viewport up or down. New page targets fail the single-page invariant.

PublicWeb allows anonymous public HTTPS/443. LocalDev top-level navigation is
restricted to one granted exact HTTP loopback origin, with same-host/port WS and
public HTTPS/443 resource dependencies. localhost, 127.0.0.1 and [::1] do not grant
each other authority; localhost is mapped to loopback without DNS. Grants bind
Session, BrowserId and Service epoch and end on close/reopen/withdrawal. Changing
mode or local origin requires closing and opening under fresh authority. The
browser never scans ports or imports user profiles. Rust validates typed
HTTP/CONNECT/WS broker requests independently of page-provided text. Chromium's
local WS CONNECT is terminated inside the helper; only a matching Host and
WebSocket GET upgrade is admitted to the exact granted destination. Ordinary
HTTP requests close their upstream connection.
Each completed helper result carries its current top-level URL, independently
of a snapshot; Rust revalidates it before accepting text or screenshots. The
initial blank document only permits the first navigation. A later `about:blank`
navigation is a policy escape and retires the scope.

Session screenshots are 1280x720 viewport PNGs. Source and canonical images each
fit 4 MiB; cumulative Media imports per BrowserId fit 32 MiB. Admission reserves
image capacity before import, preserves uncertain publication charges and checks
the returned Media identity and dimensions. Explicit input, codec and admission
rejections before publication refund the reservation. A successful but malformed
Media receipt does not establish that nothing was published and keeps the charge.
Tool results store MediaRef, while
human snapshots expose Session-authorized bounded image sources. Image failure is
explicit and does not rewrite an already completed page action. Media's retain-all
durability remains authoritative; Session input limits apply on attachment/submission.

Web and TUI present this contract through Session contributions: URL, structure,
latest screenshot, actions and finite status/error/budget diagnostics. Detach
only releases presentation. Host restart loses live page, node and grant state;
recorded Tool/Media evidence remains. No raw CDP or form values enter diagnostics.

The model Tool schema declares each operation's exact required and allowed fields.
Only click/fill accept document_version, observation_id and node; screenshot,
observe and close require only operation and binding. Invalid model arguments
return a bounded error Tool result marked not_dispatched before Browser admission,
so the model can correct its request without terminating the turn or replaying an
effect. Missing trusted Agent authority remains a Tool execution error.
Status and PublicWeb also reject extra fields during typed deserialization; a
public_web policy does not accept an origin field.
Schema descriptions state UTF-8 byte limits; JSON Schema's `maxLength` alone
counts characters. Argument errors identify the byte limit that was exceeded.
