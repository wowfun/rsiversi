# RSI desktop product fixture

The Python fixtures require Python 3.11 or newer.

`verify.py --binary /absolute/rsi-desktop --driver /absolute/WebKitWebDriver
--assets /absolute/bundle --report /absolute/new-directory` exercises the real
Linux Tauri window under `dbus-run-session -- xvfb-run -a`. It creates isolated
RSI paths and an empty configuration, then uses actual WebDriver input to configure
the deterministic provider, select a model, create a conversation and submit it.
Replacing input uses native select-all/backspace keystrokes and waits for the
empty value before typing. Nonempty composer replacement also requires a trusted
deletion input event: WebKit's WebDriver `clear` changes the DOM without updating
the draft editor through its input listener.
Model selection waits for the completed setup action and the exact deployment's
enabled option; the earlier provider receipt alone does not finish its readback.
Screenshots, capabilities, request summaries and native cleanup logs are evidence.
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
