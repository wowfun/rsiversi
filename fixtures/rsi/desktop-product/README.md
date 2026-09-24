# RSI desktop product fixture

The Python fixtures require Python 3.11 or newer.

`verify.py --binary /absolute/rsi-desktop --driver /absolute/WebKitWebDriver
--assets /absolute/bundle --report /absolute/new-directory` exercises the real
Linux Tauri window under `dbus-run-session -- xvfb-run -a`. It creates isolated
RSI paths and an empty configuration, then uses actual WebDriver input to configure
the deterministic provider, select a model, create a conversation and submit it.
Button clicks relocate only after WebDriver explicitly reports a stale element
before dispatch; ambiguous click failures are never replayed.
Replacing input uses native select-all/backspace keystrokes and waits for the
empty value before typing. Nonempty composer replacement also requires a trusted
deletion input event: WebKit's WebDriver `clear` changes the DOM without updating
the draft editor through its input listener.
Model selection waits for the completed setup action and the exact deployment's
enabled option; the earlier provider receipt alone does not finish its readback.
After scenarios close a dialog, the final composer geometry check waits for the
closed document view; native click delivery alone does not settle a Rust command.
The native window also reads and refreshes Plugins status, reads the separate
Exa credential status, and opens the typed retrieval Settings controls without
submitting a model request. Screenshots, capabilities, request summaries and
native cleanup logs are evidence.
Teardown attempts every owned process, stream and temporary-directory cleanup
even if another cleanup fails. Cleanup diagnostics attach to an active primary
failure; without one, cleanup failures make the run fail after all phases finish.
Failures record their startup/product phase and child exit codes, and print at
most 64 KiB from the daemon and WebDriver logs after replacing the exact live key.
The final sweep redacts decoded JSON strings and literal or JSON-escaped key
occurrences in text, log and HTML evidence. It rejects symlinks without reading
or modifying their targets; it does not scrub images or arbitrary encodings. A detected key fails an
otherwise successful run; during failure it adds a note to the original exception.
CI independently runs
conversation, missing-ACK and startup-close scenarios after the shared build.
The Tool-using task scenario additionally requires the native sandbox preflight;
missing-ACK and startup-close do not depend on that preflight. CI attempts artifact upload even on failure. A scenario
that never starts cannot produce product evidence.
Before launching a foreign build, the fixture records its frozen executable's
SHA-256 in `foreign-build/binary.json`. CI excludes the two native executable
paths in that directory, retaining the scenario evidence and the distribution's
receipt, build-family manifest and build log. The receipt identifies both paired
executables; these hashes cannot restore the programs for binary replay.
The paired `rsi` companion must already be beside the desktop executable.
By default the fixture does not access real user settings or credentials.
An explicit `--live-env-file /authorized/file --live-model model-id` enables
DeepSeek instead: it reads only that key into the isolated child environment,
checks actual tool-created file bytes and redacts evidence if the key appears.
`--window-close` exercises the platform window close request and retained drain.
`--restart` additionally reopens the same private state and checks the persisted
conversation and unsent draft before closing it again.
`--daemon` starts the paired headless companion first, then verifies that closing
and reopening the desktop preserves the same daemon PID and published Host epoch.
Its receipt checks both binary hashes and their common frozen input manifest.
`--save-failure` injects an IndexedDB write failure, verifies input survives a
cancelled window close beyond its original deadline, then explicitly recovers.
`--ack-timeout` withholds a frame ACK and requires failed-lifetime cleanup.
`--startup-close` stalls an isolated copy of the document bootstrap before it
installs its close listener, then closes twice. It requires one bounded deadline,
unsuccessful document-drain status and completed Runtime cleanup. This does not
claim successful draft saving from a document that never initialized.
`--foreign-binary PATH` with `--daemon` verifies rejection of a different build
family while sharing the same canonical headless companion.
`--refresh-during-click` forces a draft input refresh during native mouse down
on Send. It checks preserved text-node identity, exactly one real click and the
resulting submission; this gates the WebKit draft-save/click interleaving.

`--tasks` additionally drives recorded inline patch evidence, Goal pause/resume/
cancel and a real background Bash job through the native window. Explicit
provider gates expose the claimed round and original Jobs scope for observation.
Resume waits for both the disarmed driver and the settled Paused projection,
whose creation control is visible only after settlement. Known Goal rejection
ends every Goal-state wait immediately. The fixture retains bounded control
inputs (ticket, request ID and displayed revision), matching native replies and
observed Goal feedback on both success and failure.
An unreported completed job must fail finalization after its readonly panel was
used. These deterministic screenshots and native clicks remain separate from
the opt-in live smoke; `--tasks` cannot be combined with live mode.
The inline screenshot scrolls the recorded diff into view and records its full
containment and center hit, so DOM text alone cannot stand in for visible evidence.

