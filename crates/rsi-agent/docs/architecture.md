# rsi-agent architecture

Only the agent/tool protocol is currently active. Durable session ownership,
replay, model execution, and tool orchestration require a new product boundary
over the Context/Fiber foundation. There is no parked runtime or plugin source
in the working tree; future implementation must follow the active protocol and
foundation instead of reviving the never-integrated v1 development line.
