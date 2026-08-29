# rsi-commands

`rsi-commands` owns an explicit process-local command registry. The
[`rsi-commands-protocol`](protocol/README.md) package defines command DTOs and
handler/runtime seams; the ordinary [`rsi-commands`](core/README.md) plugin owns
exact-name registration and dispatch.

Commands are not chat syntax. A caller must invoke the command capability
explicitly; `/text` submitted to an Agent remains ordinary user text.
