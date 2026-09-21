# ACP interoperability

This fixture uses the exact TypeScript ACP SDK 1.4.0 in a separate Node process.
It communicates with the built product over stdio. It does not import RSI's
protocol driver, Session adapter or private composition implementation.

Install with `npm ci --prefix fixtures/rsi/acp`. Run `node
fixtures/rsi/acp/verify.mjs --binary target/debug/rsi --output OUTPUT`. The
default fixture starts an isolated deterministic provider and fresh product
configuration. Live verification is opt-in with `--live-env .local/dev/.env`;
only `DEEPSEEK_API_KEY` is read, and reports contain no credential values.

The fixture records initialization, new/prompt, paged native history load,
resume, close and process exit. Protocol interoperability, real provider
success and product visual evidence are distinct validation surfaces.

`agent.mjs` exercises the reverse role: the independent SDK is the Agent and the
Rust Host is its client. After installing the pinned package, run
`RSI_ACP_SDK_NODE="$(command -v node)" cargo test --locked -p rsi-acp-host
--test host independent_sdk_agent -- --ignored --nocapture`. It covers exact
permission options, resume without replay, 1,200-record load and process cleanup.
`agent.py` is the deterministic byte peer shared by Host and native delegation
and Linux PTY tests (including a Host without a native model); its results are not independent SDK interoperability evidence.

`RSI_WEB_ASSETS=/absolute/assets RSI_BINARY=/absolute/rsi node
fixtures/rsi/acp/browser.mjs /absolute/report` runs Chromium and Firefox against
the real service, Worker and independent SDK Agent. It starts a delegation through
the deterministic native provider and opens the same Host conversation from its
Tool card. It also covers
permissions, history/source reads, pane detach, resume/load, cancellation and
reaping, retaining wide and narrow screenshots. No real provider is used here.

Pinned DSH live interoperability is opt-in. `dsh-live.py --runtime RUNTIME
--source CHECKOUT --env-file FILE --report NEW_DIRECTORY` requires revision
`ddefc45fbc7f8e46dd73185e68295696d1297887`, an already built runtime closure and
`DEEPSEEK_API_KEY` in the authorized file. The closure is the checkout's
`dsh-python-runtime-closure` deployment with its host `node-addon-system` binding,
not the CLI package alone. It uses the ordinary RSI Host/Process/Sandbox services,
new/prompt, an actually called private MCP, exact file readback, close/resume and
reaping. DSH does not advertise load; this fixture never claims load support.
The credential is supplied to the test process and resolved through `rsi.acp`,
never written to a launch configuration or report. `dsh-observe.mjs` records only
process IDs before importing the unchanged built DSH entry.

`mcp.py` is the independent private MCP byte fixture shared by native ACP and
pinned DSH tests. Its optional call marker records fixture tool arguments, while
credential checks never return credential values.

`python3 fixtures/rsi/acp/opencode-live.py --assets /absolute/web-assets --report
/absolute/new-report` exercises an installed OpenCode through the actual RSI
browser, Host and managed ACP process. It explicitly selects the advertised model
and effort before prompting, then checks a real file edit, permission, resume,
load and reaping. It starts with `opencode/muse-spark-1.3-contributor-free` / `xhigh`;
only a confirmed provider availability failure during a tool-free probe permits
a separate attempt with `opencode-go/deepseek-v4.1-flash` / `max`. A tool-stage or
unknown transport failure does not replay work on another model.
`--fallback-after /absolute/earlier-report/result.json` reuses a recorded primary
availability failure and starts a fresh fallback attempt; its result retains the
original evidence path.

This opt-in fixture uses only the two selected providers from an explicitly
read OpenCode credential file (default `~/.local/share/opencode/auth.json`), held
in a private temporary directory removed on exit. OpenCode configuration and
sessions are isolated from user state. Its own SQLite message records establish
the actual provider/model/variant for all three prompts, independently of the
requested settings and browser text. Reports exclude credentials and raw provider
error bodies. The executable's reported version is evidence of the installed
binary, not proof that it was built from the reference source checkout.
