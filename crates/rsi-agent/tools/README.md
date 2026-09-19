# rsi-agent-tools

`rsi-agent-tools` is the thin model-facing adapter for the Agent control plane.
It registers `spawn_agent`, `send_message`, `followup_task`, `wait_agent`,
`interrupt_agent`, and `list_agents`. Lifecycle, lineage authorization,
durability, fork selection, and scheduling remain owned by the Turn service.

Configuration is null or `{ "roles": { "reviewer": { "persona": "...",
"allow": ["read_file"], "deny": [] } } }`, with at most 32 named roles.
`spawn_agent.role` selects only a configured role. Persona text is bounded to
32 KiB; allow/deny sets each contain at most 64 exact Tool names. Omitted allow
keeps the current catalog, while an empty allow keeps no ordinary Tools. Unknown
configured Tool names fail spawn. These restrictions do not grant permissions:
the canonical workspace, Sandbox and approval policy retain parent inheritance.

The child Header freezes the role identity, persona, normalized configuration
digest and effective Tool-name set. Descendants intersect ancestor restrictions;
cold resume intersects the current catalog with the frozen set. Discovery and
prepare use the same claim-scoped view without changing the shared catalog.
Hidden or unknown calls retain the existing `tool.not_found` Turn failure before
ToolIntent, ToolStarted or I/O. Ordinary ToolPolicy denials keep ToolRejected and
`policy.denied` semantics.

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

`read_agent_result` takes the exact child/activation/Turn/Fact coordinates from a
successful Completion. The Kernel authenticates the immediate parent, verifies
that exact durable Completion, and reads and validates that single result Fact.
It never selects a latest result. The full output schema is programmatic spawn
input; ordinary model-facing spawn/follow-up arguments cannot supply one.

`read_agent_result` registration is opt-in with `read_structured_results: true`
in this contribution's trusted configuration. Enable it only when the embedding
application supplies initial output contracts through programmatic spawn. The
standard product leaves it disabled: model-facing spawn cannot select a schema.
