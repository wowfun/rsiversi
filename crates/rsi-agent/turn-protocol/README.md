# rsi-agent-turn-protocol

Process-local application and executor contracts for Agent turns. Application
callers submit Language or direct Image turns, cancel, observe, inspect
outcomes, and read immutable session headers. Executors register and claim work, publish ordered Facts, and wait for
explicit durable watermarks before external I/O. Dropping an observation is
detach; there is no fork operation.

Nonterminal Facts are live-first and carry the durable watermark that existed
at publication. The sole terminal Fact and `outcome` become visible only after
the terminal Fact's complete prefix is durable. A permanent flush failure ends
an attached observation with `TurnError::Flush`; observation cannot wait
forever for a terminal that the Store can no longer commit.

The Kernel-owned finalization registry snapshots effect-owned hooks in
registration order. The executor runs them before publishing the sole terminal
Fact under its own validated deadline. Timeout cancels that wait and becomes
the turn's durable finalization failure; individual finalizers must separately
define whether their owned work is cooperatively cancellable or joinable.
