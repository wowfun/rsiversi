# rsi-agent-tools

`rsi-agent-tools` is the thin model-facing adapter for the Agent control plane.
It registers `spawn_agent`, `send_message`, `followup_task`, `wait_agent`,
`interrupt_agent`, and `list_agents`. Lifecycle, lineage authorization,
durability, fork selection, and scheduling remain owned by the Turn service.

Every operation requires an unforgeable caller authority injected by the
executor through the generic Tool execution-extension seam. Model arguments
never carry or select that authority.

The separate `QuestionToolsFactory` contributes root-only `ask_user`. It
validates a bounded question batch, parks through the Kernel's human-wait seam,
and invokes the Host-generation User Questions contract until answer or
cancellation. It requires exclusive-final scheduling, so no Tool siblings
remain active when its executor and tree admission are released. Child Agents
receive a model-visible error and must communicate their question to the root.
Malformed question arguments are invalid input; broker capacity and identity
conflicts are execution failures. A delivered live answer does not authorize
Tool-result publication before the Kernel confirms durable wait resumption.

`send_message` has a timing-independent next-Step horizon: it is injected at a
running target's next safe boundary and remains held if the target is idle.
`followup_task` always queues a waking next Turn, including when another Turn is
currently running. The caller's observation timing therefore cannot change a
durably accepted message from steering into starting a new activation.
