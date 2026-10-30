# Web product verification

Settings saves wait for a refreshed editor ticket and enabled controls. React
retains the editor nodes across saves, so node removal is not a completion signal.
The document runtime fixture checks catalog namespace reads within one window,
while explicit section reads and dismissal continue to fence old frames.
External navigation checks configured endpoint visibility, reopening and paging,
and reuse of the same conversation identity after an uncertain start.
Draft recovery opens a local conversation before entering Conversation options.
Goal lifecycle probes read
the current Session status and assert the visible driver and control state;
the status mirror itself is not a visible workbench label.

The accessibility component fixture mounts the production React activity and
notice owners with the document styles in Chromium and Firefox. It checks
visible activity labels and accessible names, compact rows for conversations absent
from the attention snapshot while preserving explicit unknown states, separate persistent storage and
ordinary notices, borderless composer focus in ordinary themes and a system-color
outline in forced colors. It also checks transparent resize hit areas during hover
and drag, keyboard boundary indicators and pointer/arrow-key resizing on both
sides. This is component/CSS evidence, separate from Worker/HTTP verification.
The document projection fixture preserves completion option nodes during arrow
selection and checks current transcript visibility without per-block scroll
geometry reads. Initial inline-card visibility waits for the first intersection
observations, including when render, scroll or resize schedules an earlier frame.
Image DOM checks mount the actual React details and connection-owned image cache.
They cover parallel reads, release of a removed interest before its reply, stable
canonical URLs across tickets, explicit retry and URL disposal on retirement.
The cache's capacity, byte accounting and LRU invariants have adjacent Node tests.

The review lifecycle fixture exercises current React resource owners against real
IndexedDB, including delayed opens and window replacement. It also covers
Settings editor mode switches, exact JSON retention, UTF-8 byte limits
and read-only controls after rebuilding the form. Device-storage tests
verify that failure to reserve cleanup aborts the layout transaction. UI alignment
uses the unified Settings window and verifies disclosure Escape/outside dismissal,
focus return, Verbose inline source actions, modal Escape, transcript keyboard regions and responsive geometry. These are
document/runtime checks; `plan-live.mjs` separately exercises an explicitly selected
real provider through the paired Worker and native service.
The opt-in alignment live fixture uses one isolated text file and a configured
DeepSeek model to check rendered table/code output, an actual read-only file Tool,
Stop and a subsequent successful turn. It records durable Facts and provider usage,
checks the original file bytes, and closes its browser and Service before scanning
text evidence for the supplied key. It requires the explicit live environment,
model, limits and report variables used by the other live fixtures.
Windows UI alignment verifies known-path workspace entry; native directory
browsing/creation retains its Unix-only contract. Live DeepSeek profiles bind the
request's default output cap to the explicitly supplied default output reserve,
so a provider default cannot silently exceed the fixture's declared capacity.
Manual inspection can explicitly select the fixture's loopback development HTTP
mode; TLS remains the automated default. Windows fixture process teardown uses
Node's TerminateProcess, records that limitation, then independently stops the
isolated Host through its normal command. It is not graceful application-exit
evidence; document disconnect and Host drain have their own assertions.
On Unix, watched frontend fixtures use SIGINT, matching the foreground Ctrl+C
contract. Ordinary Serve fixtures use SIGTERM. Both require exit code zero
within the shutdown deadline before the isolated Host is stopped.
Before deleting an isolated home, cleanup retains its Host diagnostic log when
it is at most 1 MiB, including failures before service readiness.

`workspace-model-settings.mjs` exercises workspace hover, rename and delete,
provider edits, discovery selection and partial-save retry against the real React
document with deterministic transport replies in Chromium and Firefox. Set
`RSI_REPORT_DIR` for its screenshots and results. `model-settings-live.mjs` is an
opt-in shipped Worker/Service check: it starts with no providers, uses the key
from `RSI_LIVE_ENV_FILE` through the UI, discovers models, configures DeepSeek,
and sends one short request. It verifies persistence after a Service restart and
registration-only workspace deletion while preserving a file and Session history.
Set `RSI_WEB_BINARY`, `RSI_WEB_ASSETS` and `RSI_WEB_REPORT` explicitly. It uses an
isolated home and loopback development HTTP, and closes its Service and browser.
Windows fixtures create the native filesystem's exact protected private ACL
(TokenUser owner; explicit full control for SYSTEM, Administrators and TokenUser)
on their newly created configuration directory before writing settings. They
never repair user paths.

The Settings draft fixture checks Plugins-to-Advanced namespace routing through
Web retrieval on native Windows. MCP management requires native Unix Profile
authority, so Windows asserts its editor entry is absent; Unix additionally
checks the MCP editor entry. Missing Windows MCP management is not a fixture
timeout or a claim of supported management.
The live plan probe reads up to 512 Facts and 16 MiB from its own SQLite database
in one read-only transaction for durable evidence; this avoids the Unix-only
interactive Line input adapter. Native terminal execution remains Linux-only.
The Dock fixture reports that execution as skipped on other platforms. On Windows
it exercises the native terminal refusal through the paired Worker and checks
that no cleanup responsibility or retry banner is created.

`verify.mjs` runs `worker-conformance.mjs` before the acknowledgement deadline
probe against the published Worker and Rust/Wasm module. The fixture supplies
its verified build family and an isolated registered device receipt. It checks
presentation-frame acknowledgements, valid commands, structured Rust admission
refusals for malformed commands, continued use after rejection, and zero live
request/timer resources after disconnect. It adds no production test export and
does not assert that Serde unit variants reject unknown fields.
`settings-drafts.mjs` verifies exact drafts across sections, concurrent-writer
conflicts, exact refreshed contents after explicit reload and Worker refusals before connection. `restore-tabs.mjs`
reopens sixteen persisted file views through the real document admission queue.
Settings also verifies Plugins links select the requested namespace.
Both run in the product Web CI step and use `RSI_REPORT_DIR` for a new report
directory and the paired publication. Settings checks include keyboard focus after
save/discard; `review-lifecycle.mjs` separately covers deterministic reply ordering,
invalid JSON, untouched mode switches and safe read recovery while awaiting a fresh ticket.
Restoration setup duplicates tabs through the product menu; it does not import
Dock internals or write IndexedDB records. The restored run requires exactly 16
open replies and no local or global cleanup notices.

