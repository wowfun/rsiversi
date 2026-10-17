# Install from source

[中文](source-install.zh.md)

This tutorial prepares a Git checkout, builds the application, and opens Web.
It does not install system components or change machine security settings.
A source ZIP is not supported. The first build compiles Rust and Web dependencies;
later launches use the published application directly.
Allow substantial time and disk space for the first release build: on a laptop
it can take over an hour. Each build stage prints the location of its `build.log`.
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
powershell -NoProfile -ExecutionPolicy Bypass -File .\rsi.ps1 setup
powershell -NoProfile -ExecutionPolicy Bypass -File .\rsi.ps1 build
powershell -NoProfile -ExecutionPolicy Bypass -File .\rsi.ps1 web
```

The execution-policy flag applies only to that process. Organization policy may
still refuse it; the script does not change policy. `setup` installs the repository
Rust toolchain and WASM target, plus repository-local pnpm and wasm-bindgen.
It can be run again. No Python or Make is required.

## Linux, WSL, or Windows Git Bash

<!-- rsi-example id=install-bash platform=git-bash -->
```bash
bash rsi.sh setup
bash rsi.sh build
bash rsi.sh web
```

Git Bash uses native Windows tools and state. It does not enter WSL. Web works
from mintty; the fullscreen TUI needs Git Bash inside Windows Terminal.

## Check, update, and locate the application

`doctor` reports missing prerequisites without installing or building anything.
Use `rsi.ps1 doctor` through the PowerShell invocation above, or
`bash rsi.sh doctor`. Follow its diagnostic, then retry.

`build` defaults to release. `build --debug` is available for development.
A failed build keeps the previous successful publication. Exit RSI before updating
source, then run setup and build again. Start Web to use the new publication.

`path` prints the current bundle directory. It contains `rsi.exe` on Windows
(`rsi` on Linux), `assets`, and its build receipt. You can launch that executable
directly with `web`; keep its generation layout intact. Launch and path do not
invoke Cargo, Node, or the builder. Managed generation directories are retained
while their application is running.

Continue with [your first conversation](first-conversation.md).
Build failures and long-path issues are covered in [troubleshooting](troubleshooting.md).
