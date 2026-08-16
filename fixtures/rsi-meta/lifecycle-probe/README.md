# rsi-meta-fixture-lifecycle-probe

`rsi-meta-fixture-lifecycle-probe` exposes controllable prepare, failure, retirement, state, and restart behavior to black-box lifecycle tests. It provides `fixture.lifecycle-probe`, requires `state.cas` and `runtime.tick`, and may inject `fixture.echo`.

[`config.schema.json`](config.schema.json) defines prepare failure, retirement acknowledgement mode, and the diagnostic tag. Core lifecycle tests and the [release demonstration](../release-demo/README.md) use the fixture to observe prepare terminals, generation lease drain, plugin state, retirement, and crash/restart boundaries through public surfaces.