Running fixtures select one paired publication and hold its
[generation lease](../../../apps/devtools/README.md) through Node lifetime,
including service restarts. Receipt hashes are checked after lease acquisition;
all launches use the original binary and adjacent artifacts. Build the supervisor
with `cargo build --locked -p rsi-app-tools` or set `RSI_APP_TOOLS_BINARY` for a
custom target directory. The readiness handshake is bounded to 30 seconds and
32 bytes; helper failure fails the fixture. Explicit cleanup allows five seconds
for EOF, then kills and reaps the helper within another five seconds and reports
the timeout. Only deliberate asset-mutation probes
copy assets. Pure PNG test data comes from `png.mjs` without browser dependencies.
Startup failure also kills and reaps a noncooperating helper. Stderr is drained
before reporting early exit; cleanup diagnostics supplement the primary error.
If admission fails before a Host exists, cleanup observes `host status` and
does not attempt to stop an absent owner. Other cleanup failures retain the
isolated home authorities for diagnosis.
The verified `paired.sha256` also supplies binary reports, avoiding a second
full executable read within each process. Each independent child still acquires
its own lease and verifies its artifacts.

The shared `paired-env.mjs` entry also serves the ACP, language, history,
Profile and workspace-review browser fixtures. Its exported `paired` value is
the verified binary/assets selection. Use `RSI_PAIRED_BUNDLE` for a complete
publication, or supply both `RSI_WEB_BINARY` and `RSI_WEB_ASSETS`; otherwise the
current generation record selects the publication. Every such fixture needs
the lease supervisor above, even when given an explicit distribution.
Simultaneous bundle and binary/assets overrides must resolve to the same pair;
conflicting inherited overrides fail instead of silently selecting another bundle.
The supervisor canonicalizes an explicit bundle override before passing it to
children, so a child working-directory change cannot reinterpret a relative path.
`fixture-startup.test.mjs` checks those sibling entrypoints in both explicit
selection modes up to the Service launch boundary, with synthetic artifacts
and a stub Service; it does not claim browser or native transport coverage.
The startup/cleanup failure test uses a real Node child with invalid readiness,
stubbing only executable selection and certificate generation. It runs on
Windows and Linux without a POSIX shell or OpenSSL installation.

The document projection and download checks load the shipped Desktop bridge
module; its marker selects browser/native conveniences without granting authority.
The document projection checks compile an authored entry against the real product
module graph using `source-fixture.mjs`; they never extract or rewrite production
functions. All authored fixture roots resolve from their module location, so the
repository and CI fixture working directories load the same product graph.
`RSI_REPORT_DIR=/absolute/report node fixtures/rsi/web-product/presentation-invariants.mjs`
checks folded delegation/source actions, summary reading state, action errors,
layout retry persistence, resource snapshot identity, disclosure dismissal and
light/dark text contrast in Chromium and Firefox. The Markdown fixture verifies
plain streaming code and highlighting after settlement.
`external-drafts-dom.mjs`, called by `dom.mjs`, exercises real
composer controls, shared drafts, unknown outcomes and capacity recovery in
desktop and narrow viewports. A MutationObserver checks that repeated frames and
unchanged input do not rewrite the live alert region. Accepted edits in a sibling
pane clear stale capacity notices. These checks provide browser rendering evidence
without claiming ACP transport or live-provider coverage.

CI runs `navigation-order.mjs` and `device-storage.mjs` alongside the Dock probes.
The storage probe exercises real IndexedDB migration, concurrent writes, rollback,
and principal isolation in an isolated Chromium profile.

`RSI_REPORT_DIR=/absolute/new/report node dock-durability.mjs` exercises concurrent
resource opens in two authenticated windows, layout reload, viewport-only float
clamping, injected IndexedDB write failure and the sixteen-tab bound. Product
controls perform every layout operation; database reads inspect the committed
document, and write faults are confined to the isolated browser context.

`RSI_REPORT_DIR=/absolute/new/report node navigation-order.mjs` creates 65 durable
Sessions through the paired product's authenticated browser and deterministic
provider. It checks full-member cross-page keyboard moves, actual drag-and-drop,
cross-window order notifications, persisted selection after responsive remount,
nonempty row titles and internal/root overflow in eight light/dark viewports. It
also registers 65 real directories through the product and moves workspace
siblings through the complete registry seed. Reports identify the exact build
family and fixture provider; these are product behavior/visual observations,
not live model evidence or DSH pixel-golden acceptance.

Source-inspection geometry resolves the current connected resource, its title
and controls in one document execution. A replaced resource is read again before
measurement; invalid geometry on the current resource remains a failure.

The opt-in `mcp-images-live.mjs` uses the authorized DeepSeek credential file,
an isolated HTTP MCP server, the real Worker and durable Media CAS. It checks
expanded template URIs, ordered durable Tool images, text-model follow-up,
source-image preview and clean sign-out. Captures cover light/dark system themes
at 1440×900, 1024×768, 767×900 and 390×844 at DPR 1; geometry checks detect
overflow. These are runtime/visual observations, not DSH pixel-golden acceptance.

Task captures require named controls to be present, enabled and reachable after
scrolling, and reject visible product notices as well as JavaScript errors.
Hidden banners are not feedback; making a nonempty banner visible still fails.
Named controls must hit directly. For incidental transcript controls beneath the
floating Back to bottom button, geometry additionally verifies that centering the
control by scrolling makes it hit, then restores the reading position. Other
occlusion still fails; the report preserves both the initial and revealed result.
They await asynchronous visible-card mounting before measuring the current control; a
viewport change is not settled merely by two animation frames. Inline-card
captures first scroll their retained Tool block into view, because offscreen
blocks do not retain executable card mounts. The task
probe accepts only Chromium or Firefox and cannot pass with no selected engine.
Hit tests resolve and measure the current named button in one document execution,
because a standard-view refresh can replace a button after actionability checks.
This does not replay a control action or accept a connected but obscured button.
Workflow readiness polling can scroll but never invokes a control. After
readiness, one document execution
resolves, hit-tests and dispatches the current enabled button. If that final check
finds it unavailable, only readiness is repeated within the same 30-second deadline.
A delayed poll after timeout cannot dispatch, and an ambiguous final dispatch is
never repeated. A renderer refresh
between mouse down and mouse up can otherwise retire that gesture's handler.
An unavailable control times out without dispatch; admitted actions are never
repeated to obtain the expected view.
It injects one rejected visible-card hint and requires an exact same-sequence
retry through the document/Worker boundary. `task-checks.test.mjs` separately
proves hidden, disabled, obscured and missing controls fail these assertions.
CI retains reports under `RSI_WEB_REPORT`, command logs and explicit step outcomes.
Readiness failure logs are uploaded before the paired build. Product failure
captures and Workflow reports are uploaded before later integration probes,
so those probes cannot delay access to the first failure's evidence.
A successful product step must produce `results.json` with passing Chromium and
Firefox results; skipped steps remain visibly skipped.
Recovery, task and Workflow probes write `binary.json` with the leased executable's SHA-256
before starting the scenario. CI retains hashes, screenshots, action receipts,
logs, assets and durable diagnostic data.
The archive identifies the executable but does not contain it for binary replay.
Failure metadata is written even when a crashed renderer prevents HTML or
screenshot capture; capture diagnostics supplement the original task error.
Renderer replacement records the scenario phase and bounded Worker start,
authentication reply, failure and termination observations without credential
payloads. A connecting screen alone does not identify which lifecycle stage waits.
Conversation-switch failures retain bounded navigation gesture and Worker
command/reply metadata plus current pane identities. The trace records only
navigation actions and IDs, never connection credentials or message contents.
It distinguishes a missing click from a dispatched open still awaiting its reply;
the expected Session identity and saved-draft assertions remain unchanged.
Service startup also retains its original failure when teardown fails; cleanup
diagnostics cannot replace the readiness or startup error.
Goal controls retain bounded Worker invocation/reply evidence and intermediate
control errors. Resume after a completed paused round waits for both the live
driver and the delivered durable settlement projection, rather than treating
their independent streams as one notification. Cancel after resume also waits
for the second Turn's held response in the transcript and the displayed Waiting
driver, then exercises the resumed controls at both viewport sizes. A provider
request arriving server-side alone does not establish a delivered client state.
Workflow captures retain bounded Worker invocation/reply evidence before and
after restart, including the last control requests on failure.

