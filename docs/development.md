# Development guide

Use the ordinary `rsi` executable and application commands. Debug/release and
`--watch` do not select different homes or a different runtime. From a checkout:

```bash
cargo build --locked -p rsi-cli
target/debug/rsi --help
```

Windows uses `target/debug/rsi.exe`. Build prerequisites and independent optional
packages are described in [source installation](user/source-install.md).
The installer is only needed when you want `rsi` on PATH.

## Choose homes

All three options select final directories relative to the caller's cwd. Omitted
options retain the normal platform defaults. For an isolated development instance:

```bash
target/debug/rsi --config-home .local/dev/config --state-home .local/dev/state --cache-home .local/dev/cache config init
target/debug/rsi --config-home .local/dev/config --state-home .local/dev/state --cache-home .local/dev/cache tui
```

Configure providers normally, or supply an environment credential to the caller
that starts the Host. The Host freezes fallback environment at startup; persistent
Credentials can be changed through Settings. Later callers get a categorical
warning if their environment differs. Use the same selected homes on management
commands, including `host stop`, `config show` and package installation.

`--reset-config` and `--reset-state` can be combined. They archive owned files and
preserve credentials, installed packages and unrelated files. Stop an active Host
first. See the [launch/reset contract](../crates/rsi/core/docs/launch.md).

## Observe configuration and source

```bash
target/debug/rsi --config-home .local/dev/config --state-home .local/dev/state --cache-home .local/dev/cache --watch tui
```

`--watch` holds configuration and source-build leases while the application is
attached. The last lease stops observation and cancels active builds. Ordinary
runs do not poll files. Explicit settings/profile management still works without
watching. Static core/TUI changes report that a process restart is required.
Valid `rsi.toml` application/source changes replace the current attachment and
source leases; malformed edits retain the current configuration. Service selection
changes require an explicit Host restart. See the [launch contract](../crates/rsi/core/docs/launch.md).

For Web, install the frontend prerequisites from the source-install guide and
add an explicit trusted recipe to `.local/dev/config/rsi.toml`. This Windows
example uses a checkout at `D:/Projects/rsiversi`; replace both absolute paths
for your checkout (Linux uses `/...` paths):

```toml
[sources.web]
directory = 'D:/Projects/rsiversi'
command = ['node', 'apps/devtools/build.mjs', 'web', 'D:/Projects/rsiversi/.local/packages/watch-web', '--debug', '--replace']
output = '.local/packages/watch-web/package.json'
inputs = ['apps/web/src', 'apps/web/worker.js', 'apps/web/admission.js', 'apps/web-worker/src']

[sources.web.frontend]
command = ['node', 'apps/web/development-server.mjs']
port = 0
```

Inputs are explicit bounded files/directories, not a whole checkout containing
build output or `node_modules`. Add the source owners you are editing. A recipe
freezes command arguments, inputs and the scrubbed caller environment; changing
its authority requires restarting the client to acquire a new lease. Source
commands use Process/Sandbox providers. They publish through ordinary package
admission; a failed build preserves the installed good generation.

```bash
target/debug/rsi --config-home .local/dev/config --state-home .local/dev/state --cache-home .local/dev/cache --watch web --no-open
```

Web watch runs in the foreground. The owned Vite process reports its actual
Origin before proxy readiness and launch. Renderer changes retain the listener;
Worker/bootstrap changes require a new application attachment. Ctrl+C closes
this frontend and its listener, leaving other clients and the shared Host alive.
An installed Desktop package uses the same recipe with `--watch desktop`.

Without `--watch`, `rsi web` starts or reuses the persistent browser listener,
opens a fresh sign-in link and returns. `web status`, `web stop` and `host stop`
control their respective lifetimes. Building and adding a UI never requires a
second core build or an installation target switch.

## Validate the changed boundary

Use the owning package's tests and the repository test policy. For deterministic
fixture-only TUI checks on Linux/WSL:

```bash
cargo run --locked -p rsi-app-tools -- fixture-dev tui --smoke
```

Ordinary runtime and watch integration live in `apps/cli/tests/resident_listeners`.
On Windows, compile native tests with `--no-run --message-format=json` and execute
the reported artifacts outside Cargo's child Job so persistent Host breakaway
can be exercised. See the [CLI contract](../apps/cli/README.md).
