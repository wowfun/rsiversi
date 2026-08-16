# rsi-meta-frame-contract

`rsi-meta-frame-contract` is the standalone support crate shared by maintained native plugins and black-box fixtures. It models the private versioned host/plugin JSON frames without exposing private core DTOs or presenting them as a public alternative to the product schemas.

The crate covers lifecycle, service request and event, durable command, runtime tick, and state-operation frames. Decoding enforces protocol and version identity, kind-specific fields, lifecycle generations, nonempty identifiers, and prepare-failure payload bounds. The [plugin-frame schema](../../../schemas/rsi-meta/plugin-frame.schema.json) is authoritative for the accepted JSON representation.

The watcher plugins, HMR consumer, conformance fixtures, and [`rsi-meta-plugin-testkit`](../../../fixtures/rsi-meta/plugin-testkit/README.md) consume these types so their black-box behavior remains aligned at one protocol owner.
