# rsi-agent-time-context

An ordinary Agent-only context contributor. It samples the host clock once before
each new model retry series and proposes one UTC RFC 3339 timestamp. The executor
assigns `rsi.time-context` provenance and commits the actual text before provider
I/O; provider retries and history replay reuse that committed text.

The plugin registers through the unpublished contribution registrar, retains its
exact registration lease, and has no Kernel, Store, UI or mutable global registry
dependency. Configuration is null. A constructor-supplied clock supports
deterministic embedding; the default uses the system clock. Out-of-range clocks
fail the contribution before persistence.
