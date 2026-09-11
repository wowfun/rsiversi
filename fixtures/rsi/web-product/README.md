# Web product verification

First install the shared document's pinned build dependencies with
`npm ci --ignore-scripts --prefix ../../../plugins/rsi/web` from this directory.
Then `npm ci && npm test` builds the actual Rust Web bundle and native RSI executable,
then drives the product document and Dedicated Worker in Chromium and Firefox.
The runner also exercises document-only reset/reopen and unresolved-submission
button projection using the shipped JavaScript; those checks are distinct from
Worker and transport evidence. Install those Playwright browsers first. `RSI_WASM_BINDGEN` selects a matching
wasm-bindgen executable. `RSI_WEB_ASSETS` and `RSI_WEB_BINARY` can select already
built artifacts explicitly; `RSI_WEB_REPORT` selects a new evidence directory.

The fixture owns an isolated real Service Host, temporary settings and Workspace,
ephemeral TLS/H2 listener and a bounded deterministic OpenAI-compatible provider.
Device credentials are registered through the real local operator application.
Browser commands pass through the product Worker, API and Agent Kernel. The
provider supplies fixed replies and Tool requests; this verifies mechanisms and
rendering, not autonomous model capability. Real provider validation is opt-in
and recorded separately.

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
application retains its input mode; signing out and reconnecting applies the
saved Enter behavior while restoring contributed Session and Files actions.
The fixture checks Shift+Enter and a real plain-Enter submission in that mode.

Image cases import generated PNGs as separate binary bodies through authenticated
HTTP, reorder their references, inspect the actual provider request's image hashes
and dimensions, and reopen an exact durable source using the same object URL as
the draft preview. Removal starts no model call. Both sign-outs verify that every
document-created URL was revoked. Synthetic document cases separately exercise
object-count/byte retention, LRU reuse/eviction, late-read fencing and edited
attachment preservation during unresolved submission; they do not validate Media
bytes. Rust public-application tests cover import ownership, limits, image-only
unknown retries, frozen request boundaries, and detail/application cancellation.

Persistent composer checks use real IndexedDB transactions in two documents,
including revision conflicts, record reincarnation, origin quotas, opaque u64
requests and receipts, transaction aborts and malformed durable metadata.
Document composer tests gate saves before repeated Send/reconcile input and
automatic recovery, checking single admission and the absence of false conflicts.
They also check cached UTF-8 accounting without re-encoding inactive editors.
These use the shipped document methods with a controlled Worker reply, not a
live Session or provider. `recovery.mjs` owns
a separate frozen executable and Service, restores a live Fresh Session and its
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
Only explicit non-secret build variables reach the test application, and an
absent private D-Bus address prevents access to the operator's native keyring.
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
and the independent Rust/WASM renderer on both browsers. Admission tests pause
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
supplied pre-migration asset directory and the current document source. It injects
bounded synthetic 16/64/128-block projections and observes actual input through
two animation frames. Production files are never rewritten. The comparison keeps
the same native Host/Tauri harness and includes its WebKit web/network process
PSS; it isolates document rendering and persistence, not provider or transport
latency. Each engine has separate samples, artifacts and screenshots.
