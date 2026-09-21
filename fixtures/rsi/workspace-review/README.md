# Workspace review acceptance

`RSI_WEB_ASSETS=/absolute/assets node fixtures/rsi/workspace-review/browser.mjs REPORT`
uses the real CLI HTTPS service, a deterministic model provider and Chromium / Firefox.
It applies a native patch in a dirty Git worktree, inspects the durable interval,
opens the exact diff and checks a narrow viewport. UI reads cause no model calls.
Git and browser dependencies are explicit; all state is private temporary fixture data.
The shared surface renderer also has a native PTY integration test and the desktop
product fixture's `--workspace-review` scenario exercises its real WebKit bridge.

Opt-in live acceptance additionally supplies `RSI_LIVE_ENV_FILE` and
`RSI_LIVE_MODEL`. Only this explicit mode reads `DEEPSEEK_API_KEY` from that file;
it uses Chromium, the actual DeepSeek provider and the real native patch Tool.
No key is written into the report. The default scenario never reads credentials.
