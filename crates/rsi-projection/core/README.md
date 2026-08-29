# rsi-projection

This package provides the Projection registry contract and ordinary plugin.
Exact-name units are safe-Rust pure functions. The registry snapshots units
before calling them and returns an ordered map of derived JSON values.
Registration count and the complete encoded output map have explicit bounds;
per-unit validation cannot be multiplied into an unbounded aggregate.
