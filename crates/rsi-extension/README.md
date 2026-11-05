# rsi-extension

This family owns the bounded bidirectional protocol for an explicitly selected
trusted process. Its core consumes Sandbox and Process and has no Agent or
language-specific semantics. The [core contract](core/README.md) defines lifecycle,
correlation and cancellation. Product adapters retain their own authority and
durable commit semantics; receiving a reply never establishes a durable receipt.
The optional [Agent adapter](agent/README.md) stages typed journal proposals and
retains immutable CAS objects through the separate canonical publication boundary.
