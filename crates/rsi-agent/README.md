# rsi-agent

The active `rsi-agent` surface is [`rsi-agent-protocol`](protocol/README.md), the bounded semantic contract between an agent runtime and tool providers.

No agent runtime or coding-tools implementation is retained in the working
tree. The abandoned v1 development line depended on removed host surfaces and
was never integrated with `Runtime -> Context -> Fiber`; retaining uncompiled
source would create a competing contract without build evidence. A future
durable-turn milestone starts from the public foundation and active protocol.
Historical rationale remains in Agent Notes and Git history, not in parked
production-shaped source.
