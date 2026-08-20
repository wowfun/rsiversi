# rsi-agent-fixture-capability-anchor

This native fixture contributes no product service. Its manifest declares all five `rsi.ai.*` injections and `rsi.agent.tools` so the external `AgentHost` can use this instance identity as the explicit `rsi-meta` routing authority. It also declares the fixture-private tools observer used only by assembled conformance.

Its behavior test covers only the lifecycle contract; AI and tool behavior remains owned by their provider fixtures.
