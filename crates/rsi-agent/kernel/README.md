# rsi-agent-kernel

Durable turn scheduler and write-behind ordinary plugin. The Kernel is the sole
owner of live session state, Fact sequencing, cancellation classification,
executor claims, 200 ms batching, flush retry, and startup interruption repair.
It hard-requires the mechanical `rsi.agent.store` Local contract and publishes
the application and executor Turn contracts atomically.

The live scheduler is a bounded working set, not a mirror of durable history.
Recovery streams lexical session/Fact pages, retains only nonterminal control,
repairs it, and releases the idle session. Runtime terminal commits prune turn
control and evict idle sessions; historical queries page through the Store on
demand. The periodic worker rebases its next 200 ms deadline after every scan;
slow Store I/O never causes back-to-back catch-up ticks. Permanent flush
failure is sticky on the session and terminates both explicit durability waits
and attached observations with `TurnError::Flush`; later submissions to that
session receive the same failure. Recovery terminalizes durably cancelled work
as `Cancelled` and all other unfinished work as `Interrupted`. Effect start
Facts require their matching intent to have crossed the durable watermark.
