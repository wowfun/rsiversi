# rsi

RSI composes the Agent and capability products into terminal, line, Headless,
Web and Linux/Windows Desktop applications. Its [architecture](docs/architecture.md)
defines product ownership and the shared Service Host.

For ordinary use, follow [your first conversation](docs/user/first-conversation.md)
([中文](docs/user/first-conversation.zh.md)).

Windows native Agent/ACP workspaces and private state require fixed local NTFS;
ReFS Dev Drives, exFAT and network roots are refused by the current filesystem
backend. Windows Web, Desktop, TUI and ACP own an embedded Service with one owner per Host;
stop it before another application or the offline `devices` manager uses that Host.
Windows SQLite storage requires
private parent directories, including pre-existing ones. See the owning
[filesystem contract](../rsi-files/native-fs/README.md) and
[Serve contract](../../apps/serve/README.md) for platform boundaries.

## Applications

`rsi tui` (equivalently `rsi --profile tui`) selects the fullscreen text
application. A custom Application Profile uses `rsi --profile NAME`. The launcher
also accepts the explicit [Agent Store reset option](core/README.md), for example
`rsi tui --reset-state`, before forwarding application arguments.
The [terminal package](../../apps/terminal/README.md) owns startup,
Home, setup, input, controller lifetime and terminal cleanup. Its
[interaction design](../../apps/terminal/docs/tui-design.md) owns terminal layout,
transcript presentation, editing, model/effort selection and Session navigation.
The [pure presentation library](terminal-ui/README.md) owns validated scenes,
cells, geometry and acknowledged source maps.

The [Line and Headless reference](../../apps/terminal/docs/line-application.md) owns their
input grammar, cancellation and exit behavior. The [terminal package](../../apps/terminal/README.md)
owns native input and cleanup, while [GUI](gui/README.md) owns shared graphical
controllers and [Desktop](../../apps/desktop/README.md) provides the Linux and Windows native adapter.

## Local Web application

The [Serve contract](../../apps/serve/README.md#local-web-launch) owns local Web
selection, options, authentication and Host ownership. Remote deployments use
Serve or an explicit Application Profile.