First install the shared document's pinned build dependencies with
`pnpm -C ../../../apps/web install --frozen-lockfile --ignore-scripts` from this directory.
The local-launch scenario also injects classified HTTP rejections to verify that
Worker diagnostics distinguish capacity and retired-service failures from an
expired ticket, without cookie fallback concealing a definitive rejection.
Build a paired publication with `pnpm -C apps/web build ABSOLUTE_PACKAGE_DIRECTORY --debug` from the repository
root. Then `npm ci && npm test` consumes its Rust Worker and native executable and drives the product document and Dedicated Worker in Chromium and Firefox.
The runner also exercises document-only reset/reopen and unresolved-submission
button projection using the shipped JavaScript; those checks are distinct from
Worker and transport evidence. The document-only and gated Worker fixtures load
the shipped admission module, so their transport limits and request classification
match the product. Install those Playwright browsers first. `RSI_WASM_BINDGEN` selects a matching
wasm-bindgen executable. `RSI_PAIRED_BUNDLE` selects a complete existing publication.
Explicit `RSI_WEB_ASSETS` and `RSI_WEB_BINARY` must pass the same receipt checks; `RSI_WEB_REPORT` selects a new evidence directory.

The fixture owns an isolated real Service Host, temporary settings and Workspace,
ephemeral TLS/H2 listener and a bounded deterministic OpenAI-compatible provider.
Device credentials are registered through the real local operator application.
Browser commands pass through the product Worker, API and Agent Kernel. The
provider supplies fixed replies and Tool requests; this verifies mechanisms and
rendering, not autonomous model capability. Real provider validation is opt-in
and recorded separately.

Stream fragments settle into one model-attempt Fact. Reopening a completed
180-fragment response retains its first fragment and one exact source reference;
fragment count does not manufacture history or source pagination. Settings checks
use the current output-cap array, preserving exact JSON when the typed form cannot
represent an integer. The document-only schema fixture covers numeric controls.

`RSI_WEB_REPORT=/absolute/new/report node fixtures/rsi/web-product/settlement.mjs`
uses the paired publication in Chromium to observe a gated Unicode stream,
explicitly reconnect and reopen it after document refresh, then settle with `MaxTokens`. Desktop and
narrow screenshots cover both preview and durable output. It verifies one source
reference, no duplicate text, the visible stop reason and no automatic second
model request. This fixture uses the isolated deterministic provider.

The opt-in `live.mjs` checks actual tool-created bytes, a completed turn and a
clean document before recording success. Product notices fail the live check;
successful model output alone does not establish visual acceptance.
`RSI_WEB_BROWSER` selects Chromium (default) or Firefox for this probe.
The isolated GUI tool probe caps each model response at 4096 tokens and allows
seven minutes for its turn, including the provider's finite connection retries.
Live DeepSeek probes require `DEEPSEEK_CONTEXT_WINDOW_TOKENS`,
`DEEPSEEK_DEFAULT_OUTPUT_RESERVE_TOKENS` and `DEEPSEEK_MAX_OUTPUT_RESERVE_TOKENS`
for the selected model, matching the provider's native live test. These explicit
capacities replace form examples; the maximum must accommodate the provider's
configured request output default. The fixture does not infer capacities from
a model name or from successful credential discovery.
Live probes share bounded credential-file parsing and profile construction;
the maximum output reserve must be at least the default reserve. Failed-request
diagnostics retain at most 64 entries with bounded fields and count omissions.

`node verify.mjs --primary-only` runs pairing, local launch and the primary
product scenarios in both browsers (or `RSI_WEB_BROWSER`). It omits the separate
renderer, recovery, task, preview and plan probes and their native/Wasm fixture
builds. The default command continues to run those probes too.
Its Files cases create raw POSIX byte names and symlinks, so the complete primary
runner requires Unix. On Windows, use the focused workbench, UI-alignment and
document/Worker fixtures; a partial primary run is not full-suite evidence.
Pairing rejection requires the listener activation diagnostic, no readiness,
and a still-unbound requested port. A generic mention of Web or assets does not
count as proof that malformed bundle admission was exercised.

The default runner includes `tasks.mjs`, which uses the same real Worker,
Service and provider with explicit
response gates. With `RSI_WEB_ASSETS`, `RSI_WEB_BINARY` and a new `RSI_WEB_REPORT`,
it checks recorded inline patch evidence after a later filesystem edit, Goal
create/pause/resume/cancel with distinct durable and live state, and an actual
background Bash job observed without reporting its output. Both browsers retain
desktop/narrow screenshots, containment and Send hit-test measurements. Gates
hold the model stream while the fixture observes actual process state; they
measure lifecycle mechanisms, not model ability. The unreported completed job
must still fail its Turn at finalization, proving that panel reads did not report
the job or consume its retained output.
The no-new-model-request assertion begins only after the current job's tool-result
follow-up enters the provider gate. A running job or retained assistant text does
not establish that the executor has sent this normal follow-up request.

Files scenarios browse an actual unpublished Session through authenticated HTTP,
including directory snapshots, byte pagination, text/hex display, Linux non-UTF8
filenames, no-follow links and Changed followed by explicit refresh. They verify
that browsing starts no model request and capture its actual generic UI cards.

