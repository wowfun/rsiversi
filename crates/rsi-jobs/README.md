# rsi-jobs

`rsi-jobs` owns the process-local control plane for background work created by
named producer generations. The [core contract](core/README.md) defines opaque
scope authority, producer retirement, admission, offset output reads, terminal
reporting, and finalization. The [local provider](local/README.md) implements
those contracts on the caller-owned Tokio runtime. The
[model-facing tools](tools/README.md) project generic list, output, and kill
operations through one unpublished Tool catalog registrar without depending on
any concrete work producer.

A Jobs provider never executes an arbitrary caller closure. A registered
producer validates and starts one typed process-local request, then transfers
the resulting control object to Jobs before an identifier is published. This
keeps process spawning and confinement with their owning producers while Jobs
owns discovery, cancellation, reporting, retention, and lifecycle.

Jobs are intentionally not durable. Identifiers, scopes, output cursors, and
tombstones are valid only for the current process and provider generation. Agent
turns use Jobs during pre-terminal finalization; durable scheduling remains an
Agent concern.
