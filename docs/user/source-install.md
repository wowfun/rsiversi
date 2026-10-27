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

The installer builds `rsi` and the independent `rsi-uninstall` in release mode and adds its `bin` directory to
your user PATH. Restart your terminal application after installation. No UI selection is needed.

On Windows, use PowerShell:

<!-- rsi-example id=install-windows platform=windows -->
```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\install.ps1
```

Or use Git Bash on Windows:

<!-- rsi-example id=install-git-bash platform=git-bash -->
```bash
bash scripts/install.sh
```

Both Windows commands use the same native installer and default to
`%LOCALAPPDATA%\RSIversi`. Restart your terminal application after installation
so a new Git Bash session inherits the updated Windows user PATH.

On Linux or WSL:

<!-- rsi-example id=install-bash platform=linux -->
```bash
bash scripts/install.sh
```

<!-- rsi-example id=install-launch platform=all -->
```bash
rsi
```

Use `--prefix DIR` / `-Prefix DIR` for another install root, or `--binary FILE` /
`-Binary FILE` together with `--uninstaller FILE` / `-Uninstaller FILE` for an
explicitly trusted prebuilt pair. Source installation builds both executables;
prebuilt installation does not invoke Cargo. The independent tool and platform
uninstall script are saved in `<prefix>/libexec/rsi`, outside PATH. The installer
checks the destination before building and refuses an existing file, directory
or link. The prefix and its ancestors must not traverse symlinks or junctions;
pass the resolved real directory explicitly. If `rsi` is already installed, it prints that installation's full path
and an upgrade command; follow [the upgrade steps below](#upgrade-and-transfer-existing-data)
or choose another empty prefix. `--no-path` / `-NoPath` leaves
PATH untouched for isolated verification. It does not open config or state.

## Uninstall or clear runtime data

Set `prefix` below to the actual installation directory before previewing removal.
Custom installations must pass their custom prefix:

<!-- rsi-example id=uninstall-preview-windows platform=windows -->
```powershell
$prefix = Join-Path $env:LOCALAPPDATA "RSIversi"
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/uninstall.ps1 -Prefix $prefix -DryRun
```

<!-- rsi-example id=uninstall-preview-linux platform=linux -->
```bash
prefix="$HOME/.local"
bash scripts/uninstall.sh --prefix "$prefix" --dry-run
```

Remove the preview flag to uninstall. After deleting this checkout, use the
corresponding script in `<prefix>/libexec/rsi`; it selects that prefix by default.
Git Bash delegates to PowerShell. Neither launcher runs the installed `rsi`,
requires Cargo nor downloads tools. Windows actual removal runs a verified temporary copy of the
uninstaller; dry-run runs the installed tool in place. Linux runs it directly.

Ordinary uninstall preserves the configuration, state and cache homes, including
shared package and Host runtime copies. `--purge` / `-Purge` removes saved
credentials, Sessions, generated state, WebView data and caches after displaying
the affected homes and asking for confirmation. Those homes may also serve other
installations and development builds. Noninteractive confirmation uses `--yes` /
`-Yes`. User-authored launch/settings documents, AGENTS.md, skills, agents,
Profiles, custom presets and addon sources remain, as do external references and
unknown files. See the [removal contract](../../crates/rsi/installation/README.md).

Use `--config-home`, `--state-home` and `--cache-home` (PowerShell `-ConfigHome`,
`-StateHome`, `-CacheHome`) to select final homes without loading configuration.
The eligible Host stops gracefully; embedded Hosts, busy packages and locked
clients must be closed before retrying. A partial failure retains the uninstall
tool and reports what remains. Empty coordination anchors intentionally remain.

Missing or corrupt installation records require an explicitly trusted external
`--uninstaller FILE` / `-Uninstaller FILE` and `--force` / `-Force`. Force authorizes
the displayed fixed installation files; it does not bypass locks, pending
recovery, path checks or stop failures. PATH is only undone from exact verified
receipts, and is retained when the shared bin serves other programs. Linked or
read-only POSIX startup files require manual PATH setup/removal.

Repeated ordinary removal is a no-op when installation files are absent. Later
purge requires an external prebuilt uninstaller from a trusted core package,
because the installed tool was removed. Extract `rsi-uninstall.exe` (Windows) or
`rsi-uninstall` (Linux) from that package to a directory outside the target prefix,
then pass the external-tool option with the same prefix and home options. The core
package build steps below explain how to prepare a package separately; uninstall
never builds or downloads a tool. A usable main executable is not required.

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

Desktop uses `node apps/devtools/build.mjs desktop ABSOLUTE_NEW_PACKAGE`, followed by `rsi plugin install --local`
for both directories. Windows needs WebView2 at launch; Linux builds also need
`libgtk-3-dev libwebkit2gtk-4.1-dev`. Desktop and Web update independently. Desktop
checks the bridge version of the actual loaded document before opening a window.
An SSH helper is built separately on Linux with `build.mjs ssh-helper`; prepare
the same-CPU musl target and compiler first, then install it through `rsi`.

Omit `--debug` for release packages. Build output directories are new by default;
`--replace` is for an explicitly selected source output. See the
[producer contract](../../apps/devtools/README.md) and [development guide](../development.md).

TUI and Web share the same resident Host on Windows and Linux. Closing a UI
detaches it. `rsi web stop` stops the browser listener, while `rsi host stop`
stops the shared service. Continue with [your first conversation](first-conversation.md).

## Upgrade and transfer existing data

The installer is for first installation only; running it again does not upgrade
the core. For a local source upgrade, prepare a core package with the existing
producer (Node.js 22.18 or newer is required for this producer, not the installer):

<!-- rsi-example id=upgrade-core-source platform=all -->
```bash
node apps/devtools/build.mjs core "$PWD/.local/packages/core" --replace
rsi core upgrade --local "$PWD/.local/packages/core"
```

For a custom prefix, invoke the installed executable at the full path printed by
the installer instead of `rsi` on PATH. Preserve any global home options used by
that installation. `--local` takes an absolute package directory containing
`package.json`, not `target/release/rsi` or `rsi.exe`. The
[producer contract](../../apps/devtools/README.md) also describes assembling an
already-built executable and uninstaller without another Cargo invocation.
Recorded installations upgrade the core, uninstaller, launcher and record as one
recoverable transaction and reject packages missing compatible uninstall tools.
Unrecorded legacy installations retain single-executable upgrade behavior; an
upgrade performed by old code does not silently create an installation record.

Manifest format 2 requires a new core executable. For an existing format-1 store,
install the new binary to a separate empty prefix first, stop the old Host and
applications, then use that new executable with the same home options to run
`plugin list`. Remove every old selection, including `core`, in one
`plugin uninstall NAME...` command. Run `plugin gc` after old leases close, then
install the new packages. Keep the configuration, state and trust records;
resetting them does not replace this migration. The old binary cannot install
format-2 packages.

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