Completed process output is read through the actual output-cache API. The fixture
captures stdout beyond 16 KiB and raw NUL/non-UTF8 bytes in both streams, then
checks separate text/hex cards and pagination. The source/result inspectors still
preserve the failed process status and literal arguments.

Markdown scenarios exercise actual Worker parsing and closed DOM construction:
Unicode emphasis, lists, code, an HTTP(S) link, literal HTML and an image alt label
without image elements. Separate document tests simulate composing and key-code
229 Enter events and verify that keyboard submission stays suppressed; they do
not establish native IME platform behavior.

Client preferences are edited through the generic Settings UI. The current
application applies its input mode after the successful save; signing out and
reconnecting retains that behavior while restoring Session and Files actions.
The fixture checks Shift+Enter and a real plain-Enter submission in that mode.

Image cases import generated PNGs as separate binary bodies through authenticated
HTTP, reorder their references, inspect the actual provider request's image hashes
and dimensions, and reopen an exact durable source using the same object URL as
the draft preview. Two real Worker reads must reach the browser's HTTP request
barrier before either request is dispatched; an upload must finish while both
are held. Releasing them continues the browser's original HTTP/2 requests. The
separate Rust Media API test holds two reads inside its provider; browser request
overlap is not presented as provider execution overlap. Removal starts
no model call. Both sign-outs verify that every
document-created URL was revoked. Synthetic document cases separately exercise
shared cache/read byte retention, display pins, exact-ticket pending deduplication,
bounded parallel reads, LRU reuse/eviction, late-read fencing and edited
attachment preservation during unresolved submission; they do not validate Media
bytes. Rust public-application tests cover import ownership, limits, image-only
unknown retries, frozen request boundaries, detail/application cancellation, and
read-slot/byte admission retained through the final returned byte clone or slice.

Persistent composer checks use real IndexedDB transactions in two documents,
including revision conflicts, record reincarnation, origin quotas, opaque u64
requests and receipts, terminal queue-conflict retirement without draft loss,
transaction aborts and malformed durable metadata.
Document composer tests gate saves before repeated Send/reconcile input and
automatic recovery, checking single admission and the absence of false conflicts.
They also hold draft loading after a Session header changes. Switching acceptance
waits for the enabled composer before checking exact saved text, since the header
can arrive while IndexedDB is still loading the draft.
They also check cached UTF-8 accounting without re-encoding inactive editors.
Normal autosaves remain silent, with unchanged composer, textarea and Send-button
geometry while persistence is held and after it completes. Failures and pending
request recovery retain visible controls.
These use the shipped document methods with a controlled Worker reply, not a
live Session or provider. `recovery.mjs` owns
a separately leased publication and Service, restores a live Fresh Session and its
canonical image, checks cross-tab conflict controls and device-cookie fencing,
then restarts the actual Service to expire the Fresh Session. Explicit recreation
moves the saved input only after the replacement opens. An aborted save before
restart must retain the cached local editor and require explicit conflict
resolution before recreation. Corrupt counters must show a storage-unavailable
notice immediately on connection, before opening a composer. A dropped real dispatch
reply followed by reload must query the original message without another provider
request. Both browsers capture desktop and narrow recovery controls.

The runner freezes and hashes its native executable before starting either
service. Concurrent Cargo builds cannot change that owner's executable gate.
Agent tree cases create an actual child through the model Tool path, inspect its
recorded parent and history through authenticated HTTP, and close the shared
detail slot while preserving both main panes. No synthetic tree enters the UI.
Only explicit non-secret build variables reach the test application. Isolated
Host paths keep credential files separate from operator state.
`RSI_WEB_BROWSER=chromium` or `firefox` selects a diagnostic subset.

Screenshots and scenario results identify the browser version and viewport.
Settings scenarios wait for the acknowledged editor replacement before creating
a conversation, whose Header freezes the settings at creation.
Failures retain their evidence and stop that run. Shutdown always closes the
browser, exact service child, provider connections and fixture directories.
Neither credentials nor user state are read by default.

The shared service fixture accepts optional setup and request-observation
callbacks for independent addon acceptance. Setup runs against isolated paths
before any application starts; request observation receives only the local
deterministic provider's admitted request. The standard product run uses neither.

Incremental-frame checks distinguish document projection from lifecycle evidence.
Synthetic document snapshots and patches assert stable DOM identity, retained
focus, upsert/remove/order, rejected stale bases and generation replacement. A
separate actual Dedicated Worker opens both Session panes through authenticated
HTTP, deliberately receives no valid acknowledgement, and proves one pending
frame, the 30-second deadline and zero Rust Worker resources after its drain.
A wrong frame ID neither releases that frame nor renews the deadline.
The product runner also counts real Worker snapshots, patches, block upserts and
frames without pane changes. It deliberately requests resynchronization for one
received patch and verifies the subsequent snapshot before continuing the same
Files workflow. Only frame counts and sizes are retained by this instrumentation.

The default runner also executes renderer admission, real A/B/C module replacement
and the independent Rust/WASM renderer on both browsers. Their explicit asset
lists include the shared Worker admission module, including cold catalogs without
an executable renderer. Admission tests pause
asynchronous setup and disposal, drain accepted input, reject stale hosts and bound
document imports across reconnects. Offers with no active slot remain pending;
failed first mounts retain a resident fallback. Authenticated product scenarios
also open a cold executable-but-broken bundle and verify Session input, one Worker
and clean sign-out on both browsers. A real module Worker with gated
WASM export fixtures separately checks sign-out during acknowledgement and
renderer commit, while still reporting failures on a connected Worker.
Replacement preserves a pending turn and both
resident and form drafts while holding the old lazy-import graph until commit.
The native UI fixture then reads its actual Session through a scoped Portable API
grant and sends arbitrary models and binary source windows through authenticated
UI API into the Rust/WASM DOM renderer. Synthetic four-slot ABI evidence and this
actual business path are reported separately. The runner builds both standalone
fixtures with their own lockfiles; `RSI_RENDERER_ASSETS` and `RSI_NATIVE_UI_ARTIFACT`
can select prebuilt diagnostic artifacts.
Synthetic Rust/WASM slots use a separate document owner from the product. Async
disposal and graph-release predicates use bounded, awaited polling rather than
passing a Promise to Playwright's synchronous `waitForFunction` predicate.

Renderer verification also drops the HTTP reply after a real server commit. The
actual WASM owner closes without replay, explicit reconnect starts a new observer,
and the new renderer and Session send/cancel actions work before clean sign-out.

