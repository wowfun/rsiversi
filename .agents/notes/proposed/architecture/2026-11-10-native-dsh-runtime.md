---
name: Native Standard plugins with an optional DSH JavaScript host
---

## Problem

The Standard product currently composes native Agent services with a DSH
application hosted in another process. That application creates a Cordis tree,
maintains a live Session projection, resolves tools and prompt contributions,
and forwards native work through its carrier. Improving Meta in isolation
cannot eliminate that application-level round trip or duplicate live state.

DSH built-ins are useful behavioral references. Their synchronous service
access, caller-owned contributions, Agent-scoped hooks and async middleware
need native equivalents when ported. Merely retaining every original built-in
inside a JS host preserves the translation work that the user wants removed.

The present call chain is explicit in the companion's
`packages/rsi-dsh-plugin-bridge/src/application/entry.mjs::serveApplication`,
`src/application/application.mjs::createApplication` and
`src/tools/application-tools.mjs::applicationTools`.
The DSH reference's `packages/core/agent/src/dispatch.ts::agentEvents`
binds subject and scope, contains notification failures and delegates awaited
middleware. Meta's current [`Local events`](../../../../crates/rsi-meta/core/src/local_events.rs)
and [`scoped contributions`](../../../../crates/rsi-meta/scope/README.md) provide
the native comparison seams; they are not already equivalent to all of those
DSH behaviors.

## Proposal

Keep Meta a Rust runtime. Port the useful behavior of DSH built-ins into
ordinary native product plugins and replace the corresponding existing
implementation after behavioral comparison. JavaScript execution, package
loading and unchanged third-party DSH package support remain in `rsiversi-dsh`.
No JS engine, DSH event names, Node objects or product-specific Session state
enter Meta core.

Use one native `Runtime -> Context -> Fiber` graph for Standard composition.
Direct native object calls remain Local, without serialized messages or an
extra managed RPC driver. A passive tool, hook or prompt contribution remains
an owned contribution to its plugin rather than a separately scheduled Fiber.
Preserve explicit caller ownership when porting Cordis's traceable registration
behavior. A provider Context must not silently become the contribution owner.

Treat Core changes as responses to demonstrated native authoring needs. Start
with public tests for the actual ported call chain; move repeated scheduling,
dependency and exact-cleanup mechanics into Meta once that duplication exists.
The current synchronous Waterfall cannot by itself express DSH's asynchronous
around middleware. Before adding a new mode, exercise native tool execution and
prompt preparation with typed input/output, an awaited continuation and caller
cancellation. Prefer a consuming continuation over a dynamic duplicate-next
guard. Notification failure semantics belong to the event contract: DSH's
Agent notifications contain listener failures, while Meta's existing Local
callbacks deliberately use ordinary Rust panic propagation.

The companion's native adapter advertises actual JS contributions and their
generation. An Agent request pins the selected native and JS contributions once.
An empty JS contribution set must not open a JS Session mirror or prepare a JS
tool view. A purely native run can operate without Node. Preserve the real JS
hook position in mixed ordered or around pipelines; batching must not move
native callbacks across JS callbacks, duplicate side effects, or replay an
uncertain mutation.

Native Session facts, metadata, workspace membership and command outcomes have
one durable owner. The JS host may retain a demand-driven projection required
by an active plugin, with explicit invalidation and release. It does not gain a
second canonical transcript or persistent Workspace registry. Keep original
browser view preferences distinct from shared Host state.

## Alternatives considered

Embedding Node into Meta would contradict the selected Rust-native scope and
move the companion's responsibility into the foundation. Replacing Cordis with
a Rust wrapper while retaining all current process calls changes the scheduler
without removing the dominant translation path. A new universal JSON service
table loses typed native calls and introduces another authority map. Rewriting
all products before running one vertical slice makes correctness and resource
regressions difficult to attribute.

Retaining Cordis inside the optional JS compatibility host is acceptable during
native migration. Claiming that this also replaces Cordis for unchanged JS
packages would be inaccurate; that requires a separate companion-owned
compatibility implementation and acceptance.

## Acceptance criteria

1. Complete a source-backed native slice from a Standard user action through
   its plugin and durable outcome. Workspace Host ordering is the current
   slice; Session account ordering and Agent hook execution require separate
   behavioral evidence. Use original DSH behavior rather than inventing a
   second product policy.
2. For native Agent hooks, prove scope and caller ownership, registration order,
   cancellation, removal/replacement and awaited continuation behavior across
   real public Meta Context/Fiber tests. Then remove superseded native glue and
   the JS implementation of the built-in from the Standard path together.
3. Run the same native workload with no JS contributions and with one installed
   third-party JS hook. Instrument process requests and bytes at the carrier:
   the first has zero JS calls; the second pays for its actual contribution.
   Also verify asynchronous JS work started before removal drains correctly.
4. Compare startup, per-turn latency, allocation/retained state, process count
   and peak memory with the current Standard build and the original DSH path
   on the same workload. A code-size reduction or an empty registry is not a
   resource benchmark. Record actual limits for 512 MB–2 GB Linux devices.
5. Preserve the real market, DSH-better-sidebar, native Windows/WSL shell and
   session-search compatibility cases. Revalidate affected visual surfaces and
   live native behavior from the exact rebuilt artifacts. Exclude native macOS
   only as requested; report other unexercised targets explicitly.

## Risks

DSH plugins can read synchronous Session objects and create registration effects
through a caller-shadowed Service. Deferring projections or narrowing IPC must
not change these observable guarantees for remaining JS consumers. Some JS
plugins may need broad live projections, so optional does not mean free.
Cancelling a waiter does not cancel a durable commit. Cross-process recovery
receipts remain necessary while actual JS tools run out of process.

The research compares upstream Cordis with Meta; the DSH checkout imports its
own published `@deepseek-ai/cordis` packages. Verify the executed implementation
for each acceptance case instead of treating the research snapshot or a source
hash as a compatibility admission rule.
