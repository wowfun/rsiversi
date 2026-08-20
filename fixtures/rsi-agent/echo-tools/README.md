# rsi-agent-fixture-echo-tools

This native provider is the side-effect-free tool witness for `rsi-agent` conformance. Through the public `rsi.agent.tools` protocol it publishes one closed-schema `echo` definition and returns the canonical JSON object containing its validated `text` argument.

One plugin lifetime accepts exactly two tools service streams, one for each concurrent session. Each stream must capture its catalog before invocation, and only the echo session invokes the tool. A separate fixture-private observer reports open attempts, accepted opens, DATA frames, and maximum concurrent streams without changing those counters. A third service open, third catalog request, or second invocation during durable replay therefore fails the assembled scenario and remains observable.

Its behavior test drives lifecycle, stream credit, catalog and invocation DATA, half-close, and terminal handling through the public plugin ABI. The assembled [conformance runner](../conformance/README.md) loads the built `cdylib` through `rsi-meta`.