The isolated Service fixture explicitly grants each product-test device configuration
authority through the Local Devices application before exercising Settings writes.
Registration itself remains unprivileged. Grant bypass and revocation tests use the
configuration owner's separate authenticated API fixtures.

The opt-in `prepare-performance.mjs` creates instrumented copies of an explicitly
supplied baseline document source directory and the current document source.
Both use the current pinned build dependencies and generated assets; source and
scene hashes are recorded. Instrumentation format 2 identifies each original entry
by path and SHA-256 (`app.js` for the baseline, `src/main.tsx` for current source).
Preparation copies the generated assets once, uses
that frozen copy for both variants, and records every source, generated asset
and final output hash together with the build dependency lockfile. Each source
copy retains its `apps/web` position in an isolated repository shape. The shared
descriptor reader and Rust-owned wire/bundle declarations are frozen once and
copied into both variants, with their hashes included in the evidence. Dependency
directories are excluded on Windows and Unix; only current pinned Web dependencies
are linked into the copies. Vite preserves
the generated assets while replacing the compiled document outputs. From the
repository root, run `RSI_WEB_ASSETS=/absolute/bundle/assets node
fixtures/rsi/web-product/prepare-performance.mjs /absolute/baseline/document-source
/absolute/new-documents`. The baseline must contain the document's source and
Vite configuration. It injects
bounded synthetic 16/64/128-block projections, including 128 blocks near the
1 MiB text limit, and observes actual input through
two animation frames. Production files are never rewritten. The comparison keeps
the same isolated WebKitGTK MiniBrowser harness and includes its web/network
process PSS. Instrumented copies are not product bundles and cannot be loaded by
rsi-desktop; this benchmark excludes pairing, native Host and transport costs. Each engine has separate samples, artifacts and screenshots.

Performance results distinguish block count from source text bytes. Chromium
also reports post-scene JS heap and DOM counters; Firefox has no equivalent CDP
sample. Time, heap and native process-family PSS are report-only. The assertions
cover actual typed input, concurrent single-block updates, usable input geometry,
and an error-free document. Document scenes do not measure Rust frame caching.

Model/effort acceptance reads the adapter-declared choices, changes the durable
Session selection without submitting, verifies the next actual provider request,
and restores the declared default. These controls use the same Worker and HTTP
path as ordinary messages; synthetic selector changes alone are not evidence.

`terminal-verify.mjs`, with explicit `RSI_WEB_BINARY`, `RSI_WEB_ASSETS` and
`RSI_WEB_REPORT`, exercises real Bubblewrap Bash through the browser: Unicode,
oversized and short OSC/DCS filtering at the Worker reply boundary before xterm,
admission-backpressure recovery and takeover sizing, absence of terminal-created style/link nodes, pane detach, readonly reattach, explicit takeover, retained
shell environment, resize, exit status and close. It records Chromium/Firefox
screenshots and checks that terminal operations do not invoke the model. Actual
computed font/whitespace, CSP violations and stylesheet disposal are asserted;
text appearing in the DOM alone does not establish a correct terminal grid.
The same scenario switches Host, current preset and resident Session diagnostic
views without an additional model request.

`preview-verify.mjs`, with explicit `RSI_WEB_BINARY`, `RSI_WEB_ASSETS` and a fresh
`RSI_WEB_REPORT`, exercises editable Markdown Agent completion and actual child
Tool calls, then code, Markdown, SVG and isolated interactive HTML through the
real Worker and Service. It checks the next spawn after an edit, empty Tool allow,
invalid-definition diagnostics, local CSS imports/images/scripts, parent/native
bridge isolation, default blocked network, per-preview HTTPS opt-in, frame reset,
explicit file refresh and over-budget source fallback. Chromium and Firefox
retain full-size and narrow screenshots. Its provider and HTTPS resources are
controlled fixtures, separate from opt-in live-provider evidence.

The default runner also executes `preview-verify.mjs` on each browser: real
Markdown Agent definitions, next-fresh-spawn refresh, and bounded Files code,
Markdown, PNG/SVG and opaque HTML previews. `preview-live.mjs` is a separate opt-in
using `RSI_LIVE_ENV_FILE`, `RSI_LIVE_MODEL`, `RSI_WEB_BINARY`, `RSI_WEB_ASSETS` and
a new `RSI_WEB_REPORT`; it checks real parent/child replies and tool-created HTML.
`RSI_WEB_BROWSER` selects Chromium (the default) or Firefox for this live probe.
The parent prompt explicitly requests a verbatim child reply, including its source
identifier. Assertions require the current definition marker in the child's reply,
the matching durable completion input, and the parent's quoted reply; ordinary
paraphrasing alone does not satisfy this traceability probe.
Multi-turn live waiters record the prior Fact sequence before submission and
require the submitted Human input's own `turn_terminal`. Clearing the draft or
observing a previous `Completed` status does not acknowledge the new Turn;
completion may also arrive without a sampled intermediate `Running` status.
`evidence.test.mjs` checks this causal matching without a live credential.
Live fixture cleanup attempts every resource close before scanning all evidence
text for credentials, even when a close fails. Cleanup and redaction failures
are both reported. Redaction tests exercise ordinary nested files independently
of link creation. The no-follow fixture uses a directory junction on Windows
and a file symlink on POSIX, verifies the native link type, and checks that the
external target remains unchanged. Default Windows checks require neither
administrator privileges nor Developer Mode; they do not claim file-symlink
creation coverage on hosts that forbid it.

Preview probes include catalog overflow, encoded image dimension rejection before
any image `src` assignment, and actual frame-src/frame-ancestors behavior with a
foreign loopback origin and an otherwise identical control with frame-ancestors
removed. Node document tests additionally
cover raster formats, bootstrap parent-origin admission and inline script closing
sequences. Native custom-scheme engine evidence belongs to the desktop fixture.

`export-verify.mjs`, with explicit `RSI_WEB_ASSETS`, `RSI_WEB_BINARY` and a new
`RSI_WEB_REPORT`, verifies native Session exports through the real Worker/API and
scoped Service Worker in Chromium and Firefox. It checks downloaded JSON/Markdown
bytes, semantic diagnostics, filename hints and unchanged provider request counts.

The download transport probe additionally transfers over 32 MiB with a gated
producer, rejects Blob buffering, and checks cancellation and abnormal EOF at
the application and Service Worker boundaries. It gates abnormal EOF until the
browser admits the download, so response failure cannot race initial download
observation. Per-mode progress is retained in `download-stream-trace.json` even
when the transport check fails. It records browser download-manager
failure separately in `download-stream.json`. Firefox currently can leave an
errored streaming download pending after the producer is released and the
application reports failure; the fixture explicitly cancels that browser artifact
for cleanup. A pending manager result does not establish a completed failed-download
UI transition. Chromium reports the failed download directly.

