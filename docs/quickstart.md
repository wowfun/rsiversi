# Developer quick start

To install and use the application with your own model provider, follow the
[RSIversi user tutorial](user/first-conversation.md)
([中文](user/first-conversation.zh.md)). This page is for
contributors using isolated development environments.

Run from the repository root with the Rust toolchain in `rust-toolchain.toml`.
On Linux or WSL, this builds the real product and starts an isolated TUI with a
local test provider. No API key is needed:

```bash
cargo run --locked -p rsi-app-tools -- dev tui
```

The command prints its private development directory. Exit the TUI to stop its
source watcher. To retain the environment after a successful run, pass
`--directory /absolute/new/directory`; `--prepare-only` also retains it.
For a keyless provider check without entering a terminal, use
`cargo run --locked -p rsi-app-tools -- dev tui --smoke`. The [launcher contract](../apps/devtools/README.md)
owns environment isolation, cache reuse and cleanup behavior.

For Web, install the WASM target, Node.js 22 or later, and the matching binding
generator once:

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.127 --locked
npm install --global pnpm@12.6.0
pnpm -C apps/web dev
```

Open the printed local origin. In another terminal, use the printed `run` command
to register a development device, then paste its receipt into the sign-in form.
Select **Allow local HTTP for development** before connecting to the local origin.
Stop the development command with Ctrl+C. `RSI_WASM_BINDGEN` can select an existing
matching executable. `--prepare-only` builds the isolated environment without
launching it; `--no-watch` disables source watching.

## Compile only the frontend you are editing

Cargo builds the selected package and its dependencies, rather than every
workspace member:

```bash
# Pure TUI layout, editor and frame logic.
cargo check --locked -p rsi-terminal-ui
cargo test --locked -p rsi-terminal-ui

# Independent native TUI renderer; no standard-product rebuild.
cargo build --locked --manifest-path crates/rsi/terminal-native/Cargo.toml

# Resident terminal controller and terminal ownership.
cargo check --locked -p rsi-terminal

# Rust Web Worker and its WASM dependencies.
cargo build --locked -p rsi-web --target wasm32-unknown-unknown
```

For the Linux desktop, install the GTK 3 and WebKitGTK 4.1 development libraries
(`libgtk-3-dev` and `libwebkit2gtk-4.1-dev` on Ubuntu), use the Web build tools
above, and run from a graphical session:

```bash
cargo run --locked -p rsi-app-tools -- dist desktop /absolute/new/output --debug
/absolute/new/output/bundle/rsi-desktop
```

On native Windows, use `scripts/rsiversi.ps1 setup desktop`,
`scripts/rsiversi.ps1 build desktop --debug`, then `scripts/rsiversi.ps1 desktop`.
MSVC/Windows SDK are build prerequisites; WebView2 is checked at launch.
For a managed minimal native package, use `scripts/rsiversi.sh build tui`
(or PowerShell equivalent). See [source installation](user/source-install.md)
for target dependencies, release defaults and same-bundle client selection.

The [desktop contract](../apps/desktop/README.md) owns persistent state,
Host selection and paired-companion requirements.

`rsi-app-tools dev tui` watches the native renderer's explicit source inputs and replaces
its presentation through the ordinary addon catalog. `pnpm dev` uses Vite
React Refresh for feature components; bootstrap edits reload the document.
Independent renderers publish a complete asset graph through their own watcher.
Editing JavaScript requires no Rust build. Worker Rust changes need a new Web
bundle and application restart; see the
[Web build contract](../apps/web/README.md).

The first development launch still builds `rsi` and all of its linked backends.
Application Profile selection changes runtime composition, not the executable's
Cargo dependency graph. For the simplest manual product build:

```bash
cargo build --locked -p rsi-cli
target/debug/rsi --help
```

The terminal package owns the detailed [development tutorial](../apps/terminal/docs/tui-development.md)
and [debugging reference](../apps/terminal/docs/tui-debugging.md).
