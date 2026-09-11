# RSI desktop product fixture

`verify.py --binary /absolute/rsi-desktop --driver /absolute/WebKitWebDriver
--assets /absolute/bundle --report /absolute/new-directory` exercises the real
Linux Tauri window under `dbus-run-session -- xvfb-run -a`. It creates isolated
RSI paths and an empty configuration, then uses actual WebDriver input to configure
the deterministic provider, select a model, create a conversation and submit it.
Replacing input uses native select-all/backspace keystrokes and waits for the
empty value before typing. Nonempty composer replacement also requires a trusted
deletion input event: WebKit's WebDriver `clear` changes the DOM without updating
the draft editor through its input listener.
Screenshots, capabilities, request summaries and native cleanup logs are evidence.
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

`performance.py --binary ... --driver ... --documents /instrumented/documents
--report /new/report` compares ten 16/64/128-block scenes with actual WebDriver
input. Prepare the isolated document copies using the Web product fixture.
PSS includes the desktop process plus all descendant WebKit web/network processes;
all three settling samples are retained. `--smoke` checks harness admission only.
Input-to-paint uses two animation frames, not a claim about physical display scanout.