`plan-review.mjs` runs in the default verifier and is also a targeted closed-review probe using the actual document,
Worker, Host and a deterministic provider. With explicit `RSI_WEB_REPORT` and
the paired publication selected as above, it checks literal plan rendering, three explicit choices,
feedback, narrow containment and the next provider request after atomic approval
in Chromium and Firefox. It retains the executable hash, screenshots and exact
mock requests; this does not establish live model or native Desktop behavior.

`plan-live.mjs` is opt-in via `RSI_LIVE_ENV_FILE`, `RSI_LIVE_MODEL`,
`RSI_WEB_REPORT` and a paired publication selected as above. It reads only the authorized DeepSeek key,
uses an isolated Workspace, explicitly approves a harmless computation plan,
then requires durable successful save/review/spawn/read Tool facts and the actual
structured result `answer: 42`. Reports distinguish the model and effort from
mock evidence and record zero mock requests. Cleanup attempts every resource owner,
including after browser launch/close failure. It recursively sanitizes all regular
JSON/JSONL/log/text evidence files before reporting any credential match; links
are excluded. Usage comparisons require nonempty, finite positive token counts.

`schedule-live.mjs` uses the same explicit live environment as `plan-live.mjs`.
It creates one short one-shot reminder, waits for the actual automatic Turn,
and verifies the human/automatic provenance and retained allowance through CLI
history. Browser screenshots inspect ordinary Tool results at wide and narrow
widths. No repeating work is created, and fixture shutdown owns all live timers.

`program-live.mjs` additionally requires an absolute `RSI_TEST_NODE`. Its isolated
Host enables the configured Node runtime and selects the builtin `workflow`
preset, which includes the Program coordinators. It uses the same explicit live
variables to compare four ordinary
file reads with one `run_code` coordinator over the same files and model. It checks
Program-origin Facts, curated output and actual provider usage. Each sample opens
its own conversation; the next sample restores All conversations before using the
Workspace create control. A paired sample
reports measured tokens and time; it does not establish a general speedup.

`workflow-live.mjs` additionally requires the opt-in Node runtime in its isolated
Host and Agent preset. `RSI_WORKFLOW_MODE` selects `background` (default),
`foreground` (one-second observation followed by detachment), `cancel`, or `plan`.
The completion scenarios verify two structured children admitted after the creator
settles, exclusive run receipts, one bounded notice and a successful result read.
Cancellation scenarios verify explicit control or plan-mode revocation and direct
Node reaping. All keep real provider evidence, zero mock requests and browser
screenshots separate from deterministic Kernel and process tests.

`profile-status.mjs` is keyless and targeted to diagnostics. With explicit
`RSI_WEB_REPORT` and a paired publication selected as above, it introduces a parse failure into an isolated
watched Host Profile, checks the unchanged desired revision and advanced observed
revision, then restores the source. Chromium and Firefox verify the redacted last
attempt, recovery, narrow containment and zero model requests.

The targeted probes share browser setup, receipt connection and Workspace opening
through `browser-fixture.mjs`. Browser setup closes a partially created owner on
failure; each probe retains its service/evidence cleanup in `finally`. Program
and workflow probes share one checked standard-preset transformation.

`local-launch.mjs` exercises the built `rsi web` entry with the real static
bundle and Worker in Chromium and Firefox: fragment removal, automatic cookie
login, credential/Provider/default-model configuration, reload/new-tab recovery,
consumed-ticket rejection, cookie recovery after a lost exchange result, manual
login, configuration grant revocation, stable identity after restart and SIGINT
cleanup. Run it with explicit
`RSI_WEB_BINARY` and `RSI_WEB_ASSETS`; `RSI_WEB_BROWSER` can select one engine.
It owns temporary HOME/XDG paths and uses no live model provider.

The default queue scenario holds an actual Rust Turn at its deterministic provider,
replaces one queued input, verifies that its explicit access policy prevents
conversion into steering, withdraws it and
then stops the original Turn. It checks that a second queued input actually
executes and that the ordinary draft survives. IndexedDB probes independently
verify opaque queue requests across an unknown reply and reopen. Native addon
probes must use an `RSI_NATIVE_UI_ARTIFACT` built with the same build-family manifest
as a frozen `RSI_WEB_BINARY`; artifacts from a different family are rejected.

`ui-alignment.mjs` drives directory validation, physical folder creation, complete
pins, primary Enter/Queue, Stop draft retention, code copying and browser refresh
with reconnect. It captures 390/768/1024/1440/1920-pixel viewports under light,
dark and system appearance. Screenshot files are human comparison evidence, not
pixel goldens. The React document checks separately observe Turn reclassification,
unchanged DOM child identity and bounded to one CSS pixel reading-anchor preservation.
Shared `controls.mjs` opens the actual Resources, detail and navigation controls;
fixtures identify conversation rows by Session ID rather than visible title text.
The same helpers open the directory picker and onboard a provider through
Models → Add model provider, waiting for the editor to close and the selected
default to become usable. Credential status is automatic; fixtures do not rely
on diagnostic receipt text as a form-completion signal.
`RSI_WEB_STALE_DEFAULT=1` runs the workbench onboarding scenario with a removed
default route, proving Settings can install and select a replacement.


The UI alignment probe also checks explicit workspace refresh after a metadata
edit invalidates group cursors: pinning publishes the complete pinned region,
shows a refresh control for the stale group, and refreshing excludes its pinned
conversation without silently restarting a retained continuation.

UI alignment also injects failures into the layout database alone: failed saves
and loads show a notice, in-memory layout remains usable, and draft restoration
continues from its independent database. Recovering a later save clears its
notice; a failed load remains explicit for that connection.

The document-only fixture mounts the product root with explicit transport and
renderer ports, including the real lazy reading-position capture. It runs in both
browsers as part of the default fixture test entry. Controlled transport gates
check that a native-close event during authentication waits for storage and the
disconnect receipt, and that failed teardown blocks creation of a replacement
Worker. These are application/DOM checks; actual native window and live transport
evidence belongs to the desktop and opt-in live fixtures.

Layout-persistence evidence polls completed IndexedDB reads until at least one
record exists, with an explicit deadline. A Promise object is not accepted as a
truthy persistence result.

`RSI_REPORT_DIR=/absolute/isolated/report node device-storage.mjs` exercises current
presentation-store modules in two actual isolated Chromium pages. It verifies the
schema upgrade, transaction serialization, 64/65 order movement, abort rollback,
notification and principal/draft isolation without a Service or model. It is a
storage check, not a substitute for paired product or visual validation.

