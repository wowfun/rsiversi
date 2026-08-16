# rsi-meta-cli

`rsi-meta-cli` owns the command-line program and the v0 control-wire adapter around `CompositionHost`. The embedded core does not expose control envelopes, protocol constants, or daemon exit codes.

## Commands and workspace

`validate` and `lock` call `CompositionProject` directly and do not open a host. `install` performs an offline workspace transaction. `daemon serve` always uses `composition.toml` and `rsi-meta.lock` inside the selected state directory; it no longer accepts arbitrary installed paths. `apply`, `graph`, `events`, `plugin inspect`, `token rotate`, and `daemon stop` use the live daemon.

`apply`, `install`, `token rotate`, and `daemon stop` accept `--operation-id`. When omitted, the CLI generates a UUIDv7. Every mutation writes `operation_id=...` to stderr and flushes it before transport or filesystem work, then repeats the ID in the structured stdout result. A process-fixed `apply` prints `restart_required` and exits 75 while leaving the daemon running. Operators must then run `daemon stop`, wait for exit, run `install`, and start a fresh `daemon serve`; the CLI does not combine those authority-changing steps.

## Transport behavior

Unix sockets and loopback HTTP/WebSocket project the CLI-owned control protocol. Read command IDs correlate work only within the current connection and may be reused after completion; a concurrently duplicated ID is rejected. Mutation IDs are durable `OperationId` values. `expected_graph_revision` is legal only for apply. Events carry an optional `operation_id` rather than a required command ID.

An explicit `--socket` is sufficient for client-only commands and does not require a default state home; serving and offline installation still need the state directory they own. The daemon validates filesystem, peer credential, bearer credential, and wire input before passing typed values in-process. It opens durable state before admission, monitors independent host termination, and drains transports after an explicit shutdown. Exact authentication belongs to [security.md](../docs/security.md); retry, event, and stream semantics belong to the [protocol reference](../docs/subsystems/protocols.md).
