# rsi

RSI composes the Agent and capability products into terminal, line, Headless,
Web and Linux Desktop applications. Its [architecture](docs/architecture.md)
defines product ownership and the shared Service Host.

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
controllers and [Desktop](../../apps/desktop/README.md) provides the Linux native adapter.

## Local Web application

The [Serve contract](../../apps/serve/README.md#local-web-launch) owns local Web
selection, options, authentication and Host ownership. Remote deployments use
Serve or an explicit Application Profile.
