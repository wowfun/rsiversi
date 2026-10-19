# Install from source

[中文](source-install.zh.md)

This tutorial prepares a Git checkout, builds the application, and opens Web.
It does not install system components or change machine security settings.
A source ZIP is not supported. The first build compiles Rust and Web dependencies;
later launches use the published application directly.
Allow substantial time and disk space for the first release build: on a laptop
it can take over an hour. Progress on stderr identifies the target, release/debug profile, stage, command and `build.log`; long stages emit a heartbeat every 15 seconds. `--verbose` also mirrors tool output. Stdout contains the final JSON result.
Later builds reuse compilation caches; starting Web does not compile anything.

## Prepare your machine

On Windows, use Windows 11 24H2 or newer, x64, and a short checkout path on a
fixed local NTFS drive. Install Git for Windows, Rustup, Node.js 22.18 or newer,
and Visual Studio Build Tools with **Desktop development with C++**, MSVC x64,
and a Windows SDK. Open a new PowerShell after installation.

On Linux or WSL, install Git, Rustup, Linux Node.js 22.18 or newer with npm,
a C compiler and musl tools. On Ubuntu, the system packages include
`build-essential musl-tools`. WSL must use Linux Node, not the Windows
Node executable inherited through PATH. Native macOS is not a source-publishing
target of this tutorial.

Clone the repository:

<!-- rsi-example id=install-clone platform=all -->
```bash
git clone https://github.com/wowfun/rsiversi.git
cd rsiversi
```

## Windows PowerShell 5.1 or 7

Run these commands in order:

<!-- rsi-example id=install-windows platform=windows -->
```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\rsiversi.ps1 setup web
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\rsiversi.ps1 build web
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\rsiversi.ps1 web
```

The execution-policy flag applies only to that process. Organization policy may
still refuse it; the script does not change policy. `setup` installs the repository
Rust toolchain and WASM target, plus repository-local pnpm and wasm-bindgen.
It can be run again. No Python or Make is required.

## Linux, WSL, or Windows Git Bash

<!-- rsi-example id=install-bash platform=git-bash -->
```bash
bash scripts/rsiversi.sh setup web
bash scripts/rsiversi.sh build web
bash scripts/rsiversi.sh web
```

Git Bash uses native Windows tools and state. It does not enter WSL. Web works
from mintty; the fullscreen TUI needs Git Bash inside Windows Terminal.

## Choose what to build

`doctor`, `setup`, `build` and `path` accept `tui`, `web` or `desktop` as their first argument. Omitting it selects **tui**; the examples above explicitly select Web.

| Target | Published application | Additional prerequisites |
| --- | --- | --- |
| `tui` | TUI, CLI, Headless, ACP, HTTP API and management in `rsi` | Rust and native compiler only; no Node, WASM or musl |
| `web` | Native `rsi` plus paired Web assets and Worker | Node/npm, pinned pnpm and wasm-bindgen, WASM target; Linux also musl |
| `desktop` | Web bundle plus `rsi-desktop` | Linux GTK 3 / WebKitGTK 4.1 development libraries; Windows WebView2 Runtime at launch |

For Desktop, run `setup desktop`, `build desktop`, then `desktop`. For native TUI, use `setup tui`, `build tui`, then `tui` in Windows Terminal or a Linux terminal. The native catalog and built-in Profiles remain complete for every target; selecting a build target does not prune dependencies by runtime Profile.

Linux Web/Desktop include the SSH helper. Linux TUI opts in with `setup tui --with-ssh-helper` and `build tui --with-ssh-helper`. The helper always uses release, including for a debug application. Without it, product SSH targets are unavailable; ordinary SSH login and local TUI still work. Windows rejects the helper flag.

Each target has its own current publication and compilation cache. Clients that need one Linux Service Host must use the same family: after `build web`, use `web` and `tui --from web`; after `build desktop`, use `desktop`, `web --from desktop` or `tui --from desktop`. Windows requires stopping the existing interface before opening another on the same Host. There is no fallback between targets.

## Check, update, and locate the application

`doctor` reports missing prerequisites without installing or building anything.
Use `scripts/rsiversi.ps1 doctor web` through the PowerShell invocation above, or
`bash scripts/rsiversi.sh doctor web`. Follow its diagnostic, then retry.

Use `--help` or `-h` for an overview, or `build --help` for build options.
Help for `doctor`, `setup`, `build` and `path` works before setup and without
published artifacts. `web --help`, `tui --help` and `desktop --help` use the built application's
help and require a successful build.

<!-- rsi-example id=install-help-windows platform=windows -->
```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\rsiversi.ps1 --help
```

<!-- rsi-example id=install-help-bash platform=git-bash -->
```bash
bash scripts/rsiversi.sh build --help
```

`build` defaults to TUI release. `build web --debug` is available for Web development. An optional new absolute output directory publishes outside the managed cache; it does not change the current publication.
A failed build keeps the previous successful publication. Exit RSIversi before updating
source, then run setup and build again. Start Web to use the new publication.

`path web` prints the current Web bundle directory (`path` alone selects TUI). It contains `rsi.exe` on Windows
(`rsi` on Linux), `assets`, and its build receipt. You can launch that executable
directly with `web`; keep its generation layout intact. Launch and path do not
invoke Cargo, Node, or the builder. Managed generation directories are retained
while their application is running.

Complete argument and publication rules are in the [source entrypoint contract](../../apps/devtools/README.md).

Continue with [your first conversation](first-conversation.md).
Build failures and long-path issues are covered in [troubleshooting](troubleshooting.md).
