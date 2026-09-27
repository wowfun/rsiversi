# Program runs

The opt-in [Program runtime](../../program/README.md) owns Node execution through
Process duplex and Jobs. Kernel owns the separate durable ProgramRun lifecycle;
Jobs remain process-local. [Execution ownership](../../kernel/README.md) is distinct
from immutable fork lineage, so a detached run retains a frozen composition and
policy without retaining a retired Tool claim. Its initial child completions
settle into run receipts. A bounded generation-bound notice publishes only the
curated completion back to the Session. Startup interrupts unfinished runs and
discards their unclaimed work and old notices without replaying external effects.