`performance.py --binary ... --driver ... --documents /instrumented/documents
--report /new/report` compares ten runs of 16/64/128-block scenes, including a near-1-MiB text scene, with actual WebDriver
input. Prepare the isolated document copies using the Web product fixture.
PSS includes the desktop process plus all descendant WebKit web/network processes;
all three settling samples are retained. `--smoke` checks harness admission only.
Input-to-paint uses two animation frames, not a claim about physical display scanout.

`--terminals` additionally types into the real xterm widget in the Linux WebView,
verifies native Bash-created file bytes, detaches and reattaches read-only, takes
control explicitly, observes exit code 7 and closes the terminal. These screenshots
and receipts are separate from browser and parser-only evidence.

The terminal scenario also sends concurrent real native reads until the read lane
returns structured Busy, then imports a renderer module and writes terminal input
while reads remain pending. It records responses and exact file bytes; no native
response is mocked. Eight real raw PTY readers then stop at filesystem barriers; eight 64 KiB
writes fill the native write lane. Actual keyboard input receives Busy and must
execute exactly once after release, with exact bytes checked in all eight readers.
Adapter unit tests separately cover unknown-outcome non-replay. These are distinct evidence boundaries.

`--external` additionally exercises the independent ACP SDK 1.4.0 Agent through
actual WebKitGTK controls and the native bridge before configuring a Native model.
It validates four-option permissions without a local always-grant, literal remote
text, cancellation, 1,200-record replay, bounded display and process reaping.
Install `fixtures/rsi/acp` dependencies first; this scenario requires Node.


`--profiles` exercises Local grants, single-leaf preparation, reviewed source
publication, and original receipt recovery through the native bridge. Its
editable user Profile is separate from the running service source.
`--attention` exercises exact native question and approval navigation, settlement,
and removal from Needs attention with a deterministic provider.

`--typed-results` executes the real `host_profile` catalog Tool and opens its
recorded `rsi.profile-leaves` version 1 result through the native bridge. Portable
addon execution and the independently linked addon are covered by their own
fixtures; this scenario establishes the desktop recorded-contract presentation.

`--history` exercises lexical search, original reread and Chinese fragment freezing
through the actual native bridge, then reopens the captured draft reference.

`--workspace-review` adds a real native patch in a dirty Git workspace and opens
its interval summary, changed file and diff through the actual GUI bridge. It
records `workspace-review.json` and screenshots and requires system Git.

`--language /absolute/rust-analyzer` additionally queries and opens a definition
through Service extensions and the native bridge, then reads hover. The fixed
server and project are owned by the [language fixture](../lsp/README.md).
It also pages a retained reference result after replacing the private source with
an oversized file, verifies that explicit Repeat query rejects that current file,
then restores the original and verifies Repeat query recovers.

The `--file-previews` scenario also verifies the native application frame gate and
captures the actual application/bootstrap response bodies and CSP in
`preview-responses.json`. The separate opt-in `csp_engine.py --responses PATH
--report PATH` probe runs under Xvfb with system Python, PyGObject and WebKit2 4.1
GI. It checks custom-scheme parent origins, including a foreign-origin transport
control and rejection of foreign parents even when frame-ancestors is removed.
This separates the engine's custom-scheme frame-ancestors limitation from the
product navigation gate and the bootstrap's one-use origin-bound handshake.
The desktop CI job runs both scenarios. The engine probe serves no-store responses
and unique case URLs so restrictive and permissive responses cannot share a cache hit.
It also injects iframe/object/embed from admitted HTML, records CSP violations,
custom-protocol requests and engine navigation decisions, and compares a permissive
frame/object policy control. The product fixture checks the actual WebView's nested
element violations separately; engine decision callbacks do not claim to be Tauri callbacks.

Native close scenarios send WM_DELETE_WINDOW on their isolated Xvfb display using
libX11, rather than granting the document a test-only Tauri capability. The normal
`--window-close` scenario also proves the Tauri close command is denied, then
observes Application cleanup after the native event.
Before native close or keyboard input, the fixture verifies that DISPLAY belongs
to a same-user Xvfb spawned by an ancestor of this fixture process. Bare invocations
on an existing desktop display fail before sending events. Save-dialog input also
checks X input focus after raising the selected visible chooser and before keys.

`--export` drives the Rust-owned native Save dialog using XTest keys on the
private Xvfb display, verifies JSON bytes and cancellation, and asserts no
additional model requests. It requires libXtst and does not automate host dialogs.
