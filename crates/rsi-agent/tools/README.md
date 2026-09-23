# rsi-agent-tools

`rsi-agent-tools` is the thin model-facing adapter for the Agent control plane.
It registers `spawn_agent`, `send_message`, `followup_task`, `wait_agent`,
`interrupt_agent`, and `list_agents`. Lifecycle, lineage authorization,
durability, fork selection, and scheduling remain owned by the Turn service.

Configuration accepts `roles` for inline definitions and `markdown_agents: true`
to discover editable files. The standard preset enables Markdown definitions.
At most 32 inline roles and the workspace source's bounded file listing are
shown (at most 64 names combined); the workspace source detects inline-name
collisions in that same directory walk. Exact-name resolution also reads files outside
that listing. A file/inline collision remains unavailable even outside the file
listing. Persona text is bounded to
32 KiB; allow/deny sets each contain at most 64 exact Tool names. Omitted allow
keeps the current catalog, while an empty allow keeps no ordinary Tools. Unknown
configured Tool names fail spawn. These restrictions do not grant permissions:
the canonical workspace, Sandbox and approval policy retain parent inheritance.

Direct Human mentions are scanned incrementally at the captured Fact horizon.
A generation-local cache retains at most 32 Header/Turn cursors, each with at most
4,096 distinct names; exceeding that bound returns Capacity. A horizon below the
cached cursor, identity change or eviction rescans authoritative Facts. Cache hits
share the immutable name set; a changed set is copied outside the cache lock.
No execution authority or durable state is inferred from this cache.
Names are filtered against the freshly loaded catalog on every contribution.

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

## Markdown definitions

The [workspace definition source](../workspace-context/README.md#agent-definition-files)
owns file discovery, precedence and syntax. Inline/file name conflicts are errors. File edits affect the next fresh spawn, including in existing Sessions.
Each new provider request receives a current bounded catalog; transport retries
replay that request's entered catalog. `@name` in direct Human text asks the main
Agent to delegate and summarize; it does not create a child by itself.
Catalog availability means the definition is valid and readable; Tool-name
existence remains a spawn-admission check. Source Capacity and Closed preserve
their typed contribution errors and map to Capacity and ShuttingDown for spawn.
Only invalid or failed observations become catalog-unavailable context.

The trusted adapter passes a named resolver to the Turn service. Under child
admission the Kernel checks for an existing accepted spawn before resolving a
fresh definition. The child Header records the complete normalized seed and
original request digest. Exact retries and cold child resume use those saved
values, even after the file changes or disappears. Inline role retries retain
exact configuration equality. Explicit model overrides definition defaults;
definition defaults override the actual producing model request. Effort requires
a model, and explicitly switching models clears inherited effort.

Ordinary child replies follow the bounded public Completion contract in the
[Kernel](../kernel/README.md). Returning a final answer requires no extra Tool permission.
