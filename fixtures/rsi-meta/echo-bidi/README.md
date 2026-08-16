# rsi-meta-fixture-echo-bidi

`rsi-meta-fixture-echo-bidi` is a real native provider used to observe routing, bidirectional streams, lifecycle, and backpressure through the public ABI. It provides `fixture.echo`, injects `runtime.tick`, and echoes service DATA under the product sequence, credit, half-close, and terminal rules.

The [echo example](../../../examples/rsi-meta/echo/README.md), core integration tests, and daemon conformance load this fixture as a real `cdylib`.
