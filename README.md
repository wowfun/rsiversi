<p align="center">
  <img src="apps/web/public/logo.svg" alt="RSIversi" width="160">
</p>

English | [简体中文](README.zh-CN.md)

# RSIversi

RSIversi is an extensible agentic harness built in Rust, with an
everything-is-a-plugin architecture.

Bring a model provider, work with an agent in your project, and adapt its tools,
context and runtime composition to the task. Use the shipped applications or
compose your own product from independently usable Rust components.

RSIversi is **pre-release**. APIs, plugin contracts and saved formats can change
without compatibility guarantees.

## Agentic work in your workspace

- Work through tasks with model calls and tools for file access, editing and
  command execution, subject to the selected permissions and platform support.
- Give agents project instructions and reusable Skills, and delegate work to
  subagents with their own definitions and tool selections.
- Keep durable conversation history and resume saved sessions. Model context is
  built from recorded session facts.

Choose the [Web interface](apps/serve/README.md#local-web-launch),
[fullscreen TUI](apps/terminal/README.md), or
[Linux/Windows Desktop application](apps/desktop/README.md). For scripted use or
integration with another client, use [Headless](apps/terminal/docs/line-application.md)
or [ACP](apps/acp/README.md).

Optional [Workflows](examples/rsi/workflow/README.md) coordinate agent tasks
through JavaScript. They require explicitly enabling the Node runtime in the
Host Profile and selecting the `workflow` Agent preset.

## Everything is a plugin

Model providers, the Agent Kernel, context builders, session stores and runtime
application components are ordinary `rsi-meta` plugins. Profiles compose these
implementations, and Agent presets select the contributions available to an
agent. Custom implementations use the same public contracts as shipped ones.

The plugin is the unit of runtime lifecycle and composition. Tools, commands and
prompt fragments are contributions registered by plugins; protocol definitions
and pure logic remain libraries. This keeps the runtime foundation independent
of model providers, Agent semantics and application policy. See the
[composition foundation](crates/rsi-meta/README.md) for the owning contracts.

## Get started

Build from a Git checkout. Install Git, Rustup and the
platform compiler prerequisites described in
[Install from source](docs/user/source-install.md).

<!-- rsi-example id=install-clone platform=all -->
```bash
git clone https://github.com/wowfun/rsiversi.git
cd rsiversi
```

On Windows 11 24H2 or newer, x64, keep the checkout on fixed local NTFS.
Use PowerShell:

<!-- rsi-example id=install-windows platform=windows -->
```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\install.ps1
```

Or Git Bash on Windows:

<!-- rsi-example id=install-git-bash platform=git-bash -->
```bash
bash scripts/install.sh
```

On Linux or WSL:

<!-- rsi-example id=install-bash platform=linux -->
```bash
bash scripts/install.sh
```

Restart your terminal application and run `rsi` for TUI. Add Web with `rsi plugin install web`,
then run `rsi web`. For local source packages, follow the source-install guide.
The browser launcher returns after opening its one-time sign-in link; the Host
remains available to other interfaces.

Use `rsi --help` for the command overview and `rsi APPLICATION --help` for detailed
options. Installation also saves an independent uninstall launcher; ordinary
uninstall preserves data. See [uninstall and purge](docs/user/source-install.md#uninstall-or-clear-runtime-data)
before clearing homes shared by multiple installations.

In **Settings → Models & access**, save a credential, configure its provider,
then select a default model. Create a conversation and choose a working
directory. [Your first conversation](docs/user/first-conversation.md) walks
through setup and returning to saved conversations.

Windows currently supports conversations, model setup, saved history and native
file access. Restricted shell execution and Browser/Automation workflows require
Linux/WSL. Windows and Linux interfaces share one resident Host.
See [platform limits and troubleshooting](docs/user/troubleshooting.md) for details.

## Extend RSIversi

- Add project instructions and [Skills](crates/rsi-agent/workspace-context/README.md)
  to teach reusable procedures without writing a runtime plugin.
- Connect external tools and resources through the opt-in
  [MCP integration](crates/rsi-mcp/README.md).
- Customize agent contributions with [Agent presets](crates/rsi-agent/presets/README.md),
  and compose services and applications through
  [Host and Application Profiles](crates/rsi/docs/subsystems/composition-and-profiles.md).
- Write Rust plugins as linked source components or native Tool addons. The
  [addon generator](crates/tools/rsi-xtask/README.md) scaffolds independent
  workspaces for either approach on Linux/WSL. Linked components take effect
  when rebuilt; native addons use the explicit Native ABI and Portable contracts.

For a custom Rust application, the [Host SDK](crates/rsi-host/README.md) lets you
assemble an explicit plugin catalog and start a Profile with your selected
components.

## Development

Follow the [development guide](docs/development.md) to run the standard debug
product on Windows or Linux/WSL, with Web hot updates. Explicit keyless fixture
tools are available on Linux/WSL. Build-only and Desktop workflows are covered in
[Install from source](docs/user/source-install.md).

The [repository architecture](docs/architecture.md) defines ownership and
dependency direction. Each product keeps its public contracts, setup, security
and verification documentation in its own subtree.
