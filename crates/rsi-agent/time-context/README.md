# rsi-agent-time-context

An ordinary Agent-only BeforeStep contributor. It samples the host clock before
each new model retry series and proposes one UTC RFC 3339 timestamp when no prior
input remains in the retained Turn projection, the refresh interval has elapsed,
or the clock has moved backwards. The executor assigns `rsi.time-context`
provenance and commits the actual text before provider I/O; provider retries and
history replay reuse that committed text.

The plugin registers through the unpublished contribution registrar, retains its
exact registration lease, and has no Kernel, Store, UI or mutable global registry
dependency. Null or empty configuration selects `refresh_interval_ms: 600000`;
zero emits on every eligible preparation. The previous input time comes from
the existing Context fold, including selected fork history and checkpoints.
Only inputs actually entered by `StepInputsPrepared` advance it. It uses the
input Fact's commit timestamp, not a second durable copy of the sampled clock.
Discarding its entire retained Turn permits a fresh sample; summarizing part of
that Turn preserves the interval. No callback-local timer or domain state needs
reconciliation after a failed publication or restart.

A constructor-supplied clock supports deterministic embedding; the default uses
the system clock. Out-of-range clocks fail the contribution before persistence.