The opt-in `ssh-targets.mjs` probe requires explicit isolated sshd/session/auth
executables, account, fixture directory and optional shared-library path through
`RSI_TEST_SSH*`, plus `RSI_TEST_HELPER_BIN` for an independently built static
helper. It installs that helper through the ordinary package command after the
Host starts. It uses the paired fixture distribution and the fixture-only Local
`profile-leaf-probe` (`RSI_PROFILE_PROBE` may select its built path). It generates
its own keys, confirms trust through the Local API, submits candidates through
the Web UI and verifies real musl helper connections. Exact HTTP directory requests
prove Use denial before grant and after revocation; stale connection epochs cannot
disconnect a replacement. Its light/dark captures check the SSH form, not remote
Session tools, the full target picker or DSH golden alignment.

The native service fixture selects its Service Profile through `rsi.toml` and
uses the ordinary resident Host. Fixture restart and teardown explicitly stop
that Host before changing configuration or removing its isolated homes.

`RSI_SSH_LIVE=1` additionally opts the SSH probe into `live-fixture.mjs` with
`RSI_LIVE_ENV_FILE`, `RSI_LIVE_MODEL` and matching `RSI_WEB_REPORT`/`RSI_REPORT_DIR`.
It opens an SSH Session from the actual picker, checks target project instructions,
Bash and apply-patch results with DeepSeek, inspects Files/Review, and exercises an
SSH terminal. No key is read without this explicit mode. Reports retain model
facts and transport/build identity; credentials are redacted on cleanup.

`review-lifecycle.mjs` also delays a restored navigation read across another
window's Updated selection, checking that automatic reconciliation preserves the
committed mode and empty saved order for both Session and workspace navigation.
Controlled storage replies separately verify that older layout reads and saves
cannot overwrite a newer local intent, and overlapping invalidations are followed
by a fresh read. A failed serialized save keeps the latest intent and displays
recovery; explicit retry must commit that intent before clearing the notice.
These are React lifecycle checks, not transport evidence.
It runs current React and storage modules through an isolated
Vite server in Chromium. It retires Dock initialization during open, read and
reopen; verifies no late focus/subscription installation; checks unchanged host
snapshot stability and detail-model publication; and inspects actual IndexedDB
writes during concurrent intents.
At 16 tabs and 64 history entries, it counts coordinate copies to reject a second
history replay during intent persistence. A second connection then injects an
invalid inverse; the next intent must reject it without changing its revision.
Timing samples are descriptive evidence, not machine-dependent pass thresholds.
Set `RSI_REPORT_DIR` to retain its deterministic result. It does not use a Service,
provider credentials or installed bundle and does not replace paired visual checks.

`RSI_REPORT_DIR=/absolute/new/report node dock-panels.mjs` drives independent
resource panels through the paired product bundle. It verifies split/float,
Settings preservation and terminal attachment geometry in Chromium, saving
screenshots and assertions. CI invokes it alongside Dock durability and the
current-module lifecycle/storage checks; native geometry is covered separately
by the desktop fixture's `--dock` mode.

The SSH target probe also stalls a second isolated TCP endpoint before SSH
negotiation. While that target is busy, it verifies an unrelated candidate write
and a directory read on the real SSH target, and checks the same-target Busy
response. It closes the stalled socket explicitly and requires a known connection
failure; this is contention evidence, not a successful second SSH connection.

The opt-in Workflow workbench fixture uses `RSI_TEST_NODE=/absolute/node
RSI_WEB_REPORT=/new/report node fixtures/rsi/web-product/workflow-verify.mjs` after
a paired build. Chromium and Firefox exercise readiness, empty and cold history,
actual parallel child receipts, user cancellation, Node reaping, frozen script and
CAS result reads, child inspection and narrow rendering. Its provider is keyless.
Terminal cancellation readback rejects the actual pending-cleanup label; the
keyless Program fixture test positively checks that this assertion rejects it.
Intermediate UI frames may coalesce during native cleanup.
`workflow-live.mjs` separately accepts `RSI_WORKFLOW_SKILL=workflow-collect` with
explicit live environment/model/report inputs and records actual Skill body read,
read-only run authorization and result evidence.
For non-Skill runs it checks the actual submitted script/background/observation
arguments and correlates a successful terminal `workflow_read` result to its Tool
intent, exact run and terminal control sequence. Completion-notice text alone
cannot satisfy that read. `RSI_WORKFLOW_MODE=foreground` exercises observation
timeout detachment; `plan` exercises `/plan on` revocation and exact Node reaping.

The CI Workflow browser gate runs `workflow-verify.mjs --dock-only`: readiness,
canonical run/result/script/child reads, restart, narrow layout and cancellation
with actual Node cleanup. The default command additionally checks Workflow inline
navigation. Those inline assertions remain a separate opt-in integration check;
passing the Dock gate does not establish inline navigation evidence.

`automation-verify.mjs` is an explicit Automation presentation check. Supply
`RSI_WEB_BINARY`, `RSI_WEB_ASSETS`, `RSI_AUTOMATION_WEB_SEED` and a new
`RSI_AUTOMATION_WEB_REPORT`. The seed is created by the ignored core
`automation::prepare_visual_fixture` test through PolicyOwner and Ledger, using
an actual normalized native checker PNG. It exercises the real Worker/API,
per-rule Device grants, retained fixed verdict and new-attempt control in wide
light and narrow light/dark views. The injected exploration report is labelled
fixture data; this scenario does not claim webhook or live Goal execution.
The service is stopped before changing the isolated seed's grants and restarted
before client negotiation. It never creates a second Service Host owner.

The Automation scenario also injects a display-only investigation link into a
Worker presentation frame and an empty surface projection. It verifies that the actual
Open action displays its refusal inside the modal. The report labels this as
presentation fault injection; it does not claim that the injected Session exists
or that an authorization boundary was exercised by that action.

`session-browser-live.mjs` is an explicit Linux, live-provider acceptance. Supply
live environment/model/report and a paired binary/assets publication, plus the
three `RSI_TEST_BROWSER_*` paths and `RSI_TEST_BROWSER_DIGEST` of the installed
immutable runtime. It uses actual human UI mutations, exact local HTTP/WS grants,
canonical screenshots, wide/narrow rendering, Unicode history selection and
workspace-bound model Tools. It correlates durable Tool intents/results and
checks that LocalDev model opening required human Approval.
After submitting Approval, it remembers that exact owner/request ID and waits
for the pending control to retire. Recreating a DOM node or reconnecting cannot
repeat the same reply while a stale pending projection remains visible.
DeepSeek's current provider adapter is text-only: image Media is retained and rendered to humans;
this probe verifies structured page text and never claims model pixel inspection.
Chromium network-change failures are retained with their failed requests and
screenshots. During a submitted turn the probe permits at most two explicit
Reconnect actions, only after `ERR_NETWORK_CHANGED`, and reopens the same saved
Session without resubmitting input. Read-only UI polling that times out during
that teardown may recover; actions are not retried and other read failures
propagate. Durable Facts must contain exactly one
submission of each prompt. Recovery is reported separately from uninterrupted
acceptance; other transport or renderer failures still fail the run.

