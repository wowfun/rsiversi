# rsi

RSIversi composes the Agent and capability products into terminal, line, Headless,
Web and Linux/Windows Desktop applications. Its [architecture](docs/architecture.md)
defines product ownership and the shared Service Host.

User-facing product names use **RSIversi**, including application titles, navigation
labels and terminal headings. Technical identities such as `rsi` commands, package
names, protocol identifiers and data directories retain their own spelling.
The [Web document](../../apps/web/README.md) owns the shared graphical brand source.

For ordinary use, follow [your first conversation](../../docs/user/first-conversation.md)
([简体中文](../../docs/i18n/zh-CN/user/first-conversation.md)).

Windows native Agent/ACP workspaces and private state require fixed local NTFS;
ReFS Dev Drives, exFAT and network roots are refused by the current filesystem
backend. Ordinary Windows and Linux applications attach to the same resident
Service Host. Closing an attachment leaves the Host and other attachments alive.
Windows SQLite storage requires
private parent directories, including pre-existing ones. See the owning
[filesystem contract](../rsi-files/native-fs/README.md) and
[Serve contract](../../apps/serve/README.md) for platform boundaries.

## Applications

`rsi tui` selects the fullscreen text application. A custom Application Profile
uses `rsi run NAME`. Global home, watch and reset options precede the application;
the [launch contract](core/docs/launch.md) defines their shared behavior.
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

## Installation maintenance

The [installation library](installation/README.md) owns native installation records,
PATH receipts, upgrade transactions and deletion authority. The independent
[uninstaller](../../apps/uninstall/README.md) uses it without product composition;
[devtools](../../apps/devtools/README.md) owns bootstrap scripts and package delivery.
