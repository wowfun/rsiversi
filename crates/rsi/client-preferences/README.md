# rsi-client-preferences

An ordinary Settings contribution owns the closed `rsi.client` namespace with
`web.enter_submit` (default false). Reconnect Web to apply edits. The standard
Host registers `rsi.client.preferences`; persistence uses Settings version CAS.
Missing registration uses defaults; malformed values and read failures remain
errors. TUI input behavior is fixed by its [owner](../terminal/README.md).
Web always supports Ctrl/Command+Enter and Shift+Enter, and suppresses submission
during composition. Questions and forms retain their own input semantics.
