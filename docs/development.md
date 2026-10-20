# Development guide

Develop against the ordinary RSIversi product: the same application Profiles,
provider configuration, tools and persistent sessions, built in debug mode.
The source launcher adds process supervision and Web hot updates on Linux/WSL
and native Windows. Run the examples from the repository root.

## 1. Choose state and prepare tools

Development uses your ordinary config, state and cache directories unless you
set absolute XDG overrides. Debug code can change those files. Before working on
Store or configuration formats, select separate roots:

```bash
development_root="$HOME/.local/share/rsi-dev"
export XDG_CONFIG_HOME="$development_root/config"
export XDG_STATE_HOME="$development_root/state"
export XDG_CACHE_HOME="$development_root/cache"
```

In Windows PowerShell:

```powershell
$env:XDG_CONFIG_HOME = Join-Path $PWD '.local/dev-product/config'
$env:XDG_STATE_HOME = Join-Path $PWD '.local/dev-product/state'
$env:XDG_CACHE_HOME = Join-Path $PWD '.local/dev-product/cache'
```

New config roots need their own provider configuration. The product prints its
resolved roots before opening state. For path resolution, Store compatibility
and existing Host conflicts, follow the
[application launch contract](../apps/cli/README.md).

Install the Rust/native prerequisites in
[source installation](user/source-install.md#prepare-your-machine).
For Web, use Node.js 22.18 or newer and prepare the pinned frontend tools:

```bash
bash scripts/rsiversi.sh setup web
```

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/rsiversi.ps1 setup web
```

For TUI-only work, use `setup tui` instead. Interactive Windows TUI requires
Windows Terminal. The source-install tutorial owns the remaining platform and
filesystem requirements.

The first launch builds a frozen product before starting source watchers. Linux
Web includes the release SSH helper used by ordinary remote tools. Web prepares
dependencies in both the captured build tree and the live source tree; these
serve different consumers. Allow time for this cold build. Later launches reuse
compiler and package caches.

## 2. Start the standard TUI

```bash
bash scripts/rsiversi.sh dev tui
```

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/rsiversi.ps1 dev tui
```

Configure a provider and model through the normal setup if needed, then use your
usual workspaces and conversations. Rust changes require restarting the command.
For native renderer watching and deterministic layout work, follow the
[terminal development tutorial](../apps/terminal/docs/tui-development.md), which
uses the explicit keyless fixture workflow.

## 3. Start Web with hot updates

```bash
bash scripts/rsiversi.sh dev web
```

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/rsiversi.ps1 dev web
```

Open the printed one-use launch link. The standard product signs in through its
normal local browser handoff; no manual device receipt is needed. Configure
credentials, a provider and a default model as described in
[your first conversation](user/first-conversation.md).
If pnpm is on PATH, `pnpm -C apps/web dev` invokes the same supervisor.

React feature edits use HMR. Rust and Worker edits need a new build and restart.
Add `--no-open` to print the link without opening a browser; `--no-watch` runs the
frozen Web product directly. Follow the [Web contract](../apps/web/README.md) for
pairing, reload behavior and shared browser identity when using daily Web too.

## 4. Stop development

Exit the TUI or press Ctrl+C in the terminal running Web, then wait for cleanup.
Successful runs clean their temporary output; failures retain evidence. The
[launcher contract](../apps/devtools/README.md#development-supervision) owns
process supervision, output retention and control readiness.

For a keyless check on Linux/WSL, use the explicit fixture:

```bash
cargo run --locked -p rsi-app-tools -- fixture-dev tui --smoke
```

This verifies the deterministic fixture provider and durable completion. It is
separate from ordinary product development. Use the existing source build
commands for build-only work and debug Desktop distributions.