`RSI_REPORT_DIR=/absolute/new/report node fixtures/rsi/web-product/standard-image.mjs`
checks the current standard renderer with the real MountTable in Chromium. It
verifies activated source reads, replacement during a pending read, bounded windows,
visible incomplete, invalid and rejected-source errors, suppression of retired
read failures, Close joining source work, and object URL disposal. This is keyless document
lifecycle evidence and does not establish Service or native runtime behavior.

`RSI_REPORT_DIR=/absolute/new/report node fixtures/rsi/web-product/history-picker.mjs`
checks the current History dialog in Chromium with controlled replies. Query and
range edits retire old pagination controls; continuation and subsequent paging
use the current workspace and query. This keyless document check does not claim
History authority or live-provider evidence.

`development.mjs DIRECTORY ORIGIN NEW_REPORT` exercises the explicit Linux/WSL
`cargo run --locked -p rsi-app-tools -- fixture-dev web --directory ABSOLUTE_NEW_DIRECTORY`
environment. It retains the generated fixture Profile, deterministic provider,
manual device receipt and explicit local-HTTP login. Ordinary `pnpm dev` uses
standard product login and caller state instead.

The product service fixture runs `rsi.application.serve` against the shared Host
and selects its independent Web bundle with `--assets`. Its foreground connection
owns only the HTTPS listener; cleanup explicitly stops the isolated Host.
On Windows the fixture applies a private ACL to its temporary configuration
directory before launch, and records Node's SIGTERM/TerminateProcess shutdown
separately from a graceful exit. Both admission and cleanup branches have isolated
service tests in the Windows tooling job.

Service readiness allows 210 seconds for a cold debug executable to publish its
immutable Host image before binding. This is distinct from the fixture-lease
helper handshake above; default-debug Linux images can include over 1 GiB of DWARF.

The renderer replacement fixture uses ordinary `rsi --watch web` with a declared
source recipe. Each edit assembles and admits an independent Web package; the
resident listener stages renderer changes through its package publication API.
The copied bootstrap/Worker bundle stays fixed; only the renderer manifest,
entry and lazy module are observed source inputs. The compiled WASM artifact is
not a source input and exceeds the source owner's per-file observation bound.
It connects through the printed one-use browser launch URL. Cold static/broken
renderer cases continue to use the authenticated Serve fixture.

Manual-receipt browser setup carries the Service's explicit development-HTTP mode
to the login checkbox before connecting. HTTPS leaves the option unchanged.
The isolated live Fact reader selects at most 513 ordered metadata rows and stops
on the first count or accumulated-byte overflow (512 Facts, 16 MiB). Only an
admitted selection is loaded and parsed, in the same read transaction.
Browser evidence capture is best effort on failure. Fixture teardown attempts
both browser and Service cleanup and retains the original error if either fails.

`review-lifecycle.mjs` renders 64 cleanup failures with the owning component and
stylesheet at 1440 px and 390 px, checks the last Retry control by keyboard, and
retains screenshots. Set `RSI_REVIEW_PREVIEW=1` with `RSI_REPORT_DIR` to keep that
isolated component scene available at the printed local URL for manual inspection.
This scene verifies cleanup layout; it is not a full connected-workbench capture.

`responsiveness.mjs` uses the real document factory with isolated transport ports.
It checks tablet navigation and desktop preference behavior, the rail logo's
hover/focus swap and click/keyboard expansion in both themes, modal initial focus
and return focus, and a frame containing sixty settled code blocks. That frame
must ACK its plain-text presentation before optional tokenization runs. This is
source/DOM evidence; real Service and provider coverage remains in the paired
product and opt-in live fixtures.
Conversation identity assertions read the pane's `data-session-id`; coordinates
inside a closed tools menu are not visible text. Presentation assertions either
open that menu or inspect its text content explicitly.

CI runs the workspace, model settings, starting transcript and composer document
fixtures in both Chromium and Firefox. The workspace navigation document fixture checks the grouped options menu,
keyboard focus, tri-state archive filtering, model-independent directory
registration, Show more expansion semantics, and the resource launcher before
a Session exists. The presentation-invariants fixture also checks that tab-strip
scroll offsets survive selection but release removed panes after repeated churn.
Source fixtures bind Vite's public HTTP server to a loopback, kernel-assigned
port, so independent fixtures can run concurrently without competing for a
default development port. Their helper test checks concurrent listeners and CSP.
Authored source fixtures are immutable during a run and disable filesystem
watching; supervised development owns the separate HMR tests.
The starting
conversation fixture checks saved text across reload and model setup, the first
send's durable handoff, concurrent-window conflicts, underlined transcript tabs,
message geometry, stable docked composer geometry while statistics arrive or
change, and light/dark/narrow screenshots in Chromium and Firefox.
These isolated transport fixtures use production React and IndexedDB; native
first-use initialization and directory browsing have separate owner tests.

`process-transcript.mjs` checks DSH-style process disclosures, live trailing
status, typed failure badges, activity rings and the More menu in Chromium and
Firefox. It covers streaming frame retention, independent row expansion,
settlement folding, reduced motion and narrow-screen geometry without a provider.

`composer-controls.mjs` checks starting-draft handoff without submission, the add
menu, permissions and reasoning choices. `composer-initialization.mjs` separately
checks initial effort metadata, stable empty-draft geometry, and scroll-based
Back to bottom visibility in Chromium and Firefox. Set `RSI_REPORT_DIR` for
its screenshots and measurements. The composer controls fixture also covers
command menus, model effort selection, access confirmation and frozen-request
locking, focus restoration and narrow popup bounds in Chromium and Firefox.
It uses the actual document and isolated transport. `composer-controls-live.mjs`
is opt-in through `RSI_LIVE_ENV_FILE`, `RSI_WEB_BINARY`, `RSI_WEB_ASSETS` and
`RSI_WEB_REPORT`; it verifies a real DeepSeek effort selection and sandbox mode
in persisted Session facts, with screenshots from the shipped package.
