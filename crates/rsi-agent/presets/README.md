# rsi-agent-presets

This package contributes immutable Agent Profile fragments. It contains no
Runtime, catalog, factory wrapper, or lifecycle adapter. The owning product
registers the ordinary Store, Kernel, and executor factories and chooses which
fragment to include.

The Headless fragment binds one absolute Store root, then starts Kernel and one
executor generation. Store authority is explicit and cannot be inferred from
the process environment or current directory.
