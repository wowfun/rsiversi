# Install from source

[简体中文](../i18n/zh-CN/user/source-install.md)

Install the core once. TUI, CLI, Headless, ACP and management are linked plugins
available through `rsi`. Web, Desktop and the SSH helper are independent packages;
adding or replacing them does not rebuild or replace the core executable.

## Prepare your machine

Use Git and the Rust toolchain pinned by this checkout. On Windows, use Windows
11 24H2 or newer, x64, a short path on fixed local NTFS, and Visual Studio Build
Tools with Desktop development with C++, MSVC x64 and a Windows SDK. TUI needs
Windows Terminal. Linux/WSL needs a C toolchain; on Ubuntu install
`build-essential pkg-config libdbus-1-dev`. These instructions do not cover native macOS.

<!-- rsi-example id=install-clone platform=all -->
```bash
git clone https://github.com/wowfun/rsiversi.git
cd rsiversi
```

## Install the core

The installer builds only `rsi` in release mode and adds its `bin` directory to
your user PATH. Open a new terminal after installation. No UI selection is needed.

<!-- rsi-example id=install-windows platform=windows -->
```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\rsiversi.ps1
```

<!-- rsi-example id=install-bash platform=linux -->
```bash
bash scripts/rsiversi.sh
```

<!-- rsi-example id=install-launch platform=all -->
```bash
rsi
```

Use `--prefix DIR` / `-Prefix DIR` for another install root, or `--binary FILE` /
`-Binary FILE` for an explicitly trusted existing executable. The installer
refuses to overwrite an existing installation. `--no-path` / `-NoPath` leaves
PATH untouched for isolated verification. It does not open config or state.

## Add Web or Desktop

With a configured signed package repository, adding an interface uses ordinary
product commands. Set `[repository]` in the [launch document](../../crates/rsi/core/docs/launch.md).
The [package contract](../../crates/rsi/packages/README.md) describes release trust; a test key is not an official release key.

<!-- rsi-example id=install-plugins platform=all -->
```bash
rsi plugin install web
rsi web
rsi plugin install desktop
rsi desktop
```

For local source packages, install Node.js 22.18 or newer, the pinned pnpm version,
the WASM target and matching wasm-bindgen. WSL must use Linux Node.

<!-- rsi-example id=install-web-tools platform=all -->
```bash
npm install --global pnpm@12.6.0
pnpm -C apps/web install --frozen-lockfile --ignore-scripts
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --locked --version 0.2.127
```

Build and install the Web package into the same product homes:

<!-- rsi-example id=install-web-windows platform=windows -->
```powershell
node apps/devtools/build.mjs web "$PWD/.local/packages/web"
rsi plugin install --local "$PWD/.local/packages/web"
rsi web
```

<!-- rsi-example id=install-web-linux platform=linux -->
```bash
node apps/devtools/build.mjs web "$PWD/.local/packages/web"
rsi plugin install --local "$PWD/.local/packages/web"
rsi web
```

Desktop uses `node apps/devtools/build.mjs desktop ABSOLUTE_NEW_PACKAGE
--web-package ABSOLUTE_WEB_PACKAGE`, followed by `rsi plugin install --local`
for both directories. Windows needs WebView2 at launch; Linux builds also need
`libgtk-3-dev libwebkit2gtk-4.1-dev`. The Desktop package pins its exact Web dependency.
An SSH helper is built separately on Linux with `build.mjs ssh-helper`; prepare
the same-CPU musl target and compiler first, then install it through `rsi`.

Omit `--debug` for release packages. Build output directories are new by default;
`--replace` is for an explicitly selected source output. See the
[producer contract](../../apps/devtools/README.md) and [development guide](../development.md).

TUI and Web share the same resident Host on Windows and Linux. Closing a UI
detaches it. `rsi web stop` stops the browser listener, while `rsi host stop`
stops the shared service. Continue with [your first conversation](first-conversation.md).

## Upgrade and transfer existing data

After stopping the Host, place `--reset-config`, `--reset-state`, or both before
the usual application command. They retain backups, credentials and installed
packages. For example, `rsi --reset-config --reset-state tui` starts TUI with
fresh owned configuration and state. Help never performs a reset.

Use `rsi core upgrade` with the repository identity pinned into the executable, or
`rsi core upgrade --local ABSOLUTE_CORE_PACKAGE`. The command stages and probes
the admitted executable, stops the selected Host, replaces the core and verifies
the new Host before completing. The previous executable and receipt remain in
the installation directory. Recover an interrupted transaction with
`rsi core recover` and the same global home options; if `rsi` is temporarily
absent, invoke the retained `.rsi-previous-…` executable. A configured mirror must
keep the pinned name and key; a source build without a pinned identity requires
`--local`. Windows upgrade requires a private current-user installation and
preserves shared-directory ACLs by refusing the operation.

For an explicit data transfer, stop both Hosts, then use global options for the
destination and import options for the old homes:

```text
rsi --config-home NEW_CONFIG --state-home NEW_STATE import --config-home OLD_CONFIG --state-home OLD_STATE --dry-run
rsi --config-home NEW_CONFIG --state-home NEW_STATE import --config-home OLD_CONFIG --state-home OLD_STATE
rsi --config-home NEW_CONFIG --state-home NEW_STATE import recover
```

The dry run creates no directories or locks and leaves permissions unchanged.
It previews the import; the actual operation repeats checks under exclusive leases.
Import stages owned configuration and state, retains destination backups and
does not rewrite external paths or translate database schemas. Reinstall optional
packages separately. An unfinished import blocks ordinary startup until recovery.
The [launch contract](../../crates/rsi/core/docs/launch.md) defines the admitted
paths, bounds and recovery behavior.
