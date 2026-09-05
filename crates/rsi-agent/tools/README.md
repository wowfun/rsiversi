# rsi-agent-tools

`rsi-agent-tools` is the thin model-facing adapter for the Agent control plane.
It registers `spawn_agent`, `send_message`, `followup_task`, `wait_agent`,
`interrupt_agent`, and `list_agents`. Lifecycle, lineage authorization,
durability, fork selection, and scheduling remain owned by the Turn service.

Every operation requires an unforgeable caller authority injected by the
executor through the generic Tool execution-extension seam. Model arguments
never carry or select that authority.

`send_message` has a timing-independent next-Step horizon: it is injected at a
running target's next safe boundary and remains held if the target is idle.
`followup_task` always queues a waking next Turn, including when another Turn is
currently running. The caller's observation timing therefore cannot change a
durably accepted message from steering into starting a new activation.
