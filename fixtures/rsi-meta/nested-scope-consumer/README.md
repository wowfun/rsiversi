# rsi-meta-fixture-nested-scope-consumer

`rsi-meta-fixture-nested-scope-consumer` is a native consumer fixture for scoped provider resolution and explicit instance binding. It provides `fixture.nested-consumer`, requires `fixture.echo` and `runtime.tick`, and forwards configured request content to the resolved provider.

The [echo example](../../../examples/rsi-meta/echo/README.md) and conformance suite use the fixture to distinguish nearest-ancestor resolution, sibling exclusion, explicit binding, and generation-pinned streams.
