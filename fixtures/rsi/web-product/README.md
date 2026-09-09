# Web product verification

`npm ci && npm test` builds the actual Rust Web bundle and native RSI executable,
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

The runner freezes and hashes its native executable before starting either
service. Concurrent Cargo builds cannot change that owner's executable gate.
Only explicit non-secret build variables reach the test application, and an
absent private D-Bus address prevents access to the operator's native keyring.
`RSI_WEB_BROWSER=chromium` or `firefox` selects a diagnostic subset.

Screenshots and scenario results identify the browser version and viewport.
Settings scenarios wait for the acknowledged editor replacement before creating
a conversation, whose Header freezes the settings at creation.
Failures retain their evidence and stop that run. Shutdown always closes the
browser, exact service child, provider connections and fixture directories.
Neither credentials nor user state are read by default.
